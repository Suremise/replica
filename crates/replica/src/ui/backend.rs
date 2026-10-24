//! What the window needs from the rest of Replica, behind [`Backend`] so tests can give it
//! temp folders and fakes: the settings file, the tray (to run jobs and reload), Credential
//! Manager, the connected volumes, Task Scheduler, the run history, restic (snapshots, browsing,
//! restoring, test restores) and the file pickers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use replica_core::cancel::CancelToken;
use replica_core::history::{HistoryFilter, RunRecord, TestRestoreRecord};
use replica_core::i18n::tr;
use replica_core::job::{Job, Target};
use replica_core::platform::{Secret, SecretStore, TargetLocation, resolve_target};
use replica_core::repo_lock::{self, Wait};
use replica_core::restic::{
    Node, Repository, Restic, ResticError, RestoreRequest, RestoreSummary, Snapshot, local_path,
};
use replica_core::run::{Outcome, Waits};
use replica_core::settings::Settings;
use replica_core::tr;
use replica_win::keys::{self, Entered, NotSaved};
use replica_win::paths::AppPaths;
use replica_win::repo_locks::NamedMutexLocks;
use replica_win::scheduler::{ScheduledTask, Scheduler};
use replica_win::targets::{VolumeInfo, WindowsTargets, list_volumes};
use replica_win::window::DialogOwner;

use crate::tray::runner;

/// What the window asks of the tray, which runs the jobs and owns the schedules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRequest {
    /// The settings file changed: load it now (schedules, tasks and menus follow it).
    Reload,
    /// Run these jobs, one after another.
    Run(Vec<String>),
    /// Stop the running job and the queued ones.
    Stop,
}

/// What the tray tells the window: what's running and how each job last ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The running job and its progress (a percentage).
    pub running: Option<(String, u32)>,
    /// What the running job is waiting for ("Waiting for hive to finish with External HDD"),
    /// while it waits its turn for a repository another run is using.
    pub waiting: Option<String>,
    /// The jobs waiting their turn, in order.
    pub queued: Vec<String>,
    /// Each job's last run, by its name in lower case.
    pub last: HashMap<String, LastRun>,
    /// The jobs that are overdue, by their names in lower case: their last success (or
    /// the tray's start, if they never had one) is older than their schedule allows, even if
    /// they keep running and failing. The tray's rule ([`crate::tray::state::is_overdue`]).
    pub overdue: HashMap<String, Overdue>,
    /// When the tray started: what a schedule counts from for a job that never ran.
    pub since: Option<DateTime<Utc>>,
}

/// When an overdue job was due, and when it last succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overdue {
    pub due: DateTime<Utc>,
    pub last_success: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    pub at: DateTime<Utc>,
    pub outcome: Outcome,
    /// Why it didn't succeed: the run's own error, else the first target's reason.
    pub reason: Option<String>,
}

impl LastRun {
    /// The reason a run with `error` and these targets (name, reason) gives: its own error,
    /// else the first target with a reason, named.
    pub fn reason_of<'a>(
        error: Option<&str>,
        targets: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Option<String> {
        error.map(str::to_owned).or_else(|| {
            targets
                .into_iter()
                .find_map(|(target, reason)| reason.map(|r| format!("{target}: {r}")))
        })
    }
}

impl Status {
    pub fn last(&self, job: &str) -> Option<LastRun> {
        self.last.get(&job.to_lowercase()).cloned()
    }

    pub fn overdue(&self, job: &str) -> Option<&Overdue> {
        self.overdue.get(&job.to_lowercase())
    }

    pub fn is_queued(&self, job: &str) -> bool {
        self.queued.iter().any(|q| q.eq_ignore_ascii_case(job))
    }

    /// What the job is waiting for, if it's the one running and it's waiting its turn.
    pub fn waiting(&self, job: &str) -> Option<&str> {
        self.progress(job).and(self.waiting.as_deref())
    }

    /// The job's progress, if it's the one running.
    pub fn progress(&self, job: &str) -> Option<u32> {
        self.running
            .as_ref()
            .filter(|(r, _)| r.eq_ignore_ascii_case(job))
            .map(|(_, p)| *p)
    }
}

