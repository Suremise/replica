//! The window's Jobs, History and Restore tabs and the job editor, driven through egui's accessibility tree (what
//! Narrator reads) with `egui_kittest`, against a fake backend: settings in a temp folder,
//! secrets in memory, and the tray, Task Scheduler and restic recorded instead of run.

#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use replica::ui::backend::{
    Backend, Places, RestoreJob, RestoreTo, Status, TargetCheck, UiRequest,
};
use replica::ui::history::HistoryTab;
use replica::ui::jobs::JobsTab;
use replica::ui::jobs::editor::{Editor, EditorResult};
use replica::ui::restore::{Done, RestoreTab};
use replica_core::cancel::CancelToken;
use replica_core::history::{HistoryFilter, RunRecord, TargetRecord, TestRestoreRecord};
use replica_core::job::{Job, Mode, Source, Target, VolumeRef};
use replica_core::platform::fake::MemorySecretStore;
use replica_core::platform::{Secret, SecretStore};
use replica_core::restic::password_secret_name;
use replica_core::restic::{Node, ResticError, RestoreSummary, Snapshot};
use replica_core::run::Outcome;
use replica_core::settings::Settings;
use replica_win::keys::{self, Entered, NotSaved};
use replica_win::scheduler::ScheduledTask;
use replica_win::targets::VolumeInfo;

#[derive(Default)]
struct Fake {
    dir: PathBuf,
    requests: Mutex<Vec<UiRequest>>,
    secrets: MemorySecretStore,
    tasks: Mutex<Vec<String>>,
    deleted_tasks: Mutex<Vec<String>>,
    check: Mutex<Option<TargetCheck>>,
    created: Mutex<Vec<(String, String)>>,
    status: Mutex<Status>,
    runs: Mutex<Vec<RunRecord>>,
    tests: Mutex<Vec<TestRestoreRecord>>,
    opened: Mutex<Vec<PathBuf>>,
    snapshots: Mutex<Vec<Snapshot>>,
    /// A folder's contents, by its path in the snapshot.
    folders: Mutex<HashMap<String, Vec<Node>>>,
    restores: Mutex<Vec<RestoreJob>>,
    /// The passwords that open the target's repository.
    repository_keys: Mutex<Vec<String>>,
    /// While set, a pin or unpin waits until it's sent to (or dropped): one waiting its turn.
    pin_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// What the Save and Open dialogs for a jobs file answer.
    jobs_file: Mutex<Option<PathBuf>>,
}

impl Backend for Fake {
    fn settings_file(&self) -> PathBuf {
        self.dir.join("settings.toml")
    }
    fn writable(&self) -> bool {
        true
    }
    fn window_state_file(&self) -> PathBuf {
        self.dir.join("window.json")
    }
    fn send(&self, request: UiRequest) {
        self.requests.lock().unwrap().push(request);
    }
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    fn secrets(&self) -> &dyn SecretStore {
        &self.secrets
    }
    fn volumes(&self) -> Result<Vec<VolumeInfo>, String> {
        Ok(vec![VolumeInfo {
            id: r"\\?\Volume{1}\".into(),
            label: Some("BACKUP".into()),
            mount_points: vec![PathBuf::from(r"E:\")],
            file_system: Some("NTFS".into()),
            total_bytes: Some(2_000_000_000_000),
            free_bytes: Some(1_000_000_000_000),
            removable: false,
        }])
    }
    fn has_task(&self, job: &str) -> bool {
        self.tasks.lock().unwrap().iter().any(|t| t == job)
    }
    fn delete_task(&self, job: &str) -> Result<(), String> {
        self.deleted_tasks.lock().unwrap().push(job.into());
        Ok(())
    }
    fn check_target(&self, _: &Target) -> TargetCheck {
        self.check.lock().unwrap().clone().expect("a check was set")
    }
    fn init_repository(&self, target: &Target, secret: Secret) -> Result<(), String> {
        self.created
            .lock()
            .unwrap()
            .push((target.display_name(), secret.expose().to_owned()));
        Ok(())
    }
    fn save_entered_password(
        &self,
        target: &Target,
        secret: Secret,
        force: bool,
    ) -> Result<Entered, NotSaved> {
        let opening = self.repository_keys.lock().unwrap().clone();
        let opens = |s: &Secret| {
            if opening.iter().any(|k| k == s.expose()) {
                Ok(())
            } else {
                Err(ResticError::WrongPassword {
                    location: r"E:\Replica".into(),
                })
            }
        };
        keys::save_entered(target, secret, &self.secrets, force, &opens)
    }
    fn pick_folder(&self) -> Option<PathBuf> {
        None
    }
    fn pick_file(&self) -> Option<PathBuf> {
        None
    }
    fn save_jobs_file(&self) -> Option<PathBuf> {
        self.jobs_file.lock().unwrap().clone()
    }
    fn open_jobs_file(&self) -> Option<PathBuf> {
        self.jobs_file.lock().unwrap().clone()
    }
    fn runs(&self, filter: &HistoryFilter, limit: usize) -> Result<Vec<RunRecord>, String> {
        let mut runs: Vec<RunRecord> = self
            .runs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| passes(filter, &r.job, r.outcome))
            .cloned()
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        runs.truncate(limit);
        Ok(runs)
    }
    fn test_restores(
        &self,
        filter: &HistoryFilter,
        limit: usize,
    ) -> Result<Vec<TestRestoreRecord>, String> {
        let mut tests: Vec<TestRestoreRecord> = self
            .tests
            .lock()
            .unwrap()
            .iter()
            .filter(|t| passes(filter, &t.job, t.outcome))
            .cloned()
            .collect();
        tests.sort_by_key(|t| std::cmp::Reverse(t.started_at));
        tests.truncate(limit);
        Ok(tests)
    }
    fn history_jobs(&self) -> Result<Vec<String>, String> {
        let mut jobs: Vec<String> = self
            .runs
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.job.clone())
            .collect();
        jobs.extend(self.tests.lock().unwrap().iter().map(|t| t.job.clone()));
        jobs.sort();
        jobs.dedup();
        Ok(jobs)
    }
    // Settings and Task Scheduler: tests/window_settings.rs uses the real backend for those.
    fn places(&self) -> Places {
        Places {
            portable: false,
            folder: self.dir.clone(),
            settings: self.settings_file(),
            history: self.dir.join("history.db"),
            logs: self.dir.join("logs"),
            run_logs: self.dir.join("logs").join("runs"),
            writable: true,
        }
    }
    fn tasks(&self, _: &[&str]) -> Result<Vec<ScheduledTask>, String> {
        Ok(Vec::new())
    }
    fn set_task_enabled(&self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn delete_all_tasks(&self) -> Result<usize, String> {
        Ok(0)
    }
    fn turn_off_tasks(&self, _: bool) -> Result<usize, String> {
        Ok(0)
    }
    fn open_task_scheduler(&self) -> Result<(), String> {
        Ok(())
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.opened.lock().unwrap().push(PathBuf::from(url));
        Ok(())
    }
    fn restic_in_use(&self, _: Option<PathBuf>) -> Result<(PathBuf, String), String> {
        Ok((PathBuf::from("restic.exe"), "0.19.1".into()))
    }
    fn open_path(&self, path: &Path) -> Result<(), String> {
        self.opened.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
    fn snapshots(&self, _: &Job, _: &Target) -> Result<Vec<Snapshot>, String> {
        Ok(self.snapshots.lock().unwrap().clone())
    }
    fn list(&self, _: &Target, _: &str, folder: &str) -> Result<Vec<Node>, String> {
        Ok(self
            .folders
            .lock()
            .unwrap()
            .get(folder)
            .cloned()
            .unwrap_or_default())
    }
    fn restore(
        &self,
        _: &Job,
        _: &Target,
        restore: &RestoreJob,
        _: &CancelToken,
    ) -> Result<RestoreSummary, String> {
        self.restores.lock().unwrap().push(restore.clone());
        Ok(RestoreSummary {
            files_restored: 1,
            bytes_restored: 3,
            ..RestoreSummary::default()
        })
    }
    /// As restic does: the tag changed, and a new ID for the rewritten snapshot.
    fn set_pinned(&self, _: &Job, _: &Target, id: &str, pinned: bool) -> Result<String, String> {
        // Taken out first, so the lock isn't held while it waits.
        let gate = self.pin_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.recv_timeout(Duration::from_secs(30));
        }
        let mut snapshots = self.snapshots.lock().unwrap();
        let s = snapshots
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("no snapshot {id}"))?;
        s.tags.retain(|t| t != replica_core::restic::PINNED_TAG);
        if pinned {
            s.tags.push(replica_core::restic::PINNED_TAG.to_string());
        }
        s.id = format!("{id}-{}", if pinned { "p" } else { "u" });
        s.short_id = s.id.chars().take(8).collect();
        Ok(s.id.clone())
    }
    fn test_restore(
        &self,
        job: &Job,
        target: &Target,
        _: &CancelToken,
    ) -> Result<TestRestoreRecord, String> {
        let record = TestRestoreRecord {
            id: 1,
            job: job.name.clone(),
            target: target.display_name(),
            snapshot_id: Some("abcdef0123".into()),
            started_at: chrono::Utc::now(),
            duration: Duration::from_secs(1),
            outcome: Outcome::Failed,
            checked: 5,
            differing: 1,
            detail: Some("/C/Work/broken.txt".into()),
        };
        self.tests.lock().unwrap().push(record.clone());
        Ok(record)
    }
}

/// What the real history's filter lets through.
fn passes(filter: &HistoryFilter, job: &str, outcome: Outcome) -> bool {
    filter
        .job
        .as_deref()
        .is_none_or(|j| j.eq_ignore_ascii_case(job))
        && filter.outcome.is_none_or(|o| o == outcome)
}

