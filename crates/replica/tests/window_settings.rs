//! The Settings tab and its Scheduled-tasks view, driven through egui's accessibility tree with
//! `egui_kittest`, against the real backend: a temp app folder, and the real Task Scheduler in
//! a test-only `\ReplicaTest-<pid>-<time>-<test>` folder, emptied and removed when each test
//! ends, pass or fail. The tasks point at an exe that doesn't exist and are never run.

#![cfg(windows)]

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use replica::ui::backend::{Backend, LiveBackend, UiRequest};
use replica::ui::settings::{Confirm, SettingsTab};
use replica_core::job::{Job, Source, Target, VolumeRef};
use replica_core::settings::{ScheduleMode, Settings};
use replica_win::credentials::CredentialManager;
use replica_win::paths::AppPaths;
use replica_win::scheduler::Scheduler;

/// A test-only task folder, `\ReplicaTest-<pid>-<time>-<test>`, emptied and removed when
/// dropped. Made before any task is, so a setup that fails part-way still cleans up.
struct TaskFolder(Scheduler);

impl TaskFolder {
    fn new(test: &str) -> TaskFolder {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        TaskFolder(Scheduler::in_folder(&format!(
            r"\ReplicaTest-{}-{nanos}-{test}",
            std::process::id()
        )))
    }
}

impl Drop for TaskFolder {
    fn drop(&mut self) {
        if let Err(error) = self.0.delete_all() {
            eprintln!("test tasks left behind in {}: {error}", self.0.folder());
        }
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    backend: Arc<LiveBackend>,
    requests: Mutex<Receiver<UiRequest>>,
    /// Last, so it's dropped after the backend that uses its folder.
    tasks: TaskFolder,
}

fn job(name: &str, schedule: &str) -> Job {
    Job {
        name: name.into(),
        schedule: Some(schedule.parse().unwrap()),
        sources: vec![Source {
            path: r"C:\Work".into(),
            exclude: vec![],
        }],
        targets: vec![Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: None,
                label: Some("BACKUP".into()),
            },
            path: "Replica".into(),
        }],
        ..Job::default()
    }
}

impl Fixture {
    /// Jobs "docs" and "music" with Task Scheduler on and a task each; then "music" is
    /// deleted from the settings, so its task is an orphan.
    fn new(test: &str) -> Fixture {
        // The cleanup first: everything below may fail after a task is made.
        let tasks = TaskFolder::new(test);
        let scheduler = tasks.0.clone();
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::resolve(
            "Replica",
            &temp.path().join("exe"),
            Some(&temp.path().join("appdata")),
        )
        .unwrap();
        let (send, requests) = channel();
        let backend = Arc::new(LiveBackend::with(
            paths,
            send,
            Arc::default(),
            CredentialManager::with_prefix(&format!(
                "ReplicaTest-{}{}/",
                std::process::id(),
                scheduler.folder().trim_start_matches(r"\ReplicaTest")
            )),
            scheduler.clone(),
        ));
        let mut settings = Settings {
            jobs: vec![job("docs", "daily 02:30"), job("music", "every 6h")],
            ..Settings::default()
        };
        settings.app.schedule_mode = ScheduleMode::TaskScheduler;
        let exe = temp.path().join("not-run").join("replica-tray.exe");
        scheduler.sync(&settings.jobs, &exe).unwrap();
        settings.jobs.truncate(1);
        settings.save(&backend.settings_file()).unwrap();
        Fixture {
            temp,
            backend,
            requests: Mutex::new(requests),
            tasks,
        }
    }

    fn settings(&self) -> Settings {
        Settings::load(&self.backend.settings_file())
            .unwrap()
            .settings
    }

    fn tasks(&self) -> Vec<(String, bool)> {
        self.tasks
            .0
            .list(&[])
            .unwrap()
            .into_iter()
            .map(|t| (t.name, t.enabled))
            .collect()
    }

    fn reloads(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .try_iter()
            .filter(|r| *r == UiRequest::Reload)
            .count()
    }

    fn harness(&self) -> Harness<'static, SettingsTab> {
        Harness::builder()
            .with_size([1300.0, 2200.0])
            .with_context_setup(|ctx| {
                replica::ui::fonts::install(ctx).unwrap();
                replica::ui::style(ctx);
            })
            .build_ui_state(
                |ui, tab: &mut SettingsTab| tab.ui(ui),
                SettingsTab::new(Arc::clone(&self.backend) as Arc<dyn Backend>),
            )
    }
}

/// Shows Settings' tab `name`.
fn pick_tab(harness: &mut Harness<'_, SettingsTab>, name: &str) {
    harness
        .get_by_role_and_label(egui::accesskit::Role::Tab, name)
        .click();
    harness.run_steps(3);
}