/// A versioned target, before it's set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetCheck {
    /// Not connected or not reachable now.
    NotAvailable(String),
    /// Reachable at `location`: whether a repository is there already, and whether its
    /// password is in the secret store.
    Ready {
        location: PathBuf,
        initialised: bool,
        has_password: bool,
    },
    /// It couldn't be checked (a login missing from the store, say).
    Failed(String),
}

pub trait Backend: Send + Sync {
    /// The settings file the window reads and writes.
    fn settings_file(&self) -> PathBuf;
    /// Whether the app's folder can be written to (a portable copy on a read-only drive can't).
    fn writable(&self) -> bool;
    /// Where the window keeps its own state.
    fn window_state_file(&self) -> PathBuf;
    fn send(&self, request: UiRequest);
    fn status(&self) -> Status;
    fn secrets(&self) -> &dyn SecretStore;
    fn volumes(&self) -> Result<Vec<VolumeInfo>, String>;
    /// Whether Replica has a task in Task Scheduler for `job`.
    fn has_task(&self, job: &str) -> bool;
    fn delete_task(&self, job: &str) -> Result<(), String>;
    /// Where a versioned target is and whether it's set up. It may take a while (a share is
    /// connected): call it off the UI thread.
    fn check_target(&self, target: &Target) -> TargetCheck;
    /// Creates the repository for `target` with `secret` (restic `init`, and the restore guide
    /// beside it). It takes a while: call it off the UI thread.
    fn init_repository(&self, target: &Target, secret: Secret) -> Result<(), String>;
    /// Saves a repository password the user typed for `target` (the copy they kept), once
    /// restic says it opens the repository there ([`keys::save_entered`]; `force` replaces one
    /// saved that opens it too). It runs restic: call it off the UI thread.
    fn save_entered_password(
        &self,
        target: &Target,
        secret: Secret,
        force: bool,
    ) -> Result<Entered, NotSaved>;
    /// The window the file dialogs below belong to (its `HWND`), once it exists: they open
    /// over it, and it can't be used while one is open.
    fn set_owner(&self, _hwnd: isize) {}
    fn pick_folder(&self) -> Option<PathBuf>;
    fn pick_file(&self) -> Option<PathBuf>;
    /// Where to export jobs to: a Save dialog for a jobs file, which asks before
    /// replacing one.
    fn save_jobs_file(&self) -> Option<PathBuf>;
    /// A jobs file to import.
    fn open_jobs_file(&self) -> Option<PathBuf>;

    /// The most recent runs `filter` lets through, newest first.
    fn runs(&self, filter: &HistoryFilter, limit: usize) -> Result<Vec<RunRecord>, String>;
    /// The most recent test restores `filter` lets through, newest first.
    fn test_restores(
        &self,
        filter: &HistoryFilter,
        limit: usize,
    ) -> Result<Vec<TestRestoreRecord>, String>;
    /// Every job name in the history, old ones included.
    fn history_jobs(&self) -> Result<Vec<String>, String>;

    /// Where Replica keeps its files, for the Settings tab.
    fn places(&self) -> Places;
    /// Every task in Replica's Task Scheduler folder, orphans flagged against `jobs`.
    fn tasks(&self, jobs: &[&str]) -> Result<Vec<ScheduledTask>, String>;
    fn set_task_enabled(&self, task: &str, enabled: bool) -> Result<(), String>;
    /// Deletes every task in Replica's folder; returns how many there were.
    fn delete_all_tasks(&self) -> Result<usize, String>;
    /// Turning the Task Scheduler option off: delete the tasks (the default) or turn them off.
    fn turn_off_tasks(&self, delete: bool) -> Result<usize, String>;
    /// Opens Windows' own Task Scheduler.
    fn open_task_scheduler(&self) -> Result<(), String>;
    /// Opens a web page in the default browser.
    fn open_url(&self, url: &str) -> Result<(), String>;
    /// The restic that would run with `override_path` (or the bundled one), and its version.
    /// It runs restic: call it off the UI thread.
    fn restic_in_use(&self, override_path: Option<PathBuf>) -> Result<(PathBuf, String), String>;
    /// Opens a file or folder as Explorer would (a log in its viewer, a folder in Explorer).
    fn open_path(&self, path: &Path) -> Result<(), String>;

    // The rest reach a target and run restic: call them off the UI thread.