fn drive(label: &str) -> Target {
    Target::Drive {
        name: None,
        volume: VolumeRef {
            volume_id: None,
            label: Some(label.into()),
        },
        path: "Replica".into(),
    }
}

fn job(name: &str, mode: Mode) -> Job {
    Job {
        name: name.into(),
        mode,
        mirror_deletes_confirmed: mode == Mode::Mirror,
        sources: vec![Source {
            path: r"C:\Work".into(),
            exclude: vec![],
        }],
        targets: vec![match mode {
            Mode::Versioned => drive("BACKUP"),
            // A mirror's own folder: it may not overlap another mirror or a repository.
            Mode::Mirror => Target::Drive {
                name: None,
                volume: VolumeRef {
                    volume_id: None,
                    label: Some("BACKUP".into()),
                },
                path: format!("Mirror-{name}").into(),
            },
        }],
        ..Job::default()
    }
}

/// A fake backend whose settings hold `jobs`, and the temp folder it lives in.
fn backend(jobs: Vec<Job>) -> (Arc<Fake>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake {
        dir: dir.path().to_path_buf(),
        ..Fake::default()
    };
    Settings {
        jobs,
        ..Settings::default()
    }
    .save(&fake.settings_file())
    .unwrap();
    (Arc::new(fake), dir)
}

fn saved_jobs(fake: &Fake) -> Vec<Job> {
    Settings::load(&fake.settings_file()).unwrap().settings.jobs
}

fn setup(ctx: &eframe::egui::Context) {
    replica::ui::fonts::install(ctx).unwrap();
    replica::ui::style(ctx);
}

fn editor_harness(
    fake: &Arc<Fake>,
    original: Option<Job>,
) -> Harness<'static, (Editor, EditorResult)> {
    let editor = Editor::new(Arc::clone(fake) as Arc<dyn Backend>, original);
    Harness::builder()
        .with_size([1000.0, 2400.0])
        .with_context_setup(setup)
        .build_ui_state(
            |ui, (editor, result): &mut (Editor, EditorResult)| {
                let r = editor.ui(ui);
                if r != EditorResult::Open {
                    *result = r;
                }
            },
            (editor, EditorResult::Open),
        )
}

/// Steps the harness until `done` holds (work runs on other threads), for at most 10 s.
fn step_until<S>(harness: &mut Harness<'_, S>, mut done: impl FnMut(&Harness<'_, S>) -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done(harness) {
        assert!(Instant::now() < until, "timed out");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

const MIRROR: &str = "Mirror: keeps an exact copy, and deletions are copied too";

#[test]
fn choosing_mirror_needs_the_warning_s_confirmation() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Versioned)]);
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Versioned)));
    harness.run();

    harness.get_by_label(MIRROR).click();
    harness.run();
    harness.get_by_label("A mirror copies deletions too");
    assert_eq!(
        harness.state().0.form.mode,
        Mode::Versioned,
        "not until confirmed"
    );

    harness.get_by_label("Keep it versioned").click();
    harness.run();
    assert!(
        harness
            .query_by_label("A mirror copies deletions too")
            .is_none()
    );
    assert_eq!(harness.state().0.form.mode, Mode::Versioned);

    harness.get_by_label(MIRROR).click();
    harness.run();
    harness.get_by_label("Use a mirror anyway").click();
    harness.run();
    let form = &harness.state().0.form;
    assert_eq!(form.mode, Mode::Mirror);
    assert!(form.mirror_deletes_confirmed);
}

#[test]
fn saving_writes_the_settings_and_tells_the_tray() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Mirror)]);
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Mirror)));
    harness.run();
    harness.state_mut().0.form.schedule = "daily 02:30".into();
    harness.get_by_label("Save").click();
    harness.run();

    assert_eq!(harness.state().1, EditorResult::Saved("docs".into()));
    let jobs = saved_jobs(&fake);
    assert_eq!(
        jobs[0].schedule.as_ref().unwrap().to_string(),
        "daily 02:30"
    );
    assert_eq!(*fake.requests.lock().unwrap(), [UiRequest::Reload]);
}

#[test]
fn core_s_validation_stops_a_save() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Mirror), job("other", Mode::Mirror)]);
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Mirror)));
    harness.run();
    harness.state_mut().0.form.name = "Other".into();
    harness.state_mut().0.form.schedule = "now and then".into();
    harness.get_by_label("Save").click();
    harness.run();
    // The schedule is checked first; its message names what was typed.
    harness.get_by_label_contains("now and then");
    harness.state_mut().0.form.schedule.clear();
    harness.get_by_label("Save").click();
    harness.run();
    // Core's message, shown with a capital.
    harness.get_by_label_contains("Another job is also called");
    assert_eq!(harness.state().1, EditorResult::Open);
    assert_eq!(saved_jobs(&fake)[0].name, "docs", "nothing written");
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[test]
fn a_new_versioned_target_shows_its_password_once_and_cancel_saves_nothing() {
    let (fake, _dir) = backend(vec![]);
    *fake.check.lock().unwrap() = Some(TargetCheck::Ready {
        location: PathBuf::from(r"E:\Replica"),
        initialised: false,
        has_password: false,
    });
    let mut harness = editor_harness(&fake, None);
    harness.run();
    {
        let form = &mut harness.state_mut().0.form;
        form.name = "docs".into();
        form.add_source(PathBuf::from(r"C:\Work"));
        form.targets
            .push(replica::ui::jobs::form::TargetForm::drive());
        form.targets[0].volume.label = Some("BACKUP".into());
    }
    harness.get_by_label("Save").click();
    step_until(&mut harness, |h| {
        h.query_by_label("Save a copy of this password").is_some()
    });
    harness.get_by_label("Copy password");
    // The password window's Cancel, over the editor's own.
    harness
        .query_all_by_label("Cancel")
        .last()
        .expect("a Cancel")
        .click();
    harness.run();

    assert!(
        harness
            .query_by_label("Save a copy of this password")
            .is_none()
    );
    assert!(saved_jobs(&fake).is_empty(), "the job isn't saved");
    assert!(fake.created.lock().unwrap().is_empty());
    assert!(
        fake.secrets.entries.lock().unwrap().is_empty(),
        "the password isn't stored"
    );
}

#[test]
fn confirming_the_password_stores_it_creates_the_repository_and_saves() {
    let (fake, _dir) = backend(vec![]);
    *fake.check.lock().unwrap() = Some(TargetCheck::Ready {
        location: PathBuf::from(r"E:\Replica"),
        initialised: false,
        has_password: false,
    });
    let mut harness = editor_harness(&fake, None);
    harness.run();
    {
        let form = &mut harness.state_mut().0.form;
        form.name = "docs".into();
        form.add_source(PathBuf::from(r"C:\Work"));
        form.targets
            .push(replica::ui::jobs::form::TargetForm::drive());
        form.targets[0].volume.label = Some("BACKUP".into());
    }
    harness.get_by_label("Save").click();
    step_until(&mut harness, |h| {
        h.query_by_label("Save a copy of this password").is_some()
    });
    harness.get_by_label("I've saved a copy").click();
    step_until(&mut harness, |h| h.state().1 != EditorResult::Open);

    assert_eq!(harness.state().1, EditorResult::Saved("docs".into()));
    let created = fake.created.lock().unwrap().clone();
    assert_eq!(created.len(), 1);
    let stored = fake
        .secrets
        .get(&password_secret_name(&drive("BACKUP")))
        .unwrap()
        .expect("stored");
    assert_eq!(
        stored.secret.expose(),
        created[0].1,
        "the shown password is used"
    );
    assert_eq!(saved_jobs(&fake)[0].name, "docs");
}

#[test]
fn a_target_that_isn_t_connected_can_be_saved_without_setting_it_up() {
    let (fake, _dir) = backend(vec![]);
    *fake.check.lock().unwrap() = Some(TargetCheck::NotAvailable("not connected".into()));
    let mut harness = editor_harness(&fake, None);
    harness.run();
    {
        let form = &mut harness.state_mut().0.form;
        form.name = "docs".into();
        form.add_source(PathBuf::from(r"C:\Work"));
        form.targets
            .push(replica::ui::jobs::form::TargetForm::drive());
        form.targets[0].volume.label = Some("BACKUP".into());
    }
    harness.get_by_label("Save").click();
    step_until(&mut harness, |h| {
        h.query_by_label("Save without setting it up").is_some()
    });
    assert!(saved_jobs(&fake).is_empty(), "not until asked");
    harness.get_by_label("Save without setting it up").click();
    harness.run();
    assert_eq!(harness.state().1, EditorResult::Saved("docs".into()));
    assert!(fake.created.lock().unwrap().is_empty());
}

fn saved_password(fake: &Fake) -> Option<String> {
    fake.secrets
        .get(&password_secret_name(&drive("BACKUP")))
        .unwrap()
        .map(|c| c.secret.expose().to_owned())
}

/// Opens "Enter a saved password…" on the job's target, types `password` and checks it.
fn enter_password(harness: &mut Harness<'_, (Editor, EditorResult)>, password: &str) {
    if harness.query_by_label("Enter a saved password").is_none() {
        let name = format!(
            "Enter a saved password for {}",
            drive("BACKUP").display_name()
        );
        harness.get_by_label(&name).click();
        harness.run();
    }
    harness.get_by_label("Password").focus();
    harness.run();
    harness.get_by_label("Password").type_text(password);
    harness.run();
    harness.get_by_label("Check and save").click();
    // Checking shows a spinner, which keeps repainting: the caller steps until it's done.
    harness.step();
}

