//! Running jobs from the tray, the same way `replica run` does: the same paths, settings,
//! history, secret store, target resolver, restic and per-job lock.

use std::path::Path;
use std::sync::Arc;

use replica_core::cancel::CancelToken;
use replica_core::history::History;
use replica_core::job::Job;
use replica_core::restic::{Restic, ResticSearch};
use replica_core::run::{RunContext, RunEvent, RunReport, Waits, run_job};
use replica_core::settings::{Loaded, Settings, SettingsError};
use replica_win::credentials::CredentialManager;
use replica_win::jobs::JobObjectTree;
use replica_win::paths::AppPaths;
use replica_win::repo_locks::NamedMutexLocks;
use replica_win::scheduler::Scheduler;
use replica_win::targets::WindowsTargets;

use crate::cli::CREDENTIAL_PREFIX_VAR;

/// The app's name for its folders, logs and single-instance objects.
pub const APP: &str = "Replica";
/// The history database, in the app's folder.
pub const HISTORY_FILE: &str = "history.db";

/// For tests: a name of their own, `ReplicaTest-…`, that gives the tray its own Task
/// Scheduler folder, Start with Windows key (never the Run key), single-instance objects and
/// Credential Manager prefix, so a test tray never touches an installed Replica's, nor wakes
/// or quits its tray. A value that doesn't start with `ReplicaTest-` is ignored.
pub const TEST_TAG_VAR: &str = "REPLICA_TEST_TAG";

/// The test's own name ([`TEST_TAG_VAR`]), if one is set.
pub fn test_tag() -> Option<String> {
    let tag = std::env::var(TEST_TAG_VAR).ok()?;
    if !is_test_tag(&tag) {
        tracing::warn!(tag, "not a test's name: ignored");
        return None;
    }
    Some(tag)
}

/// Whether `tag` can be a test's own name: `ReplicaTest-` and more, in letters, digits, `-`,
/// `_` and `.` only, so it makes a Task Scheduler folder, a registry key and object names.
pub fn is_test_tag(tag: &str) -> bool {
    tag.strip_prefix("ReplicaTest-")
        .is_some_and(|rest| !rest.is_empty())
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// The secret store: Credential Manager under `Replica/`, or a test's own prefix
/// (`REPLICA_CREDENTIAL_PREFIX`, else its [`test_tag`]).
pub fn secrets() -> CredentialManager {
    match std::env::var(CREDENTIAL_PREFIX_VAR) {
        Ok(prefix) if !prefix.is_empty() => CredentialManager::with_prefix(&prefix),
        _ => match test_tag() {
            Some(tag) => CredentialManager::with_prefix(&format!("{tag}/")),
            None => CredentialManager::new(),
        },
    }
}

/// Replica's tasks: Task Scheduler's `\Replica` folder, or a test's own.
pub fn scheduler() -> Scheduler {
    match test_tag() {
        Some(tag) => Scheduler::in_folder(&format!(r"\{tag}")),
        None => Scheduler::new(),
    }
}

/// The name of the tray's single-instance objects (`Replica.Tray`), or a test's own.
pub fn instance_name() -> String {
    format!("{}.Tray", test_tag().as_deref().unwrap_or(APP))
}

/// The settings, or why they can't be read.
pub fn load_settings(paths: &AppPaths) -> Result<Loaded, SettingsError> {
    Settings::load_or_default(&paths.settings_file())
}

/// The run history: the file in the app's folder, or one in memory when that folder can't be
/// written to (nothing is written anywhere else).
pub fn open_history(paths: &AppPaths) -> Result<History, String> {
    if paths.is_writable() {
        History::open(&paths.file(HISTORY_FILE)).map_err(|e| e.to_string())
    } else {
        History::in_memory().map_err(|e| e.to_string())
    }
}

/// restic, or why it can't be used (only versioned targets need it).
pub fn find_restic(settings: &Settings) -> Result<Restic, String> {
    ResticSearch::for_this_exe(settings.app.restic_path.clone())
        .find()
        .and_then(Restic::new)
        .map(|r| r.with_tree(Arc::new(JobObjectTree)))
        .map_err(|e| e.to_string())
}

/// How a run from the tray's queue takes its turn for a repository.
#[derive(Default, Clone, Copy)]
pub struct QueueTurn<'a> {
    /// How long it has already waited its turn, in earlier tries: counted off the bound.
    pub waited: std::time::Duration,
    /// Whether a job queued behind it could run now: then it gives way
    /// ([`RunReport::gave_way`]).
    pub give_way: Option<&'a dyn Fn() -> bool>,
}