    /// The job's snapshots on a versioned target, oldest first.
    fn snapshots(&self, job: &Job, target: &Target) -> Result<Vec<Snapshot>, String>;
    /// A folder's files and folders in a snapshot (`/C/Work`), the folder itself left out.
    fn list(&self, target: &Target, snapshot: &str, folder: &str) -> Result<Vec<Node>, String>;
    /// Restores items of one of the job's snapshots.
    fn restore(
        &self,
        job: &Job,
        target: &Target,
        restore: &RestoreJob,
        cancel: &CancelToken,
    ) -> Result<RestoreSummary, String>;
    /// Runs a test restore of the job's latest snapshot on `target`, records it in the
    /// history and returns it.
    fn test_restore(
        &self,
        job: &Job,
        target: &Target,
        cancel: &CancelToken,
    ) -> Result<TestRestoreRecord, String>;

    /// Pins the job's snapshot `id` on `target`, or unpins it, holding the repository's
    /// lock. restic gives a snapshot a new ID when its tags change: that's what comes back.
    fn set_pinned(
        &self,
        job: &Job,
        target: &Target,
        id: &str,
        pinned: bool,
    ) -> Result<String, String>;

    /// What a restore, test restore or set-up running now is waiting for, while another job
    /// is using its repository: "Waiting for hive to finish with External HDD".
    fn waiting(&self) -> Option<String> {
        None
    }
}

/// Where Replica keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// Portable (everything beside the exe) rather than installed.
    pub portable: bool,
    pub folder: PathBuf,
    pub settings: PathBuf,
    pub history: PathBuf,
    pub logs: PathBuf,
    pub run_logs: PathBuf,
    pub writable: bool,
}

/// What to restore, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreJob {
    /// The snapshot's ID.
    pub snapshot: String,
    /// Files and folders in it, as `ls` shows them (`/C/Work/a.txt`).
    pub items: Vec<String>,
    pub to: RestoreTo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreTo {
    /// A new or empty folder: each item as `<folder>/<its name>`.
    Folder(PathBuf),
    /// Where each item was, over what's there now (the window asks first).
    Original,
}

impl RestoreJob {
    /// Restores with `restic` from `repo`, for `job`: the window's restore, through the same
    /// core calls as `replica restore`.
    pub fn run(
        &self,
        restic: &Restic,
        repo: &Repository,
        job: &Job,
        cancel: &CancelToken,
    ) -> Result<RestoreSummary, String> {
        let sources: Vec<PathBuf> = job.sources.iter().map(|s| s.path.clone()).collect();
        match &self.to {
            RestoreTo::Folder(folder) => {
                let items: Vec<&str> = self.items.iter().map(String::as_str).collect();
                let request = RestoreRequest {
                    job: &job.name,
                    snapshot: &self.snapshot,
                    item: None,
                    target: folder,
                    sources: &sources,
                    allow_over_source: false,
                };
                restic
                    .restore_many(repo, &request, &items, cancel)
                    .map_err(|e| e.to_string())
            }
            RestoreTo::Original => {
                let mut total = RestoreSummary::default();
                for item in &self.items {
                    let original = local_path(item);
                    let parent = original.parent().unwrap_or(&original).to_path_buf();
                    let request = RestoreRequest {
                        job: &job.name,
                        snapshot: &self.snapshot,
                        item: Some(item),
                        target: &parent,
                        sources: &sources,
                        allow_over_source: true,
                    };
                    let done = restic
                        .restore(repo, &request, cancel)
                        .map_err(|e| e.to_string())?;
                    total.total_files += done.total_files;
                    total.files_restored += done.files_restored;
                    total.files_skipped += done.files_skipped;
                    total.total_bytes += done.total_bytes;
                    total.bytes_restored += done.bytes_restored;
                }
                Ok(total)
            }
        }
    }
}

/// The real thing: the app's folder, the tray's channel, Credential Manager, Task Scheduler.
pub struct LiveBackend {
    pub paths: AppPaths,
    pub requests: Mutex<Sender<UiRequest>>,
    pub status: Arc<Mutex<Status>>,
    secrets: replica_win::credentials::CredentialManager,
    scheduler: Scheduler,
    /// restic's cache, if not restic's own default ([`Self::with_restic_cache`]).
    restic_cache: Option<PathBuf>,
    /// What the restic work running now waits for ([`Backend::waiting`]).
    waiting: Mutex<Option<String>>,
    /// The window's `HWND`, which owns the file dialogs ([`Backend::set_owner`]); 0 until then.
    owner: AtomicIsize,
}