#[test]
fn an_entered_password_is_saved_only_once_it_opens_the_repository() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Versioned)]);
    *fake.repository_keys.lock().unwrap() = vec!["right".into(), "also-right".into()];
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Versioned)));
    harness.run();
    let target = drive("BACKUP").display_name();

    enter_password(&mut harness, "wrong");
    let wrong = format!("That password doesn't open the backup at {target}, so it wasn't saved.");
    step_until(&mut harness, |h| h.query_by_label(&wrong).is_some());
    assert_eq!(saved_password(&fake), None, "a wrong one isn't saved");

    enter_password(&mut harness, "right");
    let saved = format!("Saved the repository password for {target}: it opens the backup there.");
    step_until(&mut harness, |h| h.query_by_label(&saved).is_some());
    assert!(harness.query_by_label("Enter a saved password").is_none());
    assert_eq!(saved_password(&fake).as_deref(), Some("right"));

    // One that opens it too doesn't replace it, until asked to.
    enter_password(&mut harness, "also-right");
    step_until(&mut harness, |h| {
        h.query_by_label("Replace it anyway").is_some()
    });
    assert_eq!(saved_password(&fake).as_deref(), Some("right"));
    // A button new this frame moves once laid out: let it settle before clicking its middle.
    harness.run();
    harness.get_by_label("Replace it anyway").click();
    harness.step();
    let replaced = format!(
        "Saved the repository password for {target} in place of the one saved before: it opens the backup there."
    );
    step_until(&mut harness, |h| h.query_by_label(&replaced).is_some());
    assert_eq!(saved_password(&fake).as_deref(), Some("also-right"));
    assert_eq!(
        harness.state().1,
        EditorResult::Open,
        "the job isn't saved by it"
    );
    assert!(
        fake.created.lock().unwrap().is_empty(),
        "nor a repository made"
    );
}

fn jobs_harness(fake: &Arc<Fake>) -> Harness<'static, JobsTab> {
    Harness::builder()
        .with_size([1200.0, 800.0])
        .with_context_setup(setup)
        .build_ui_state(
            |ui, tab: &mut JobsTab| tab.ui(ui),
            JobsTab::new(Arc::clone(fake) as Arc<dyn Backend>),
        )
}

#[test]
fn the_jobs_list_runs_turns_off_and_deletes_with_a_confirmation() {
    let (fake, _dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("music", Mode::Mirror),
    ]);
    fake.tasks.lock().unwrap().push("music".into());
    let mut harness = jobs_harness(&fake);
    harness.run();

    harness.get_by_label("Run docs now").click();
    harness.run();
    harness.get_by_label("Run all").click();
    harness.run();
    assert_eq!(
        *fake.requests.lock().unwrap(),
        [
            UiRequest::Run(vec!["docs".into()]),
            UiRequest::Run(vec!["docs".into(), "music".into()]),
        ]
    );

    harness.get_by_label("docs is on").click();
    harness.run();
    assert!(!saved_jobs(&fake)[0].enabled);
    assert_eq!(
        fake.requests.lock().unwrap().last(),
        Some(&UiRequest::Reload)
    );

    // Delete is in the row's ⋯ menu.
    harness.get_by_label("More actions for music").click();
    harness.run();
    harness.get_by_label("Delete music").click();
    harness.run();
    harness.get_by_label("Its task in Task Scheduler is deleted too.");
    harness.get_by_label("Cancel").click();
    harness.run();
    assert_eq!(saved_jobs(&fake).len(), 2, "cancel keeps it");

    // Delete is in the row's ⋯ menu.
    harness.get_by_label("More actions for music").click();
    harness.run();
    harness.get_by_label("Delete music").click();
    harness.run();
    harness.get_by_label("Delete the job").click();
    harness.run();
    let jobs = saved_jobs(&fake);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].name, "docs");
    assert_eq!(*fake.deleted_tasks.lock().unwrap(), ["music"]);
    harness.get_by_label("Deleted music.");
}

#[test]
fn every_control_has_a_name_for_screen_readers() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Versioned)]);
    let mut harness = jobs_harness(&fake);
    harness.run();
    harness.check_accessibility();

    let mut full = job("docs", Mode::Versioned);
    full.targets.push(Target::Unc {
        name: None,
        path: r"\\nas\backup".into(),
        credential: Some(r"share:\\nas\backup".into()),
    });
    full.before.push(replica_core::job::BeforeStep::Command {
        cmd: "echo".into(),
        timeout_secs: 10,
    });
    full.after.push(replica_core::job::AfterStep::Verify {
        schedule: None,
        read_data_subset: "5%".into(),
    });
    let mut harness = editor_harness(&fake, Some(full));
    harness.run();
    harness.check_accessibility();
}

#[test]
fn a_running_job_shows_its_progress_and_a_queued_one_waits() {
    let (fake, _dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("music", Mode::Mirror),
    ]);
    *fake.status.lock().unwrap() = Status {
        running: Some(("docs".into(), 42)),
        queued: vec!["music".into()],
        ..Status::default()
    };
    let mut harness = jobs_harness(&fake);
    harness.run();
    harness.get_by_label_contains("Running, 42%");
    harness.get_by_label("Waiting its turn");
    harness.get_by_label("Stop docs").click();
    harness.run();
    assert_eq!(fake.requests.lock().unwrap().last(), Some(&UiRequest::Stop));
}

/// A running job waiting for another job's repository shows "Waiting its turn", says
/// what for, and can still be stopped.
#[test]
fn a_job_waiting_for_a_shared_repository_says_so_and_can_be_stopped() {
    let (fake, _dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("photos", Mode::Versioned),
    ]);
    *fake.status.lock().unwrap() = Status {
        running: Some(("photos".into(), 0)),
        waiting: Some("Waiting for docs to finish with BACKUP".into()),
        ..Status::default()
    };
    let mut harness = jobs_harness(&fake);
    harness.run();
    let chip = harness.get_by_label("Waiting its turn");
    let at = chip.rect().center();
    assert!(
        harness.query_by_label_contains("Running").is_none(),
        "waiting, not running"
    );
    harness.hover_at(at);
    harness.run();
    harness.get_by_label_contains("Waiting for docs to finish with BACKUP");
    harness.get_by_label("Stop photos").click();
    harness.run();
    assert_eq!(fake.requests.lock().unwrap().last(), Some(&UiRequest::Stop));
}

/// A job whose runs keep failing past its schedule shows Overdue, with when it was due,
/// its last good backup and its last run's result in the hover text; one that isn't overdue
/// shows its last result as before.
#[test]
fn an_overdue_job_says_so_whatever_its_last_result() {
    let (fake, _dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("photos", Mode::Versioned),
    ]);
    let now = chrono::Utc::now();
    let last = |outcome, reason: Option<&str>| replica::ui::backend::LastRun {
        at: now - chrono::TimeDelta::hours(1),
        outcome,
        reason: reason.map(Into::into),
    };
    *fake.status.lock().unwrap() = Status {
        last: [
            (
                "docs".to_string(),
                last(
                    Outcome::Failed,
                    Some("Waited 120 min for photos to finish with BACKUP"),
                ),
            ),
            ("photos".to_string(), last(Outcome::Ok, None)),
        ]
        .into(),
        overdue: [(
            "docs".to_string(),
            replica::ui::backend::Overdue {
                due: now - chrono::TimeDelta::days(2),
                last_success: Some(now - chrono::TimeDelta::days(9)),
            },
        )]
        .into(),
        ..Status::default()
    };
    let mut harness = jobs_harness(&fake);
    harness.run();
    let chip = harness.get_by_label("Overdue");
    let at = chip.rect().center();
    assert_eq!(
        harness.query_all_by_label("Overdue").count(),
        1,
        "only the overdue job"
    );
    harness.get_by_label("OK");
    harness.hover_at(at);
    harness.run();
    harness.get_by_label_contains("Overdue since ");
    harness.get_by_label_contains("Its last good backup was");
    harness.get_by_label_contains("Last run: Failed. Waited 120 min for photos");
}

#[test]
fn leaving_the_editor_with_changes_asks_first() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Versioned)]);
    // Nothing changed: Cancel leaves at once.
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Versioned)));
    harness.run();
    harness.get_by_label("Cancel").click();
    harness.run();
    assert_eq!(harness.state().1, EditorResult::Cancelled);

    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Versioned)));
    harness.run();
    harness.state_mut().0.form.schedule = "daily 02:30".into();
    harness.get_by_label("Back to Jobs").click();
    harness.run();
    harness.get_by_label("Discard changes?");
    harness.get_by_label("Keep editing").click();
    harness.run();
    assert_eq!(harness.state().1, EditorResult::Open, "kept");
    assert_eq!(harness.state().0.form.schedule, "daily 02:30");

    harness.query_all_by_label("Cancel").next().unwrap().click();
    harness.run();
    harness.get_by_label("Discard changes").click();
    harness.run();
    assert_eq!(harness.state().1, EditorResult::Cancelled);
    assert!(saved_jobs(&fake)[0].schedule.is_none(), "nothing saved");
}

#[test]
fn a_saved_share_login_shows_as_saved_and_its_password_never_shows() {
    let mut docs = job("docs", Mode::Versioned);
    docs.targets = vec![Target::Unc {
        name: None,
        path: r"\\nas\backup\docs".into(),
        credential: Some(r"share:\\nas\backup".into()),
    }];
    let (fake, _dir) = backend(vec![docs.clone()]);
    fake.secrets
        .set(
            r"share:\\nas\backup",
            &replica_core::platform::Credential {
                username: Some("alex".into()),
                secret: Secret::new("hunter2"),
            },
        )
        .unwrap();
    let mut harness = editor_harness(&fake, Some(docs));
    harness.run();
    harness.get_by_label("Saved · alex");
    assert!(harness.query_by_label("User name").is_none(), "no fields");
    assert!(harness.query_by_label_contains("hunter2").is_none());

    // Change… shows the fields; Save login stores the new one at once.
    harness.get_by_label("Change…").click();
    harness.run();
    harness.get_by_label("User name").focus();
    harness.run();
    harness.get_by_label("User name").type_text("backup");
    harness.get_by_label("Password").focus();
    harness.run();
    harness.get_by_label("Password").type_text("s3cret");
    harness.run();
    harness.get_by_label("Save login").click();
    harness.run();
    let saved = fake.secrets.get(r"share:\\nas\backup").unwrap().unwrap();
    assert_eq!(saved.username.as_deref(), Some("backup"));
    assert_eq!(saved.secret.expose(), "s3cret");
    harness.get_by_label("Saved · backup");
    assert!(harness.query_by_label("User name").is_none());
}