/// Runs `job` to the end (or until `cancel`), recording it in `history`, as `replica run`
/// does, unless it gives way to a job behind it in its queue (`turn`). Shares connected for
/// it are disconnected when it ends.
pub fn run_one(
    paths: &AppPaths,
    settings: &Settings,
    job: &Job,
    history: &mut History,
    cancel: &CancelToken,
    turn: QueueTurn<'_>,
    on_event: &mut dyn FnMut(&RunEvent),
) -> RunReport {
    let restic = find_restic(settings);
    let secrets = secrets();
    let targets = WindowsTargets::new();
    let (logs, locks, staging) = (
        paths.run_logs_dir(),
        paths.file("locks"),
        paths.file("staging"),
    );
    let repo_locks = NamedMutexLocks::new();
    let ctx = RunContext {
        restic: restic.as_ref().ok(),
        resolver: &targets,
        secrets: &secrets,
        logs_dir: &logs,
        locks_dir: &locks,
        repo_locks: &repo_locks,
        waits: {
            let bound = Waits::from_settings(&settings.app);
            Waits {
                turn: bound.turn.saturating_sub(turn.waited),
                ..bound
            }
        },
        give_way: turn.give_way,
        staging_dir: &staging,
        processes: &JobObjectTree,
        cancel,
    };
    run_job(job, &ctx, history, on_event)
}

/// A run's progress, as a percentage, from one of its events.
pub fn percent(event: &RunEvent) -> Option<u32> {
    match event {
        RunEvent::Backup(p) => Some((p.percent_done * 100.0).clamp(0.0, 100.0) as u32),
        RunEvent::Mirror(p) if p.bytes_total > 0 => {
            Some((p.bytes_done.saturating_mul(100) / p.bytes_total).min(100) as u32)
        }
        _ => None,
    }
}

/// The locks folder: where `replica run`, the tray and Task Scheduler runs take each job's lock.
pub fn locks_dir(paths: &AppPaths) -> std::path::PathBuf {
    paths.file("locks")
}

/// What [`reconcile_tasks`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// Task Scheduler is on: the tasks now match the jobs.
    Synced(replica_win::scheduler::SyncReport),
    /// Task Scheduler is off: this many tasks that were still on were removed.
    Removed(usize),
}

/// Makes Replica's tasks follow the saved schedule mode, whenever the tray loads the settings
/// (at start too, so an option turned off while the tray wasn't running is honoured):
///
/// - **Task Scheduler on:** a task for each scheduled, enabled job, and no other.
/// - **Off (the built-in timer):** no task may keep running jobs, so the ones still on are
///   removed. Any the user turned off are left, as the window's "keep them" answer
///   leaves them, and `--run` refuses to run while the option is off anyway.
pub fn reconcile_tasks(
    scheduler: &replica_win::scheduler::Scheduler,
    settings: &Settings,
    exe: &Path,
) -> Result<Reconciled, replica_core::platform::PlatformError> {
    match settings.app.schedule_mode {
        replica_core::settings::ScheduleMode::TaskScheduler => {
            scheduler.sync(&settings.jobs, exe).map(Reconciled::Synced)
        }
        replica_core::settings::ScheduleMode::Timer => {
            scheduler.delete_enabled().map(Reconciled::Removed)
        }
    }
}

/// The modified time of the settings file, to notice an edit.
pub fn settings_stamp(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica_core::run::{JobLock, Outcome};

    #[test]
    fn only_a_replica_test_name_isolates_the_tray() {
        assert!(is_test_tag("ReplicaTest-123-tray"));
        assert!(is_test_tag("ReplicaTest-a_b.c"));
        for not in [
            "",
            "Replica",
            "ReplicaTest-",
            "replicatest-1",
            r"ReplicaTest-1\..\Replica",
            "ReplicaTest-1/x",
            "ReplicaTest-1 2",
        ] {
            assert!(!is_test_tag(not), "{not:?}");
        }
    }

    /// A portable copy in a temp folder: everything stays there.
    fn portable() -> (tempfile::TempDir, AppPaths) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("portable"), "").unwrap();
        let paths = AppPaths::resolve(APP, temp.path(), None).unwrap();
        (temp, paths)
    }

    /// A job that a `replica run` (or Task Scheduler) is running already is skipped from the
    /// tray: same lock folder, same lock, nothing done and nothing recorded.
    #[test]
    fn a_job_a_cli_run_holds_is_skipped() {
        let (_temp, paths) = portable();
        let job = Job {
            name: "hive".into(),
            ..Job::default()
        };
        let held = JobLock::try_acquire(&locks_dir(&paths), "hive")
            .unwrap()
            .expect("the CLI's lock");
        let mut history = open_history(&paths).unwrap();
        let report = run_one(
            &paths,
            &Settings::default(),
            &job,
            &mut history,
            &CancelToken::new(),
            QueueTurn::default(),
            &mut |_| {},
        );
        assert_eq!(report.outcome, Outcome::Skipped);
        assert!(history.recent(Some("hive"), 10).unwrap().is_empty());
        drop(held);
    }

    #[test]
    fn the_history_lives_in_the_app_s_folder() {
        let (_temp, paths) = portable();
        assert!(paths.is_writable());
        assert!(open_history(&paths).is_ok());
        assert!(paths.file(HISTORY_FILE).exists());
    }

    #[test]
    fn progress_is_a_percentage() {
        use replica_core::mirror::MirrorProgress;
        let mirror = RunEvent::Mirror(MirrorProgress {
            files_done: 1,
            files_total: 4,
            bytes_done: 250,
            bytes_total: 1000,
            current: None,
        });
        assert_eq!(percent(&mirror), Some(25));
        assert_eq!(percent(&RunEvent::Started { job: "x".into() }), None);
    }
}