impl LiveBackend {
    pub fn new(paths: AppPaths, requests: Sender<UiRequest>, status: Arc<Mutex<Status>>) -> Self {
        Self::with(
            paths,
            requests,
            status,
            runner::secrets(),
            runner::scheduler(),
        )
    }

    /// With a Credential Manager prefix and a Task Scheduler folder of its own (for tests and
    /// the `window` example).
    pub fn with(
        paths: AppPaths,
        requests: Sender<UiRequest>,
        status: Arc<Mutex<Status>>,
        secrets: replica_win::credentials::CredentialManager,
        scheduler: Scheduler,
    ) -> Self {
        Self {
            paths,
            requests: Mutex::new(requests),
            status,
            secrets,
            scheduler,
            restic_cache: None,
            waiting: Mutex::new(None),
            owner: AtomicIsize::new(0),
        }
    }

    /// Shows a file dialog over the window, logging that it opened and what it gave, so a
    /// dialog that never comes back shows in the log.
    fn file_dialog(
        &self,
        what: &str,
        dialog: rfd::FileDialog,
        show: impl FnOnce(rfd::FileDialog) -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        // The window lives as long as the process: it's never closed, only hidden.
        let owner = DialogOwner::new(self.owner.load(Ordering::SeqCst));
        let dialog = match &owner {
            Some(owner) => dialog.set_parent(owner),
            None => dialog,
        };
        tracing::info!(
            dialog = what,
            owned = owner.is_some(),
            "a file dialog opened"
        );
        let chosen = show(dialog);
        match &chosen {
            Some(path) => {
                tracing::info!(dialog = what, path = %path.display(), "a file dialog closed")
            }
            None => tracing::info!(dialog = what, "a file dialog was cancelled"),
        }
        chosen
    }

    /// Keeps restic's cache in `dir` instead of restic's own default (for tests: nothing
    /// touches the user's own cache).
    pub fn with_restic_cache(mut self, dir: impl Into<PathBuf>) -> Self {
        self.restic_cache = Some(dir.into());
        self
    }

    /// Takes `target`'s repository lock for restic work outside a run, waiting its turn
    /// as a run does, up to the app's `repository_wait_minutes`; what it waits for is shown
    /// meanwhile ([`Backend::waiting`]). Held until dropped, on this thread.
    fn take_turn(
        &self,
        target: &Target,
        holder: &str,
        cancel: &CancelToken,
    ) -> Result<repo_lock::Held, repo_lock::AcquireError> {
        let bound = Waits::from_settings(&self.settings().app).turn;
        let set = |text: Option<String>| {
            *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = text;
        };
        let taken = repo_lock::acquire_target(
            target,
            holder,
            &NamedMutexLocks::new(),
            &self.paths.file("locks"),
            repo_lock::Turn::alone(bound),
            cancel,
            &mut |wait| set(wait.map(Wait::text)),
        );
        set(None);
        taken
    }

    /// restic as the settings find it, with this backend's cache.
    fn restic(&self) -> Result<Restic, String> {
        runner::find_restic(&self.settings()).map(|r| match &self.restic_cache {
            Some(dir) => r.with_cache_dir(dir),
            None => r,
        })
    }

    fn settings(&self) -> Settings {
        runner::load_settings(&self.paths)
            .map(|l| l.settings)
            .unwrap_or_default()
    }

    /// Runs `work` with restic and the target's repository, as `replica` opens it; a share
    /// connected for it is disconnected afterwards.
    fn with_repository<T>(
        &self,
        target: &Target,
        work: impl FnOnce(&Restic, &Repository) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_repository_or_unavailable(target, work)
            .map_err(Unreachable::into_text)
    }