#[test]
fn changing_the_targets_after_the_offline_offer_takes_it_away_and_checks_again() {
    for change in ["edit the target", "add a target"] {
        let (fake, _dir) = backend(vec![]);
        *fake.check.lock().unwrap() = Some(TargetCheck::NotAvailable("not connected".into()));
        let mut harness = editor_harness(&fake, None);
        harness.run();
        {
            let form = &mut harness.state_mut().0.form;
            form.name = "docs".into();
            form.add_source(PathBuf::from(r"C:\Work"));
            form.targets
                .push(replica::ui::jobs::form::TargetForm::drive());
            form.targets[0].volume.label = Some("OFFLINE".into());
        }
        harness.get_by_label("Save").click();
        step_until(&mut harness, |h| {
            h.query_by_label("Save without setting it up").is_some()
        });

        // A different, connected destination now.
        *fake.check.lock().unwrap() = Some(TargetCheck::Ready {
            location: PathBuf::from(r"E:\Replica"),
            initialised: false,
            has_password: false,
        });
        {
            let form = &mut harness.state_mut().0.form;
            match change {
                "edit the target" => form.targets[0].volume.label = Some("BACKUP".into()),
                _ => {
                    form.targets
                        .push(replica::ui::jobs::form::TargetForm::drive());
                    form.targets[1].volume.label = Some("BACKUP".into());
                }
            }
        }
        harness.run();
        assert!(
            harness
                .query_by_label("Save without setting it up")
                .is_none(),
            "{change}: the offer goes with the form it was made for"
        );
        assert!(saved_jobs(&fake).is_empty(), "{change}");

        harness.get_by_label("Save").click();
        step_until(&mut harness, |h| {
            h.query_by_label("Save a copy of this password").is_some()
        });
        assert!(
            saved_jobs(&fake).is_empty(),
            "{change}: not before the password"
        );
        assert!(fake.created.lock().unwrap().is_empty(), "{change}");
    }
}

// --- History ------------------------------------------------------------------------------

fn run_record(id: i64, job: &str, outcome: Outcome) -> RunRecord {
    RunRecord {
        id,
        job: job.into(),
        started_at: chrono::Utc::now() - chrono::TimeDelta::hours(id),
        duration: Duration::from_secs(12),
        outcome,
        exit_code: outcome.exit_code(),
        files: 4,
        bytes: 2_000,
        error: (outcome == Outcome::Failed).then(|| "restic said no".into()),
        log_path: Some(PathBuf::from(format!(r"C:\logs\{id}.log"))),
        targets: vec![TargetRecord {
            target: "BACKUP:Replica".into(),
            mode: "versioned".into(),
            outcome,
            files: 4,
            bytes: 2_000,
            reason: (outcome == Outcome::Skipped).then(|| "the target isn't available".into()),
            snapshot_id: None,
            skipped: Some(0),
        }],
    }
}

fn history_harness(fake: &Arc<Fake>) -> Harness<'static, HistoryTab> {
    Harness::builder()
        .with_size([1200.0, 800.0])
        .with_context_setup(setup)
        .build_ui_state(
            |ui, tab: &mut HistoryTab| tab.ui(ui),
            HistoryTab::new(Arc::clone(fake) as Arc<dyn Backend>),
        )
}

#[test]
fn history_shows_every_run_with_its_targets_and_filters_them() {
    let (fake, _dir) = backend(vec![]);
    *fake.runs.lock().unwrap() = vec![
        run_record(1, "docs", Outcome::Ok),
        run_record(2, "music", Outcome::Skipped),
        run_record(3, "docs", Outcome::Failed),
    ];
    let mut harness = history_harness(&fake);
    harness.run();
    assert_eq!(harness.state().shown().len(), 3);
    harness.check_accessibility();

    // A run opens to its targets, why it was skipped, and its log.
    harness.get_by_label_contains("music").click();
    harness.run();
    harness.get_by_label_contains("the target isn't available");
    harness.get_by_label("Open the log").click();
    harness.run();
    assert_eq!(
        *fake.opened.lock().unwrap(),
        [PathBuf::from(r"C:\logs\2.log")]
    );

    harness.state_mut().job = Some("docs".into());
    harness.run();
    assert_eq!(harness.state().shown().len(), 2);
    harness.state_mut().result = Some(Outcome::Failed);
    harness.run();
    assert_eq!(harness.state().shown().len(), 1);
    assert!(harness.query_by_label_contains("music").is_none());
}

#[test]
fn a_partly_done_run_says_how_many_files_it_skipped_unless_its_row_is_older() {
    let (fake, _dir) = backend(vec![]);
    let mut counted = run_record(1, "photos", Outcome::Partly);
    counted.targets[0].skipped = Some(3);
    counted.targets[0].reason = Some("3 files couldn't be backed up.".into());
    // A row from before the history kept the count.
    let mut old = run_record(2, "music", Outcome::Partly);
    old.targets[0].skipped = None;
    *fake.runs.lock().unwrap() = vec![counted, old];
    let mut harness = history_harness(&fake);
    harness.run();
    assert_eq!(
        harness
            .query_all_by_label("Partly done · 3 skipped")
            .count(),
        1,
        "the counted run's chip"
    );
    assert_eq!(
        harness.query_all_by_label("Partly done").count(),
        1,
        "the old one's"
    );

    // Its details: the target's chip says it too.
    harness.get_by_label_contains("photos").click();
    harness.run();
    assert_eq!(
        harness
            .query_all_by_label("Partly done · 3 skipped")
            .count(),
        2,
        "the row's chip and the target's in the details"
    );
    harness.get_by_label_contains("3 files couldn't be backed up.");
}

#[test]
fn history_s_dropdowns_are_named_and_an_empty_list_says_why() {
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;
    let (fake, _dir) = backend(vec![]);
    let mut harness = history_harness(&fake);
    harness.run();
    harness.get_by_label("No runs yet");
    harness.check_accessibility();

    *fake.runs.lock().unwrap() = vec![run_record(1, "docs", Outcome::Ok)];
    harness.get_by_label("Look again").click();
    harness.run();
    assert_eq!(harness.state().shown().len(), 1);
    // Each dropdown carries its label, and is read by it with its choice as its value.
    for (name, value) in [
        ("Job", "All jobs"),
        ("Result", "All results"),
        ("When", "Any time"),
    ] {
        let dropdown = harness.get_by_role_and_label(Role::ComboBox, name);
        assert_eq!(dropdown.accesskit_node().value().as_deref(), Some(value));
    }

    // Filters that match nothing say so, and clearing them shows every run again.
    harness.state_mut().result = Some(Outcome::Failed);
    harness.run();
    harness.get_by_label("No runs match these filters");
    harness.get_by_label("Clear filters").click();
    harness.run();
    assert_eq!(harness.state().result, None);
    assert_eq!(harness.state().shown().len(), 1);
}

/// A long job name, near the most core allows.
const LONG_JOB: &str = "documents-and-every-project-folder-from-the-old-laptop-2026";

#[test]
fn a_long_job_name_is_cut_on_screen_but_read_whole() {
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;
    let (fake, _dir) = backend(vec![job(LONG_JOB, Mode::Versioned)]);
    *fake.runs.lock().unwrap() = vec![run_record(1, LONG_JOB, Outcome::Ok)];

    // In a narrow window, the Jobs table's name is cut, its mode's tag still in view.
    let mut jobs = Harness::builder()
        .with_size([800.0, 600.0])
        .with_context_setup(setup)
        .build_ui_state(
            |ui, tab: &mut JobsTab| tab.ui(ui),
            JobsTab::new(Arc::clone(&fake) as Arc<dyn Backend>),
        );
    jobs.run();
    let name = jobs.get_by_role_and_label(Role::Label, LONG_JOB).rect();
    let tag = jobs.get_by_label("Versioned").rect();
    let next_column = jobs.get_by_label("Sort by Status").rect();
    assert!(name.right() <= tag.left(), "the name ends before its tag");
    assert!(
        tag.right() <= next_column.left(),
        "the tag is in its column: {tag:?}, the next starts at {next_column:?}"
    );

    // History's job dropdown, with it chosen: cut to its widest, its whole name the value.
    let mut history = history_harness(&fake);
    history.run();
    history.state_mut().job = Some(LONG_JOB.into());
    history.run();
    let dropdown = history.get_by_role_and_label(Role::ComboBox, "Job");
    assert_eq!(dropdown.accesskit_node().value().as_deref(), Some(LONG_JOB));
    assert!(dropdown.rect().width() <= 260.0, "{:?}", dropdown.rect());
}

#[test]
fn a_click_on_a_row_s_node_chooses_it_whatever_the_first_column_s_width() {
    // The narrowest, the one that once failed, and a wide one, at two scales: the full sweep is
    // the stress test below.
    clicks_on_rows_choose_them(&[100.0, 145.0, 220.0], &[1.0, 1.5], &[1200.0, 800.0]);
}

#[test]
#[ignore = "stress: the full sweep of widths, scales and sizes (docs/TESTING.md, Tiers)"]
fn stress_a_click_on_a_row_s_node_chooses_it_at_every_width_scale_and_size() {
    clicks_on_rows_choose_them(
        &[100.0, 120.0, 130.0, 140.0, 145.0, 150.0, 175.0, 220.0],
        &[1.0, 1.25, 1.5],
        &[1200.0, 800.0],
    );
}