/// Steps until `done` (a setting saved, the restic version found on a thread), for at most 30 s:
/// under load a few frames can pass before either lands.
fn step_until<S>(harness: &mut Harness<'_, S>, mut done: impl FnMut(&Harness<'_, S>) -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done(harness) {
        assert!(Instant::now() < until, "timed out");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_tasks_view_lists_flags_orphans_turns_off_and_deletes() {
    let f = Fixture::new("view");
    let mut harness = f.harness();
    harness.run_steps(3);
    pick_tab(&mut harness, "Schedules");
    // "docs": the task, and its job.
    assert_eq!(harness.query_all_by_label("docs").count(), 2);
    // "music": its task only, flagged as an orphan.
    assert_eq!(harness.query_all_by_label("music").count(), 1);
    harness.get_by_label("No such job any more");
    harness.check_accessibility();

    harness.get_by_label("Turn off the task docs").click();
    harness.run_steps(3);
    assert_eq!(f.tasks(), [("docs".into(), false), ("music".into(), true)]);
    harness.get_by_label("Turn on the task docs").click();
    harness.run_steps(3);
    assert_eq!(f.tasks()[0], ("docs".into(), true));

    harness.get_by_label("Delete the task music").click();
    harness.run_steps(3);
    harness.get_by_label("Delete the task music?");
    harness
        .query_all_by_label("Cancel")
        .last()
        .expect("the window's Cancel")
        .click();
    harness.run_steps(3);
    assert_eq!(f.tasks().len(), 2, "cancel deletes nothing");
    harness.get_by_label("Delete the task music").click();
    harness.run_steps(3);
    harness
        .query_all_by_label("Delete")
        .last()
        .expect("the window's Delete")
        .click();
    harness.run_steps(3);
    assert_eq!(f.tasks(), [("docs".into(), true)]);

    harness.get_by_label("Remove all Replica tasks").click();
    harness.run_steps(3);
    harness.get_by_label("Remove all of Replica's tasks?");
    harness
        .query_all_by_label("Remove all Replica tasks")
        .last()
        .unwrap()
        .click();
    harness.run_steps(3);
    assert!(f.tasks().is_empty());
    harness.get_by_label("Replica has no tasks in Task Scheduler.");
}

#[test]
fn turning_task_scheduler_off_deletes_its_tasks_unless_kept() {
    for keep in [false, true] {
        let f = Fixture::new(if keep { "keep" } else { "delete" });
        let mut harness = f.harness();
        harness.run_steps(3);
        pick_tab(&mut harness, "Schedules");
        let _ = f.reloads();
        harness.get_by_label_contains("Built-in timer").click();
        harness.run_steps(3);
        assert_eq!(harness.state().confirm, Some(Confirm::TurnOff));
        assert_eq!(
            f.settings().app.schedule_mode,
            ScheduleMode::TaskScheduler,
            "nothing changes before the answer"
        );
        let answer = if keep {
            "Keep them, turned off"
        } else {
            "Delete the tasks"
        };
        harness.get_by_label(answer).click();
        harness.run_steps(3);
        assert_eq!(f.settings().app.schedule_mode, ScheduleMode::Timer);
        assert_eq!(f.reloads(), 1, "the tray is told");
        if keep {
            assert_eq!(f.tasks(), [("docs".into(), false), ("music".into(), false)]);
        } else {
            assert!(f.tasks().is_empty());
        }
    }
}

#[test]
fn every_setting_round_trips_and_reset_keeps_the_jobs() {
    let f = Fixture::new("settings");
    let mut harness = f.harness();
    harness.run_steps(3);

    pick_tab(&mut harness, "Schedules");
    harness.get_by_label("Pause schedules").click();
    step_until(&mut harness, |_| f.settings().app.pause_schedules);
    pick_tab(&mut harness, "General");
    harness.get_by_label("Start with Windows").click();
    step_until(&mut harness, |_| f.settings().app.start_with_windows);

    pick_tab(&mut harness, "restic");
    // The restic in use, found off the UI thread.
    step_until(&mut harness, |h| {
        h.query_by_label_contains("In use:").is_some()
    });
    let restic = f.temp.path().join("other").join("restic.exe");
    harness
        .get_by_label("restic.exe")
        .set_text_value_accesskit(&restic.to_string_lossy());
    harness.run_steps(3);
    // The restic in use is looked for again on a thread whenever the path changes, and what it
    // finds changes the card's height: these buttons are pressed through AccessKit, as a
    // screen reader does, so a frame that moves them can't make the press miss.
    harness.get_by_label("Save").click_accesskit();
    step_until(&mut harness, |_| {
        f.settings().app.restic_path.as_ref() == Some(&restic)
    });
    harness
        .get_by_label("Use the one that comes with Replica")
        .click_accesskit();
    step_until(&mut harness, |_| f.settings().app.restic_path.is_none());

    pick_tab(&mut harness, "General");
    harness
        .get_by_label("Keep run logs for (days)")
        .set_value_accesskit(45.0);
    step_until(&mut harness, |_| f.settings().app.log_retention_days == 45);

    // Where the files are, as the app's paths say.
    pick_tab(&mut harness, "Storage");
    let places = f.backend.places();
    harness.get_by_label(&places.settings.display().to_string());
    harness.get_by_label("Installed");

    pick_tab(&mut harness, "General");
    harness.get_by_label("Reset settings").click();
    harness.run_steps(3);
    harness.get_by_label("Reset Replica's settings?");
    harness
        .query_all_by_label("Reset settings")
        .last()
        .unwrap()
        .click();
    step_until(&mut harness, |_| f.settings().app == Default::default());
    let settings = f.settings();
    assert_eq!(settings.jobs.len(), 1, "the jobs stay");
    assert!(f.tasks().is_empty(), "Task Scheduler was on: its tasks go");
    harness.check_accessibility();
}

#[test]
fn a_setup_that_fails_after_making_tasks_still_removes_them() {
    let made: Mutex<Option<String>> = Mutex::new(None);
    let result = std::panic::catch_unwind(|| {
        let tasks = TaskFolder::new("setup-panic");
        *made.lock().unwrap() = Some(tasks.0.folder().to_owned());
        let exe = std::env::temp_dir()
            .join("replica-test-not-run")
            .join("replica-tray.exe");
        tasks.0.sync(&[job("docs", "daily 02:30")], &exe).unwrap();
        assert!(tasks.0.folder_exists().unwrap());
        panic!("the setup fails after a task is registered");
    });
    assert!(result.is_err());
    let folder = made.into_inner().unwrap().expect("a folder was made");
    assert!(
        !Scheduler::in_folder(&folder).folder_exists().unwrap(),
        "{folder} is removed"
    );
}