    /// As [`Self::with_repository`], telling a target that isn't there (as a run would skip
    /// it) from one that failed.
    fn with_repository_or_unavailable<T>(
        &self,
        target: &Target,
        work: impl FnOnce(&Restic, &Repository) -> Result<T, String>,
    ) -> Result<T, Unreachable> {
        let restic = self.restic().map_err(Unreachable::Failed)?;
        let targets = WindowsTargets::new();
        let result = match resolve_target(target, &targets, &self.secrets) {
            Ok(TargetLocation::Available(location)) => {
                Repository::from_store(&location, target, &self.secrets)
                    .map_err(|e| e.to_string())
                    .and_then(|repo| work(&restic, &repo))
                    .map_err(Unreachable::Failed)
            }
            Ok(TargetLocation::NotAvailable(why)) => Err(Unreachable::NotAvailable(tr!(
                "restore-target-not-available",
                target = target.display_name(),
                reason = why
            ))),
            Err(e) => Err(Unreachable::Failed(e.0)),
        };
        targets.disconnect();
        result
    }
}

impl Backend for LiveBackend {
    fn settings_file(&self) -> PathBuf {
        self.paths.settings_file()
    }

    fn writable(&self) -> bool {
        self.paths.is_writable()
    }

    fn window_state_file(&self) -> PathBuf {
        self.paths.file(super::geometry::FILE_NAME)
    }

    fn send(&self, request: UiRequest) {
        let _ = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(request);
    }

    fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn secrets(&self) -> &dyn SecretStore {
        &self.secrets
    }

    fn volumes(&self) -> Result<Vec<VolumeInfo>, String> {
        list_volumes().map_err(|e| e.0)
    }

    fn has_task(&self, job: &str) -> bool {
        self.scheduler
            .list(&[])
            .map(|tasks| tasks.iter().any(|t| t.name.eq_ignore_ascii_case(job)))
            .unwrap_or(false)
    }

    fn delete_task(&self, job: &str) -> Result<(), String> {
        self.scheduler.delete(job).map_err(|e| e.0)
    }

    fn check_target(&self, target: &Target) -> TargetCheck {
        let targets = WindowsTargets::new();
        let check = match resolve_target(target, &targets, &self.secrets) {
            Ok(TargetLocation::Available(location)) => TargetCheck::Ready {
                initialised: is_repository(&location),
                has_password: matches!(keys::for_restic(target, &self.secrets), Ok(Some(_))),
                location,
            },
            Ok(TargetLocation::NotAvailable(why)) => TargetCheck::NotAvailable(why),
            Err(e) => TargetCheck::Failed(e.0),
        };
        targets.disconnect();
        check
    }

    fn init_repository(&self, target: &Target, secret: Secret) -> Result<(), String> {
        let restic = self.restic()?;
        let targets = WindowsTargets::new();
        let result = match resolve_target(target, &targets, &self.secrets) {
            Ok(TargetLocation::Available(location)) => self
                .take_turn(target, &tr("holder-window"), &CancelToken::new())
                .map_err(|e| e.to_string())
                .and_then(|_turn| {
                    restic
                        .init_target(&Repository::new(location.to_string_lossy(), secret))
                        .map_err(|e| e.to_string())
                }),
            Ok(TargetLocation::NotAvailable(why)) => Err(why),
            Err(e) => Err(e.0),
        };
        targets.disconnect();
        result
    }

    fn save_entered_password(
        &self,
        target: &Target,
        secret: Secret,
        force: bool,
    ) -> Result<Entered, NotSaved> {
        let restic = self.restic().map_err(NotSaved::Failed)?;
        let targets = WindowsTargets::new();
        let result = match resolve_target(target, &targets, &self.secrets) {
            Ok(TargetLocation::Available(location)) => {
                let opens = |s: &Secret| {
                    restic.opens(&Repository::new(location.to_string_lossy(), s.clone()))
                };
                keys::save_entered(target, secret, &self.secrets, force, &opens)
            }
            Ok(TargetLocation::NotAvailable(why)) => Err(NotSaved::Failed(tr!(
                "restore-target-not-available",
                target = target.display_name(),
                reason = why
            ))),
            Err(e) => Err(NotSaved::Failed(e.0)),
        };
        targets.disconnect();
        result
    }

    fn set_owner(&self, hwnd: isize) {
        self.owner.store(hwnd, Ordering::SeqCst);
    }

    fn pick_folder(&self) -> Option<PathBuf> {
        self.file_dialog("pick folder", rfd::FileDialog::new(), |d| d.pick_folder())
    }

    fn pick_file(&self) -> Option<PathBuf> {
        self.file_dialog("pick file", rfd::FileDialog::new(), |d| d.pick_file())
    }