/// For each first-column width the History tab starts at, scale and window width, a click at
/// each row's node chooses that row.
fn clicks_on_rows_choose_them(widths: &[f32], scales: &[f32], sizes: &[f32]) {
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;
    let (fake, _dir) = backend(vec![]);
    *fake.runs.lock().unwrap() = vec![
        run_record(1, "docs", Outcome::Ok),
        run_record(2, "photos", Outcome::Partly),
        run_record(3, "music", Outcome::Failed),
    ];
    let variants = widths
        .iter()
        .flat_map(|&w| scales.iter().map(move |&scale| (w, scale)))
        .flat_map(|(w, scale)| sizes.iter().map(move |&size| (w, scale, size)));
    for (width, scale, size) in variants {
        let mut tab = HistoryTab::new(Arc::clone(&fake) as Arc<dyn Backend>);
        tab.started_width = width;
        let mut harness = Harness::builder()
            .with_size([size, 800.0])
            .with_pixels_per_point(scale)
            .with_context_setup(setup)
            .build_ui_state(|ui, tab: &mut HistoryTab| tab.ui(ui), tab);
        harness.run();
        for job in ["photos", "music", "docs"] {
            let row = harness
                .get_all_by_role(Role::ListBoxOption)
                .find(|n| n.accesskit_node().label().unwrap_or_default().contains(job))
                .unwrap_or_else(|| panic!("{job}'s row"));
            let rect = row.rect();
            row.click();
            harness.run();
            let chosen = harness
                .get_all_by_role(Role::ListBoxOption)
                .find(|n| n.accesskit_node().is_selected() == Some(true))
                .map(|n| n.accesskit_node().label().unwrap_or_default());
            assert!(
                chosen.as_deref().is_some_and(|l| l.contains(job)),
                "Started at {width}, at {scale}x, {size} wide: a click at {:?} (the middle of {rect:?}) chose {chosen:?}, not {job}",
                rect.center()
            );
        }
    }
}

#[test]
fn a_screen_reader_s_click_on_a_row_chooses_it() {
    use eframe::egui::accesskit::{Action, Role};
    use egui_kittest::kittest::NodeT;
    let (fake, _dir) = backend(vec![]);
    *fake.runs.lock().unwrap() = vec![
        run_record(1, "docs", Outcome::Ok),
        run_record(2, "photos", Outcome::Partly),
    ];
    let mut harness = history_harness(&fake);
    harness.run();
    for job in ["photos", "docs"] {
        let row = harness
            .get_all_by_role(Role::ListBoxOption)
            .find(|n| n.accesskit_node().label().unwrap_or_default().contains(job))
            .unwrap_or_else(|| panic!("{job}'s row"));
        assert!(
            row.accesskit_node().data().supports_action(Action::Click),
            "{job}'s row offers Click"
        );
        row.click_accesskit();
        harness.run();
        let chosen = harness
            .get_all_by_role(Role::ListBoxOption)
            .find(|n| n.accesskit_node().is_selected() == Some(true))
            .map(|n| n.accesskit_node().label().unwrap_or_default());
        assert!(
            chosen.as_deref().is_some_and(|l| l.contains(job)),
            "a reader's Click on {job} chose {chosen:?}"
        );
    }
}

// --- Restore ------------------------------------------------------------------------------

fn node(path: &str, kind: &str) -> Node {
    Node {
        name: path.rsplit('/').next().unwrap().into(),
        kind: kind.into(),
        path: path.into(),
        size: 3,
        mtime: None,
    }
}

/// A fake with job "docs" (source C:\Work) and one snapshot of it holding /C/Work/a.txt and
/// /C/Work/sub/b.txt.
fn restore_backend() -> (Arc<Fake>, tempfile::TempDir) {
    let mut docs = job("docs", Mode::Versioned);
    docs.sources[0].path = r"C:\Work".into();
    let (fake, dir) = backend(vec![docs]);
    *fake.snapshots.lock().unwrap() = vec![Snapshot {
        id: "abcdef0123456789".into(),
        short_id: "abcdef01".into(),
        time: chrono::Utc::now().fixed_offset(),
        paths: vec![r"C:\Work".into()],
        tags: vec!["docs".into()],
        hostname: "PC".into(),
        summary: None,
    }];
    let mut folders = HashMap::new();
    folders.insert(
        "/C/Work".to_string(),
        vec![node("/C/Work/sub", "dir"), node("/C/Work/a.txt", "file")],
    );
    folders.insert(
        "/C/Work/sub".to_string(),
        vec![node("/C/Work/sub/b.txt", "file")],
    );
    *fake.folders.lock().unwrap() = folders;
    (fake, dir)
}

fn restore_harness(fake: &Arc<Fake>) -> Harness<'static, RestoreTab> {
    let mut tab = RestoreTab::new(Arc::clone(fake) as Arc<dyn Backend>);
    tab.job = Some("docs".into());
    Harness::builder()
        .with_size([1200.0, 900.0])
        .with_context_setup(setup)
        .build_ui_state(|ui, tab: &mut RestoreTab| tab.ui(ui), tab)
}

/// Picks the snapshot and opens the source folder.
fn browse_to_files(harness: &mut Harness<'static, RestoreTab>) {
    step_until(harness, |h| h.query_by_label_contains("abcdef01").is_some());
    harness.get_by_label_contains("abcdef01").click();
    harness.run();
    harness.get_by_label(r"C:\Work").click();
    step_until(harness, |h| h.query_by_label_contains("a.txt").is_some());
}

/// Pin and Unpin on Restore: a pinned snapshot shows it, the same snapshot stays chosen
/// under the new ID restic gives it, and unpinning asks first.
#[test]
fn a_snapshot_is_pinned_and_unpinned_from_restore() {
    let (fake, _dir) = restore_backend();
    let mut harness = restore_harness(&fake);
    step_until(&mut harness, |h| {
        h.query_by_label_contains("abcdef01").is_some()
    });
    assert!(
        harness.query_by_label("Pin this backup").is_none(),
        "no snapshot chosen yet"
    );
    harness.get_by_label_contains("abcdef01").click();
    harness.run();
    harness.get_by_label("Pin this backup").click();
    step_until(&mut harness, |h| h.query_by_label("Unpin…").is_some());
    let pinned = fake.snapshots.lock().unwrap()[0].clone();
    assert!(pinned.is_pinned(), "{pinned:?}");
    assert_eq!(
        harness.state().snapshot.as_ref().map(|s| s.id.as_str()),
        Some(pinned.id.as_str()),
        "still chosen, under its new ID"
    );
    harness.get_by_label_contains("Pinned: it's kept");
    assert!(
        harness.query_by_label_contains(", Pinned").is_some(),
        "the row says so to a screen reader"
    );
    harness.check_accessibility();

    // Unpinning asks first; Cancel keeps the pin.
    harness.get_by_label("Unpin…").click();
    harness.run();
    harness.get_by_label("Unpin this backup?");
    harness.get_by_label("Cancel").click();
    harness.run();
    assert!(fake.snapshots.lock().unwrap()[0].is_pinned());
    harness.get_by_label("Unpin…").click();
    harness.run();
    harness.get_by_label("Unpin").click();
    step_until(&mut harness, |h| {
        h.query_by_label("Pin this backup").is_some()
    });
    assert!(!fake.snapshots.lock().unwrap()[0].is_pinned());
}

/// Round 1's finding: while a pin waits (its turn for the repository, say), the page is busy:
/// no other snapshot, job or action can be chosen, so its result lands where it was started.
#[test]
fn the_restore_page_waits_for_a_pin_in_progress() {
    let (fake, _dir) = restore_backend();
    {
        let mut snapshots = fake.snapshots.lock().unwrap();
        let mut older = snapshots[0].clone();
        older.id = "0123456789abcdef".into();
        older.short_id = "01234567".into();
        older.time -= chrono::TimeDelta::days(1);
        snapshots.push(older);
    }
    let (release, gate) = std::sync::mpsc::channel::<()>();
    *fake.pin_gate.lock().unwrap() = Some(gate);
    // Lets the pin go however the test ends.
    struct Release(std::sync::mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let release = Release(release);

    let mut harness = restore_harness(&fake);
    step_until(&mut harness, |h| {
        h.query_by_label_contains("abcdef01").is_some()
    });
    harness.get_by_label_contains("abcdef01").click();
    harness.run();
    harness.get_by_label("Pin this backup").click();
    step_until(&mut harness, |h| h.query_by_label("Pinning…").is_some());
    assert!(
        harness.state().busy(),
        "a pin in progress keeps the page busy"
    );
    // The other snapshot can't be chosen meanwhile.
    harness.get_by_label_contains("01234567").click();
    // The spinner keeps repainting: a few frames, rather than until it settles.
    harness.run_steps(4);
    assert_eq!(
        harness.state().snapshot.as_ref().map(|s| s.id.as_str()),
        Some("abcdef0123456789")
    );

    drop(release);
    step_until(&mut harness, |h| h.query_by_label("Unpin…").is_some());
    assert!(!harness.state().busy());
    let chosen = harness.state().snapshot.clone().unwrap();
    assert!(
        chosen.is_pinned() && chosen.id.starts_with("abcdef"),
        "{chosen:?}"
    );
}

/// The editor's Keep choice: Everything, forever hides the counts and says nothing is
/// deleted; Newest backups brings them back.
#[test]
fn keep_forever_is_chosen_in_the_editor() {
    let (fake, _dir) = backend(vec![job("docs", Mode::Versioned)]);
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Versioned)));
    harness.run();
    harness.get_by_label("How many");
    harness.get_by_label("Everything, forever").click();
    harness.run();
    assert!(harness.state().0.form.retention.keep_forever);
    assert!(
        harness.query_by_label("How many").is_none(),
        "the counts hide"
    );
    use egui_kittest::kittest::NodeT as _;
    let help = harness.get_by_label("Keep").accesskit_node().description();
    assert!(
        help.as_deref()
            .is_some_and(|h| h.contains("Nothing is ever deleted")),
        "{help:?}"
    );
    harness.get_by_label("Newest backups").click();
    harness.run();
    assert!(!harness.state().0.form.retention.keep_forever);
    harness.get_by_label("How many");
}