    fn save_jobs_file(&self) -> Option<PathBuf> {
        let dialog = rfd::FileDialog::new()
            .add_filter(tr("jobs-file-filter"), &["toml"])
            .set_file_name(format!("replica{}", replica_core::jobs_file::EXTENSION));
        self.file_dialog("export jobs", dialog, |d| d.save_file())
    }

    fn open_jobs_file(&self) -> Option<PathBuf> {
        let dialog = rfd::FileDialog::new().add_filter(tr("jobs-file-filter"), &["toml"]);
        self.file_dialog("import jobs", dialog, |d| d.pick_file())
    }

    fn runs(&self, filter: &HistoryFilter, limit: usize) -> Result<Vec<RunRecord>, String> {
        runner::open_history(&self.paths)?
            .runs(filter, limit)
            .map_err(|e| e.to_string())
    }

    fn test_restores(
        &self,
        filter: &HistoryFilter,
        limit: usize,
    ) -> Result<Vec<TestRestoreRecord>, String> {
        runner::open_history(&self.paths)?
            .test_restores(filter, limit)
            .map_err(|e| e.to_string())
    }

    fn history_jobs(&self) -> Result<Vec<String>, String> {
        runner::open_history(&self.paths)?
            .job_names()
            .map_err(|e| e.to_string())
    }

    fn places(&self) -> Places {
        Places {
            portable: self.paths.is_portable(),
            folder: self.paths.root().to_path_buf(),
            settings: self.paths.settings_file(),
            history: self.paths.file(runner::HISTORY_FILE),
            logs: self.paths.logs_dir(),
            run_logs: self.paths.run_logs_dir(),
            writable: self.paths.is_writable(),
        }
    }

    fn tasks(&self, jobs: &[&str]) -> Result<Vec<ScheduledTask>, String> {
        self.scheduler.list(jobs).map_err(|e| e.0)
    }

    fn set_task_enabled(&self, task: &str, enabled: bool) -> Result<(), String> {
        self.scheduler.set_enabled(task, enabled).map_err(|e| e.0)
    }

    fn delete_all_tasks(&self) -> Result<usize, String> {
        self.scheduler.delete_all().map_err(|e| e.0)
    }

    fn turn_off_tasks(&self, delete: bool) -> Result<usize, String> {
        self.scheduler.turn_off(delete).map_err(|e| e.0)
    }

    fn open_task_scheduler(&self) -> Result<(), String> {
        replica_win::window::open(Path::new("taskschd.msc")).map_err(|e| e.to_string())
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        replica_win::window::open(Path::new(url)).map_err(|e| e.to_string())
    }

    fn restic_in_use(&self, override_path: Option<PathBuf>) -> Result<(PathBuf, String), String> {
        let path = replica_core::restic::ResticSearch::for_this_exe(override_path)
            .find()
            .map_err(|e| e.to_string())?;
        let restic = Restic::new(&path).map_err(|e| e.to_string())?;
        // Shown as people write it: no `..`, no `\\?\`.
        let shown = std::fs::canonicalize(&path)
            .map(|p| PathBuf::from(replica_core::paths::without_verbatim_prefix(&p)))
            .unwrap_or(path);
        Ok((shown, restic.version().to_owned()))
    }

    fn set_pinned(
        &self,
        job: &Job,
        target: &Target,
        id: &str,
        pinned: bool,
    ) -> Result<String, String> {
        self.with_repository(target, |restic, repo| {
            let cancel = CancelToken::new();
            let _turn = self
                .take_turn(target, &job.name, &cancel)
                .map_err(|e| e.to_string())?;
            repo_lock::retry_locked(restic, repo, repo_lock::LISTING_WAIT, &cancel, || {
                restic.set_pinned(repo, id, pinned)
            })
            .map_err(|e| e.to_string())
        })
    }