#[test]
fn restore_browses_a_snapshot_and_restores_the_chosen_files_to_a_new_folder() {
    let (fake, _dir) = restore_backend();
    let mut harness = restore_harness(&fake);
    browse_to_files(&mut harness);

    // A folder inside is listed only when it's opened.
    assert!(harness.query_by_label_contains("b.txt").is_none());
    harness.get_by_label("sub").click();
    step_until(&mut harness, |h| {
        h.query_by_label_contains("b.txt").is_some()
    });
    harness.check_accessibility();

    harness.get_by_label_contains("a.txt").click();
    harness.get_by_label("All of sub").click();
    harness.run();
    let folder = harness.state().folder.clone();
    assert!(folder.contains("docs"), "a new folder by default: {folder}");
    harness.get_by_label("Restore 2 items").click();
    step_until(&mut harness, |h| h.state().done.is_some());

    let restores = fake.restores.lock().unwrap().clone();
    assert_eq!(
        restores,
        [RestoreJob {
            snapshot: "abcdef0123456789".into(),
            items: vec!["/C/Work/a.txt".into(), "/C/Work/sub".into()],
            to: RestoreTo::Folder(PathBuf::from(&folder)),
        }]
    );
    // Through AccessKit: the Pin row puts it below this harness's window.
    harness.get_by_label("Open the folder").click_accesskit();
    harness.run();
    assert_eq!(*fake.opened.lock().unwrap(), [PathBuf::from(&folder)]);
}

#[test]
fn a_snapshot_listed_late_is_chosen_by_a_click_on_the_frame_it_shows() {
    // The snapshots come after the harness's first frames, as under load: the table's
    // rows are new that frame, and a row's node must have its rect already, or a click lands
    // at NaN.
    let (fake, _dir) = restore_backend();
    let held = fake.snapshots.lock().unwrap();
    let mut harness = restore_harness(&fake);
    drop(held);
    step_until(&mut harness, |h| {
        h.query_by_label_contains("abcdef01").is_some()
    });
    harness.get_by_label_contains("abcdef01").click();
    harness.run();
    harness.get_by_label(r"C:\Work");
}

#[test]
fn restoring_over_the_originals_needs_a_confirmation() {
    let (fake, _dir) = restore_backend();
    let mut harness = restore_harness(&fake);
    browse_to_files(&mut harness);
    harness.get_by_label_contains("a.txt").click();
    harness
        .get_by_label("Where they were, replacing what's there now")
        .click();
    harness.run();

    harness.get_by_label("Restore 1 item").click();
    harness.run();
    harness.get_by_label("Replace the files where they were?");
    harness
        .query_all_by_label("Cancel")
        .last()
        .expect("the window's Cancel")
        .click();
    harness.run();
    assert!(
        fake.restores.lock().unwrap().is_empty(),
        "cancel restores nothing"
    );

    harness.get_by_label("Restore 1 item").click();
    harness.run();
    harness.get_by_label("Replace them").click();
    step_until(&mut harness, |h| h.state().done.is_some());
    assert_eq!(
        fake.restores.lock().unwrap()[0].to,
        RestoreTo::Original,
        "only after the yes"
    );
}

#[test]
fn a_test_restore_shows_which_file_differs() {
    let (fake, _dir) = restore_backend();
    let mut harness = restore_harness(&fake);
    // The snapshots load on a thread, with a spinner meanwhile.
    step_until(&mut harness, |h| {
        h.query_by_label_contains("abcdef01").is_some()
    });
    harness.get_by_label("Test restore").click();
    step_until(&mut harness, |h| h.state().done.is_some());
    assert!(matches!(harness.state().done, Some(Done::Tested(_))));
    harness.get_by_label_contains("differs from its source");
    harness.get_by_label("/C/Work/broken.txt");
    assert_eq!(fake.tests.lock().unwrap().len(), 1);
}

#[test]
fn history_reaches_runs_beyond_the_first_page() {
    let (fake, _dir) = backend(vec![]);
    let page = replica::ui::history::PAGE as i64;
    // A page and a half of newer runs, then one old failure of a job not run since.
    let mut runs: Vec<RunRecord> = (1..=page + page / 2)
        .map(|i| run_record(i, "docs", Outcome::Ok))
        .collect();
    runs.push(run_record(page * 3, "archive", Outcome::Failed));
    *fake.runs.lock().unwrap() = runs;
    let mut harness = history_harness(&fake);
    harness.run();
    assert_eq!(harness.state().shown().len(), page as usize);
    assert!(harness.state().has_older());
    assert!(harness.query_by_label_contains("archive").is_none());

    // Filtering asks the history itself: the old job is there to pick, and its run is found.
    harness.state_mut().job = Some("archive".into());
    harness.state_mut().load();
    harness.run();
    assert_eq!(harness.state().shown().len(), 1);
    harness.get_by_label_contains("archive");

    // So is paging back through everything.
    harness.state_mut().job = None;
    harness.state_mut().load();
    harness.run();
    // At the end of a long list: pressed as a screen reader would, without scrolling to it.
    harness.get_by_label("Show older").click_accesskit();
    harness.run();
    assert_eq!(
        harness.state().shown().len(),
        (page + page / 2 + 1) as usize
    );
    assert!(!harness.state().has_older());
    harness.get_by_label_contains("archive");
}

#[test]
fn history_offers_every_job_in_it_to_filter_by() {
    let (fake, _dir) = backend(vec![]);
    let page = replica::ui::history::PAGE as i64;
    let mut runs: Vec<RunRecord> = (1..=page)
        .map(|i| run_record(i, "docs", Outcome::Ok))
        .collect();
    runs.push(run_record(page * 3, "archive", Outcome::Failed));
    *fake.runs.lock().unwrap() = runs;
    let mut harness = history_harness(&fake);
    harness.run();
    // The job picker lists "archive" though none of its runs is on the first page.
    harness.get_by_label("Job").click();
    harness.run();
    harness.get_by_label("archive").click();
    harness.run();
    assert_eq!(harness.state().job.as_deref(), Some("archive"));
    assert_eq!(harness.state().shown().len(), 1);
}

// --- About --------------------------------------------------------------------------------

#[test]
fn about_shows_the_version_licence_notices_and_project_page() {
    let (fake, _dir) = backend(vec![]);
    let mut harness = Harness::builder()
        .with_size([1000.0, 1400.0])
        .with_context_setup(setup)
        .build_ui_state(
            |ui, tab: &mut replica::ui::about::AboutTab| tab.ui(ui),
            replica::ui::about::AboutTab::new(Arc::clone(&fake) as Arc<dyn Backend>),
        );
    harness.run();
    harness.get_by_label_contains(env!("CARGO_PKG_VERSION"));
    harness.check_accessibility();

    harness.get_by_label("The MIT licence").click();
    harness.run();
    harness.get_by_label_contains("Permission is hereby granted");

    harness.get_by_label("Third-party notices").click();
    harness.run();
    // restic's section, the approved exception, is always there.
    harness.get_by_label_contains("golang-lru");

    harness
        .get_by_label(replica::ui::about::PROJECT_URL)
        .click();
    harness.run();
    assert_eq!(
        *fake.opened.lock().unwrap(),
        [PathBuf::from(replica::ui::about::PROJECT_URL)]
    );
}

// --- Settings (the parts that need no Task Scheduler) ---------------------------------------

#[test]
fn dragging_the_log_days_saves_the_value_let_go_at() {
    let (fake, _dir) = backend(vec![]);
    let mut harness = Harness::builder()
        .with_size([1300.0, 1800.0])
        .with_context_setup(setup)
        .build_ui_state(
            |ui, tab: &mut replica::ui::settings::SettingsTab| tab.ui(ui),
            replica::ui::settings::SettingsTab::new(Arc::clone(&fake) as Arc<dyn Backend>),
        );
    harness.run_steps(3);
    harness
        .get_by_role_and_label(eframe::egui::accesskit::Role::Tab, "General")
        .click();
    harness.run_steps(3);
    let saved = || {
        Settings::load(&fake.settings_file())
            .unwrap()
            .settings
            .app
            .log_retention_days
    };
    assert_eq!(saved(), 30);
    let start = harness
        .get_by_label("Keep run logs for (days)")
        .rect()
        .center();
    // Press, move right in steps (frames run between them), let go.
    harness.hover_at(start);
    harness.step();
    harness.drag_at(start);
    harness.step();
    for dx in [10.0, 20.0, 30.0, 40.0] {
        harness.hover_at(start + eframe::egui::vec2(dx, 0.0));
        harness.step();
    }
    assert_eq!(saved(), 30, "not saved while it's dragged");
    harness.drop_at(start + eframe::egui::vec2(40.0, 0.0));
    harness.run_steps(3);
    let after = saved();
    assert!(after > 30, "the dragged value is saved: {after}");
    assert_eq!(
        harness
            .get_by_label("Keep run logs for (days)")
            .value()
            .as_deref(),
        Some(after.to_string().as_str()),
        "and shown"
    );
}

// --- Export… and Import… --------------------------------------------------------------

fn toggled(harness: &Harness<'_, JobsTab>, label: &str) -> bool {
    use egui_kittest::kittest::NodeT;
    harness.get_by_label(label).accesskit_node().toggled()
        == Some(eframe::egui::accesskit::Toggled::True)
}

fn disabled(harness: &Harness<'_, JobsTab>, label: &str) -> bool {
    use egui_kittest::kittest::NodeT;
    harness.get_by_label(label).accesskit_node().is_disabled()
}

/// The check box of a job in the Export dialog, as a screen reader names it.
fn export_box(name: &str, mode: &str) -> String {
    format!(
        "{name}, {mode}, to BACKUP:{}",
        if mode == "Mirror" {
            format!("Mirror-{name}")
        } else {
            "Replica".into()
        }
    )
}

#[test]
fn export_chooses_jobs_with_check_boxes_and_writes_the_file() {
    let (fake, dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("music", Mode::Mirror),
        job("photos", Mode::Versioned),
    ]);
    let mut harness = jobs_harness(&fake);
    harness.run();

    // Nothing selected in the table: every job is chosen.
    harness.get_by_label("Export…").click_accesskit();
    harness.run();
    harness.get_by_label("3 of 3 selected");
    let docs = export_box("docs", "Versioned");
    assert!(toggled(&harness, &docs));

    // The filter narrows the list; Select none and Select all act on what it shows.
    harness.get_by_label("Filter the jobs").focus();
    harness.get_by_label("Filter the jobs").type_text("MU");
    harness.run();
    assert!(harness.query_by_label(&docs).is_none(), "filtered out");
    harness.get_by_label("Select none").click_accesskit();
    harness.run();
    harness.get_by_label("2 of 3 selected");
    harness.get_by_label("Filter the jobs").focus();
    harness.get_by_label("Filter the jobs").type_text("zz");
    harness.run();
    harness.get_by_label("No job matches \"MUzz\".");
    harness.get_by_label("Filter the jobs").focus();
    for _ in 0.."MUzz".len() {
        harness.key_press(eframe::egui::Key::Backspace);
    }
    harness.run();
    harness.get_by_label("Select none").click_accesskit();
    harness.run();
    harness.get_by_label("0 of 3 selected");
    assert!(disabled(&harness, "Export 0 jobs"), "nothing to export");
    harness.get_by_label("Select all").click_accesskit();
    harness.run();
    harness.get_by_label("3 of 3 selected");
    harness
        .get_by_label(&export_box("music", "Mirror"))
        .click_accesskit();
    harness.run();
    harness.get_by_label("2 of 3 selected");

    // Export asks where (the Save dialog), writes the file and says so.
    let file = dir.path().join("out.replica-jobs.toml");
    *fake.jobs_file.lock().unwrap() = Some(file.clone());
    harness.get_by_label("Export 2 jobs").click_accesskit();
    harness.run();
    let written = replica_core::jobs_file::read(&file).unwrap();
    let names: Vec<&str> = written.entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(names, ["docs", "photos"]);
    harness.get_by_label(&format!("Exported 2 jobs to {}.", file.display()));
    assert!(harness.query_by_label("Export jobs").is_none(), "closed");
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains("password =")
    );
}

#[test]
fn export_from_a_job_s_menu_or_with_a_row_selected_chooses_that_job() {
    let (fake, _dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("music", Mode::Mirror),
    ]);
    let mut harness = jobs_harness(&fake);
    harness.run();
    harness
        .get_by_label("More actions for music")
        .click_accesskit();
    harness.run();
    harness.get_by_label("Export music…").click_accesskit();
    harness.run();
    harness.get_by_label("1 of 2 selected");
    assert!(toggled(&harness, &export_box("music", "Mirror")));
    assert!(!toggled(&harness, &export_box("docs", "Versioned")));
    harness.get_by_label("Cancel").click_accesskit();
    harness.run();
    assert!(harness.query_by_label("Export jobs").is_none());

    // A row selected in the table is the one chosen.
    harness
        .get_by_role_and_label(eframe::egui::accesskit::Role::ListBoxOption, "docs")
        .click_accesskit();
    harness.run();
    harness.get_by_label("Export…").click_accesskit();
    harness.run();
    harness.get_by_label("1 of 2 selected");
    assert!(toggled(&harness, &export_box("docs", "Versioned")));
}

/// A jobs file with a job of every status against [`backend`]'s docs and music: photos (new),
/// docs (its name taken), offsite (its drive not here, no password) and one that's invalid.
fn every_status_file(fake: &Fake, dir: &Path) -> PathBuf {
    let photos = job("photos", Mode::Versioned);
    // photos' repository has its password here already.
    let secret = replica_core::platform::Credential {
        username: None,
        secret: Secret::new("x"),
    };
    fake.secrets
        .set(&password_secret_name(&photos.targets[0]), &secret)
        .unwrap();
    let docs = Job {
        sources: vec![Source {
            path: r"D:\Elsewhere".into(),
            exclude: vec![],
        }],
        ..job("docs", Mode::Versioned)
    };
    let mut offsite = job("offsite", Mode::Versioned);
    offsite.targets = vec![drive("ELSEWHERE")];
    offsite.before.push(replica_core::job::BeforeStep::Command {
        cmd: "net stop app".into(),
        timeout_secs: 60,
    });
    let text = format!(
        "{}\n[[jobs]]\nname = \"bad name\"\n",
        replica_core::jobs_file::to_toml(&[photos, docs, offsite])
    );
    let path = dir.join("in.replica-jobs.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn import_previews_every_job_s_status_and_imports_the_chosen_turned_off() {
    let (fake, dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("music", Mode::Mirror),
    ]);
    let file = every_status_file(&fake, dir.path());
    *fake.jobs_file.lock().unwrap() = Some(file);
    let mut harness = jobs_harness(&fake);
    harness.run();
    harness.get_by_label("Import…").click_accesskit();
    harness.run();
    harness.get_by_label("Import jobs");

    // Each job's badges.
    for badge in [
        "New",
        "Name exists",
        "Needs a password",
        "Drive not found",
        "Invalid",
    ] {
        assert!(
            harness.query_all_by_label(badge).next().is_some(),
            "{badge}"
        );
    }
    assert!(toggled(&harness, "Import photos"));
    assert!(toggled(&harness, "Import docs"));
    assert!(toggled(&harness, "Import offsite"));
    assert!(
        !toggled(&harness, "Import bad name"),
        "an invalid job isn't chosen"
    );
    assert!(disabled(&harness, "Import bad name"), "and can't be");
    assert!(
        harness
            .query_by_label_contains("may only use letters")
            .is_some()
    );
    harness.get_by_label("Runs: net stop app");
    // docs is skipped by default.
    harness.get_by_label("Import 2 jobs");

    choose_by_accesskit(
        &mut harness,
        "What to do with docs, whose name is taken",
        "Import as a copy",
        "Replace",
    );
    harness.get_by_label("Imports as docs-2.");
    harness.get_by_label("Import 3 jobs");
    harness.get_by_label("Import offsite").click_accesskit();
    harness.run();
    assert!(!toggled(&harness, "Import offsite"));
    harness.get_by_label("Import 2 jobs").click_accesskit();
    harness.run();

    let jobs = saved_jobs(&fake);
    let names: Vec<(&str, bool)> = jobs.iter().map(|j| (j.name.as_str(), j.enabled)).collect();
    assert_eq!(
        names,
        [
            ("docs", true),
            ("music", true),
            ("photos", false),
            ("docs-2", false)
        ],
        "imported turned off"
    );
    assert_eq!(
        fake.requests.lock().unwrap().last(),
        Some(&UiRequest::Reload)
    );
    assert!(harness.query_by_label("Import jobs").is_none(), "closed");
    harness.get_by_label_contains("Imported 2 jobs. Imported jobs are disabled");
    use egui_kittest::kittest::NodeT;
    assert!(
        harness
            .get_by_role_and_label(eframe::egui::accesskit::Role::ListBoxOption, "photos")
            .accesskit_node()
            .is_selected()
            == Some(true),
        "the first imported job is selected"
    );
}

#[test]
fn replacing_a_job_keeps_its_name_and_a_refused_file_imports_nothing() {
    let (fake, dir) = backend(vec![
        job("Docs", Mode::Versioned),
        job("music", Mode::Mirror),
    ]);
    let file = every_status_file(&fake, dir.path());
    *fake.jobs_file.lock().unwrap() = Some(file);
    let mut harness = jobs_harness(&fake);
    harness.run();
    harness.get_by_label("Import…").click_accesskit();
    harness.run();
    for job in ["photos", "offsite"] {
        harness
            .get_by_label(&format!("Import {job}"))
            .click_accesskit();
        harness.run();
    }
    harness
        .get_by_label("What to do with docs, whose name is taken")
        .click_accesskit();
    harness.run();
    harness.get_by_label("Replace").click_accesskit();
    harness.run();
    harness.get_by_label("Replaces Docs here, which keeps its history.");
    harness.get_by_label("Import 1 job").click_accesskit();
    harness.run();
    let jobs = saved_jobs(&fake);
    assert_eq!(jobs[0].name, "Docs", "the name here, so its history stays");
    assert_eq!(jobs[0].sources[0].path, Path::new(r"D:\Elsewhere"));
    assert!(!jobs[0].enabled);
    assert_eq!(jobs.len(), 2);

    // settings.toml isn't a jobs file: the reason, and nothing changes.
    let before = std::fs::read_to_string(fake.settings_file()).unwrap();
    *fake.jobs_file.lock().unwrap() = Some(fake.settings_file());
    harness.get_by_label("Import…").click_accesskit();
    harness.run();
    assert!(
        harness.query_by_label("Import jobs").is_none(),
        "no preview"
    );
    harness.get_by_label_contains(
        "can't be imported, and nothing was: This isn't a file of Replica jobs",
    );
    assert_eq!(
        std::fs::read_to_string(fake.settings_file()).unwrap(),
        before
    );

    // Cancel imports nothing either.
    let file = every_status_file(&fake, dir.path());
    *fake.jobs_file.lock().unwrap() = Some(file);
    harness.get_by_label("Import…").click_accesskit();
    harness.run();
    harness.get_by_label("Cancel").click_accesskit();
    harness.run();
    assert_eq!(
        std::fs::read_to_string(fake.settings_file()).unwrap(),
        before
    );
}