    fn waiting(&self) -> Option<String> {
        self.waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn open_path(&self, path: &Path) -> Result<(), String> {
        replica_win::window::open(path).map_err(|e| e.to_string())
    }

    fn snapshots(&self, job: &Job, target: &Target) -> Result<Vec<Snapshot>, String> {
        self.with_repository(target, |restic, repo| {
            // A quick look: restic's own lock is retried briefly.
            let cancel = CancelToken::new();
            repo_lock::retry_locked(restic, repo, repo_lock::LISTING_WAIT, &cancel, || {
                restic.snapshots(repo, Some(&job.name))
            })
            .map_err(|e| e.to_string())
        })
    }

    fn list(&self, target: &Target, snapshot: &str, folder: &str) -> Result<Vec<Node>, String> {
        self.with_repository(target, |restic, repo| {
            let cancel = CancelToken::new();
            let nodes =
                repo_lock::retry_locked(restic, repo, repo_lock::LISTING_WAIT, &cancel, || {
                    restic.ls(repo, snapshot, Some(folder))
                })
                .map_err(|e| e.to_string())?;
            Ok(nodes
                .into_iter()
                .filter(|n| !n.path.eq_ignore_ascii_case(folder))
                .collect())
        })
    }

    fn restore(
        &self,
        job: &Job,
        target: &Target,
        restore: &RestoreJob,
        cancel: &CancelToken,
    ) -> Result<RestoreSummary, String> {
        self.with_repository(target, |restic, repo| {
            let _turn = self
                .take_turn(target, &job.name, cancel)
                .map_err(|e| e.to_string())?;
            restore.run(restic, repo, job, cancel)
        })
    }

    fn test_restore(
        &self,
        job: &Job,
        target: &Target,
        cancel: &CancelToken,
    ) -> Result<TestRestoreRecord, String> {
        let started_at = Utc::now();
        let clock = std::time::Instant::now();
        let mut cancelled = false;
        let attempt = self.with_repository_or_unavailable(target, |restic, repo| {
            let _turn = self.take_turn(target, &job.name, cancel).map_err(|e| {
                cancelled = matches!(e, repo_lock::AcquireError::Cancelled);
                e.to_string()
            })?;
            replica_core::test_restore::run_in_temp(
                restic,
                repo,
                &job.name,
                &std::env::temp_dir(),
                cancel,
            )
            .map_err(|e| {
                cancelled = matches!(e, ResticError::Cancelled);
                e.to_string()
            })
        });
        let name = target.display_name();
        // Every attempt is recorded, a failure most of all, except one the user stopped.
        let (mut record, error) = match attempt {
            Ok((report, at, duration)) => (report.record(&job.name, &name, at, duration), None),
            Err(_) if cancelled => return Err(tr("error-cancelled")),
            Err(ended) => {
                let (outcome, text) = match ended {
                    Unreachable::NotAvailable(text) => (Outcome::Skipped, text),
                    Unreachable::Failed(text) => (Outcome::Failed, text),
                };
                let record = TestRestoreRecord {
                    id: 0,
                    job: job.name.clone(),
                    target: name,
                    snapshot_id: None,
                    started_at,
                    duration: clock.elapsed(),
                    outcome,
                    checked: 0,
                    differing: 0,
                    detail: Some(text.clone()),
                };
                (record, Some(text))
            }
        };
        let history = runner::open_history(&self.paths)?;
        record.id = history
            .record_test_restore(&record)
            .map_err(|e| e.to_string())?;
        match error {
            Some(text) => Err(text),
            None => Ok(record),
        }
    }
}

/// Changes the settings as they are on disk now (not as the window last read them), checks
/// them with core's validation, saves them and tells the tray to load them: how every change in
/// the window takes effect at once.
pub fn change_settings(
    backend: &dyn Backend,
    edit: impl FnOnce(&mut Settings),
) -> Result<Settings, String> {
    let path = backend.settings_file();
    let mut settings = Settings::load_or_default(&path)
        .map_err(|e| e.to_string())?
        .settings;
    edit(&mut settings);
    let errors: Vec<String> = settings.validate().iter().map(|e| e.to_string()).collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    settings.save(&path).map_err(|e| e.to_string())?;
    backend.send(UiRequest::Reload);
    Ok(settings)
}

/// Why work on a target didn't happen.
enum Unreachable {
    /// It isn't connected or reachable now (a run would be "skipped").
    NotAvailable(String),
    Failed(String),
}

impl Unreachable {
    fn into_text(self) -> String {
        match self {
            Unreachable::NotAvailable(text) | Unreachable::Failed(text) => text,
        }
    }
}

/// Whether a restic repository is at `location` (its `config` file is there).
pub fn is_repository(location: &Path) -> bool {
    location.join("config").is_file()
}