/// A copy of a mirror would be a second mirror over its folder: invalid, but only as that
/// choice has it, so the choice stays to change to Replace (round 1's finding).
#[test]
fn a_copy_that_s_invalid_can_still_be_changed_to_replace() {
    let (fake, dir) = backend(vec![
        job("docs", Mode::Versioned),
        job("music", Mode::Mirror),
    ]);
    let theirs = Job {
        notify_on_success: true,
        ..job("music", Mode::Mirror)
    };
    let file = dir.path().join("music.replica-jobs.toml");
    std::fs::write(&file, replica_core::jobs_file::to_toml(&[theirs])).unwrap();
    *fake.jobs_file.lock().unwrap() = Some(file);
    let mut harness = jobs_harness(&fake);
    harness.run();
    harness.get_by_label("Import…").click_accesskit();
    harness.run();
    let choice = "What to do with music, whose name is taken";
    harness.get_by_label(choice).click_accesskit();
    harness.run();
    harness.get_by_label("Import as a copy").click_accesskit();
    harness.run();
    harness.get_by_label("Invalid");
    harness.get_by_label("Name exists");
    assert!(
        harness
            .query_by_label_contains("delete the other's files")
            .is_some()
    );
    assert!(
        !disabled(&harness, "Import music"),
        "still chosen, to choose again"
    );
    harness.get_by_label("Import 0 jobs");
    assert!(
        harness.query_by_label("Replace").is_none(),
        "the list closes once a choice is made, however it's made"
    );

    harness.get_by_label(choice).click_accesskit();
    harness.run();
    harness.get_by_label("Replace").click_accesskit();
    harness.run();
    harness.get_by_label("Replaces music here, which keeps its history.");
    harness.get_by_label("Import 1 job").click_accesskit();
    harness.run();
    let jobs = saved_jobs(&fake);
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[1].name, "music", "its name, so its history stays");
    assert!(jobs[1].notify_on_success, "the file's job");
    assert!(!jobs[1].enabled, "turned off");
}

/// Chooses `choice` in the dropdown named `name` as a screen reader does, through AccessKit
///: its list closes, so `other`, a choice not chosen, is gone; and the next press opens
/// it again rather than closing it.
fn choose_by_accesskit<S>(harness: &mut Harness<'_, S>, name: &str, choice: &str, other: &str) {
    use eframe::egui::accesskit::Role;
    let open = |harness: &Harness<'_, S>| eframe::egui::Popup::is_any_open(&harness.ctx);
    harness
        .get_by_role_and_label(Role::ComboBox, name)
        .click_accesskit();
    harness.run();
    assert!(open(harness), "{name}: opens");
    harness.get_by_label(choice).click_accesskit();
    harness.run();
    assert!(!open(harness), "{name}: closed once {choice} is chosen");
    assert!(
        harness.query_by_label(other).is_none(),
        "{name}: {other} is gone with its list"
    );
    harness
        .get_by_role_and_label(Role::ComboBox, name)
        .click_accesskit();
    harness.run();
    assert!(open(harness), "{name}: opens again");
    harness.get_by_label(other);
    // Choosing what's chosen closes it too.
    harness.get_by_label(choice).click_accesskit();
    harness.run();
    assert!(!open(harness), "{name}: closed again");
}

#[test]
fn history_s_dropdowns_close_on_a_screen_reader_s_choice() {
    let (fake, _dir) = backend(vec![]);
    *fake.runs.lock().unwrap() = vec![
        run_record(1, "docs", Outcome::Ok),
        run_record(2, "music", Outcome::Ok),
    ];
    let mut harness = history_harness(&fake);
    harness.run();
    choose_by_accesskit(&mut harness, "When", "The last 7 days", "The last 30 days");
    assert_eq!(harness.state().shown().len(), 2);
    choose_by_accesskit(&mut harness, "Result", "Failed", "Skipped");
    assert_eq!(harness.state().result, Some(Outcome::Failed));
    harness.get_by_label("Clear filters").click_accesskit();
    harness.run();
    choose_by_accesskit(&mut harness, "Job", "All jobs", "music");
    assert_eq!(harness.state().job, None);
}

#[test]
fn restore_s_pickers_close_on_a_screen_reader_s_choice() {
    let (fake, _dir) = restore_backend();
    let mut docs = job("docs", Mode::Versioned);
    docs.sources[0].path = r"C:\Work".into();
    docs.targets.push(drive("SPARE"));
    Settings {
        jobs: vec![docs, job("music", Mode::Versioned)],
        ..Settings::default()
    }
    .save(&fake.settings_file())
    .unwrap();
    let mut harness = restore_harness(&fake);
    harness.run();
    let (first, second) = (
        drive("BACKUP").display_name(),
        drive("SPARE").display_name(),
    );
    choose_by_accesskit(&mut harness, "Backup", &second, &first);
    choose_by_accesskit(&mut harness, "Job", "music", "docs");
    assert_eq!(harness.state().job.as_deref(), Some("music"));
}

#[test]
fn the_editor_s_dropdowns_close_on_a_screen_reader_s_choice() {
    use eframe::egui::accesskit::Role;
    let (fake, _dir) = backend(vec![job("docs", Mode::Versioned)]);
    let mut harness = editor_harness(&fake, Some(job("docs", Mode::Versioned)));
    harness.run();
    choose_by_accesskit(&mut harness, "Runs", "At an interval", "Every day");
    choose_by_accesskit(&mut harness, "Hours or minutes", "minutes", "hours");

    // The drive: the one volume the fake has, so it's checked by hand.
    let open = |harness: &Harness<'_, _>| eframe::egui::Popup::is_any_open(&harness.ctx);
    harness
        .get_by_role_and_label(Role::ComboBox, "Drive")
        .click_accesskit();
    harness.run();
    harness
        .get_by_label_contains("BACKUP (E:)")
        .click_accesskit();
    harness.run();
    assert!(!open(&harness), "the drive's list closes");
    assert!(harness.query_by_label_contains("BACKUP (E:)").is_none());
    harness
        .get_by_role_and_label(Role::ComboBox, "Drive")
        .click_accesskit();
    harness.run();
    assert!(open(&harness), "and opens again");
    harness.get_by_label_contains("BACKUP (E:)");
}

// --- Every frame's tree as AccessKit takes it ---------------------------------------------

/// Asserts that every node's name can be worked out from the tree as this frame left it.
/// AccessKit's Windows adapter names each node as it arrives, and a `labelled_by` naming a
/// node that isn't there panics it, on the window's thread: the window is gone. Only
/// with a screen reader or any other UI Automation client running, so check each frame.
fn assert_names_resolve<S>(harness: &Harness<'_, S>, when: &str) {
    use egui_kittest::kittest::{AccessKitNode, NodeT};
    fn walk(node: AccessKitNode<'_>, bad: &mut Vec<String>) {
        let named = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| node.label()));
        if named.is_err() {
            bad.push(format!(
                "{:?} labelled by {:?}, not in the tree",
                node.role(),
                node.data().labelled_by()
            ));
        }
        for child in node.children() {
            walk(child, bad);
        }
    }
    let mut bad = Vec::new();
    walk(harness.root().accesskit_node(), &mut bad);
    assert!(bad.is_empty(), "{when}: {bad:?}");
}

/// Runs frames one at a time until a label with `label` in it shows, checking each one's tree.
fn step_checking_names_until<S>(harness: &mut Harness<'_, S>, label: &str, when: &str) {
    for frame in 0..64 {
        harness.step();
        assert_names_resolve(harness, &format!("{when}, frame {frame}"));
        if harness.query_by_label_contains(label).is_some() {
            return;
        }
    }
    panic!("{when}: {label} never showed");
}

/// A file like a real user's: one turned-off versioned job to a NAS folder with a share
/// login's name.
const NAS_JOB_FILE: &str = r#"format = "replica-jobs"
version = 1
replica_version = "0.1.0"

[[jobs]]
name = "backup-nas-workspace-misc"
enabled = false
mode = "versioned"
notify_on_success = true

[[jobs.sources]]
path = 'D:\Work\Misc'
exclude = ["node_modules", "**/.godot", "dist", "out", "*.tmp"]

[[jobs.targets]]
kind = "unc"
name = "NAS-ARCHIVE-LIVE"
path = '\\nas\archive\LIVE'
credential = 'share:\\nas\archive'
"#;

#[test]
fn importing_a_nas_job_previews_and_imports_with_every_frame_s_names_resolving() {
    let (fake, dir) = backend(vec![]);
    let file = dir.path().join("replica.replica-jobs.toml");
    std::fs::write(&file, NAS_JOB_FILE).unwrap();
    *fake.jobs_file.lock().unwrap() = Some(file);
    let mut harness = jobs_harness(&fake);
    harness.run();
    assert_names_resolve(&harness, "the Jobs page");

    // The preview's first frames: a new dialog's first is laid out unseen, its message bar
    // with it (the round 1 cause).
    harness.get_by_label("Import…").click_accesskit();
    step_checking_names_until(&mut harness, "Import 1 job", "the preview opening");
    harness.run();
    harness.get_by_label("Import jobs");
    harness.get_by_label("Needs a password");
    assert!(toggled(&harness, "Import backup-nas-workspace-misc"));
    harness.get_by_label("Import 1 job").click_accesskit();
    step_checking_names_until(
        &mut harness,
        "Imported 1 job. Imported jobs are disabled",
        "the import done",
    );
    harness.run();
    let jobs = saved_jobs(&fake);
    assert_eq!(jobs.len(), 1);
    assert!(!jobs[0].enabled, "turned off");
    assert!(
        matches!(&jobs[0].targets[0], Target::Unc { credential: Some(c), .. } if c == r"share:\\nas\archive"),
        "{:?}",
        jobs[0].targets
    );

    // Again, over the job it made: its name is taken.
    harness.get_by_label("Import…").click_accesskit();
    step_checking_names_until(&mut harness, "Name exists", "the second preview opening");
    harness.run();
    harness.get_by_label("Import 0 jobs");
}
