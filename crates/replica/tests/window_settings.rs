//! The Settings page and the Schedules page (Replica's tasks), driven through egui's accessibility tree with
//! `egui_kittest`, against the real backend: a temp app folder, and the real Task Scheduler in
//! a test-only `\ReplicaTest-<pid>-<time>-<test>` folder, emptied and removed when each test
//! ends, pass or fail. The tasks point at an exe that doesn't exist and are never run.

#![cfg(windows)]

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use replica::ui::backend::{Backend, LiveBackend, UiRequest};
use replica::ui::schedules::SchedulesTab;
use replica::ui::settings::{Confirm, SettingsTab};
use replica_core::job::{Job, Source, Target, VolumeRef};
use replica_core::settings::{ScheduleMode, Settings};
use replica_win::paths::AppPaths;
use replica_win::scheduler::Scheduler;

#[path = "../../replica-win/tests/support/test_credentials.rs"]
mod test_credentials;

use test_credentials::TestCredentials;

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
    /// After the backend that uses its folder, so it's dropped after it.
    tasks: TaskFolder,
    /// Its Credential Manager prefix, emptied after the backend is gone.
    _credentials: TestCredentials,
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
        let credentials = TestCredentials::new(test);
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
            credentials.store(),
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
            _credentials: credentials,
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

    /// The Schedules page, `width` points wide.
    fn schedules(&self, width: f32) -> Harness<'static, SchedulesTab> {
        Harness::builder()
            .with_size([width, 1600.0])
            .with_context_setup(|ctx| {
                replica::ui::fonts::install(ctx).unwrap();
                replica::ui::style(ctx);
            })
            .build_ui_state(
                |ui, tab: &mut SchedulesTab| tab.ui(ui),
                SchedulesTab::new(Arc::clone(&self.backend) as Arc<dyn Backend>),
            )
    }
}

/// Shows Settings' tab `name`, and waits until it's the one selected.
fn pick_tab(harness: &mut Harness<'_, SettingsTab>, name: &str) {
    let tab = |h: &Harness<'_, SettingsTab>| {
        h.query_by_role_and_label(egui::accesskit::Role::Tab, name)
            .is_some_and(|t| t.accesskit_node().is_selected() == Some(true))
    };
    step_until(harness, |h| {
        h.query_by_role_and_label(egui::accesskit::Role::Tab, name)
            .is_some()
    });
    harness
        .get_by_role_and_label(egui::accesskit::Role::Tab, name)
        .click_accesskit();
    step_until(harness, tab);
}

/// Clicks the last control named `name` (a dialog's, over the page's own of that name) through
/// AccessKit, once it's there.
fn click_last<S>(harness: &mut Harness<'_, S>, name: &str) {
    step_until(harness, |h| h.query_all_by_label(name).last().is_some());
    harness
        .query_all_by_label(name)
        .last()
        .expect("found above")
        .click_accesskit();
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
fn a_narrow_window_scrolls_the_tasks_table_sideways_to_its_last_columns() {
    // Narrower than the table's columns, as the page is at the window's least width.
    const WIDTH: f32 = 600.0;
    let f = Fixture::new("narrow");
    let mut harness = f.schedules(WIDTH);
    // Each step waits for what it needs, never a fixed few frames: the tasks (read from Task
    // Scheduler) laid out.
    step_until(&mut harness, |h| {
        h.query_by_label("Delete the task docs").is_some()
            && h.query_by_label("daily 02:30").is_some()
    });
    settle(&mut harness);
    let delete = |h: &Harness<'_, SchedulesTab>| h.get_by_label("Delete the task docs").rect();
    assert!(
        delete(&harness).left() > WIDTH,
        "past the edge at first: {:?}",
        delete(&harness)
    );
    // A sideways wheel over the table (the harness's own input, not the screen's) brings the
    // last columns into view.
    let over = harness.get_by_label("daily 02:30").rect().center();
    harness.event(egui::Event::PointerMoved(over));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(-600.0, 0.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::NONE,
    });
    step_until(&mut harness, |h| delete(h).right() <= WIDTH);
    settle(&mut harness);
    harness.get_by_label("Delete the task docs").click();
    step_until(&mut harness, |h| {
        h.query_by_label("Delete the task docs?").is_some()
    });
}

/// Runs frames until nothing more is asked for (a scroll's last frames, a list laid out), for
/// at most 10 s: a condition, not a count of frames.
fn settle<S>(harness: &mut Harness<'_, S>) {
    let until = Instant::now() + Duration::from_secs(10);
    while harness.try_run().is_err() {
        assert!(Instant::now() < until, "never settled");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_schedules_page_lists_flags_orphans_turns_off_and_deletes() {
    let f = Fixture::new("view");
    let mut harness = f.schedules(1300.0);
    // The tasks, read from Task Scheduler, laid out.
    step_until(&mut harness, |h| {
        h.query_by_label("No such job any more").is_some()
            && h.query_by_label("Turn off the task docs").is_some()
    });
    settle(&mut harness);
    // Replica's tasks in Task Scheduler, and Windows' own a click away.
    harness.get_by_role_and_label(egui::accesskit::Role::Heading, "Task Scheduler tasks");
    harness.get_by_label("Open Task Scheduler");
    // "docs": the task, and its job.
    assert_eq!(harness.query_all_by_label("docs").count(), 2);
    // "music": its task only, flagged as an orphan.
    assert_eq!(harness.query_all_by_label("music").count(), 1);
    harness.get_by_label("No such job any more");
    harness.check_accessibility();

    // Turned off, and on again: Task Scheduler changes, and the row's switch says so.
    harness
        .get_by_label("Turn off the task docs")
        .click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("Turn on the task docs").is_some()
    });
    assert_eq!(f.tasks(), [("docs".into(), false), ("music".into(), true)]);
    harness
        .get_by_label("Turn on the task docs")
        .click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("Turn off the task docs").is_some()
    });
    assert_eq!(f.tasks()[0], ("docs".into(), true));

    // Delete asks first: Cancel deletes nothing.
    harness
        .get_by_label("Delete the task music")
        .click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("Delete the task music?").is_some()
    });
    click_last(&mut harness, "Cancel");
    step_until(&mut harness, |h| {
        h.query_by_label("Delete the task music?").is_none()
    });
    assert_eq!(f.tasks().len(), 2, "cancel deletes nothing");
    harness
        .get_by_label("Delete the task music")
        .click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("Delete the task music?").is_some()
    });
    click_last(&mut harness, "Delete");
    step_until(&mut harness, |h| {
        h.query_by_label("Delete the task music").is_none()
    });
    assert_eq!(f.tasks(), [("docs".into(), true)]);

    harness
        .get_by_label("Remove all Replica tasks")
        .click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("Remove all of Replica's tasks?").is_some()
    });
    click_last(&mut harness, "Remove all Replica tasks");
    step_until(&mut harness, |h| {
        h.query_by_label("Replica has no tasks in Task Scheduler.")
            .is_some()
    });
    assert!(f.tasks().is_empty());
}

#[test]
fn turning_task_scheduler_off_deletes_its_tasks_unless_kept() {
    for keep in [false, true] {
        let f = Fixture::new(if keep { "keep" } else { "delete" });
        let mut harness = f.harness();
        pick_tab(&mut harness, "Scheduling");
        let _ = f.reloads();
        harness
            .get_by_label_contains("Built-in timer")
            .click_accesskit();
        step_until(&mut harness, |h| {
            h.query_by_label("Turn Task Scheduler off?").is_some()
        });
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
        harness.get_by_label(answer).click_accesskit();
        step_until(&mut harness, |_| {
            f.settings().app.schedule_mode == ScheduleMode::Timer
        });
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

    pick_tab(&mut harness, "Scheduling");
    step_until(&mut harness, |h| {
        h.query_by_label("Pause schedules").is_some()
    });
    // The options only: Replica's tasks are on the Schedules page.
    assert!(harness.query_by_label("Task Scheduler tasks").is_none());
    assert!(harness.query_by_label("Remove all Replica tasks").is_none());
    assert!(harness.query_by_label("Open Task Scheduler").is_none());
    harness.get_by_label("Pause schedules").click_accesskit();
    step_until(&mut harness, |_| f.settings().app.pause_schedules);
    pick_tab(&mut harness, "General");
    step_until(&mut harness, |h| {
        h.query_by_label("Start with Windows").is_some()
    });
    harness.get_by_label("Start with Windows").click_accesskit();
    step_until(&mut harness, |_| f.settings().app.start_with_windows);
    // Off by default, beside Start with Windows.
    assert!(!f.settings().app.check_for_updates);
    harness.get_by_label("Check for updates").click();
    step_until(&mut harness, |_| f.settings().app.check_for_updates);

    pick_tab(&mut harness, "restic");
    // The restic in use, found off the UI thread.
    step_until(&mut harness, |h| {
        h.query_by_label_contains("In use:").is_some()
    });
    let restic = f.temp.path().join("other").join("restic.exe");
    harness
        .get_by_label("restic.exe")
        .set_text_value_accesskit(&restic.to_string_lossy());
    // Save turns on once the path typed differs from the one saved.
    step_until(&mut harness, |h| {
        h.query_by_label("Save")
            .is_some_and(|b| !b.accesskit_node().is_disabled())
    });
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
    step_until(&mut harness, |h| {
        h.query_by_label("Keep run logs for (days)").is_some()
    });
    harness
        .get_by_label("Keep run logs for (days)")
        .set_value_accesskit(45.0);
    step_until(&mut harness, |_| f.settings().app.log_retention_days == 45);

    // Where the files are, as the app's paths say.
    pick_tab(&mut harness, "Storage");
    let places = f.backend.places();
    let settings_file = places.settings.display().to_string();
    step_until(&mut harness, |h| h.query_by_label(&settings_file).is_some());
    harness.get_by_label("Installed");

    pick_tab(&mut harness, "General");
    step_until(&mut harness, |h| {
        h.query_by_label("Reset settings").is_some()
    });
    harness.get_by_label("Reset settings").click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("Reset Replica's settings?").is_some()
    });
    click_last(&mut harness, "Reset settings");
    step_until(&mut harness, |_| f.settings().app == Default::default());
    let settings = f.settings();
    assert_eq!(settings.jobs.len(), 1, "the jobs stay");
    assert!(f.tasks().is_empty(), "Task Scheduler was on: its tasks go");
    harness.check_accessibility();
    // Reset looks for the restic in use again, on a thread that runs restic: the test waits
    // for it to answer, so no restic outlives it (docs/TESTING.md, no process left running).
    pick_tab(&mut harness, "restic");
    step_until(&mut harness, |h| {
        h.query_by_label_contains("In use:").is_some()
    });
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

#[test]
fn a_schedule_saved_on_the_schedules_page_changes_its_task_as_the_editor_s_save_does() {
    use egui_kittest::kittest::NodeT;
    use replica::tray::runner::reconcile_tasks;
    let f = Fixture::new("schedule");
    let mut harness = f.schedules(1300.0);
    step_until(&mut harness, |h| h.query_by_label("Edit job…").is_some());
    settle(&mut harness);
    let _ = f.reloads();
    let docs = |f: &Fixture| {
        f.tasks
            .0
            .list(&["docs"])
            .unwrap()
            .into_iter()
            .find(|t| t.name == "docs")
            .and_then(|t| t.schedule)
    };
    let before = docs(&f);

    // Docs every 6 hours, rather than daily at 02:30.
    harness
        .get_by_role_and_label(egui::accesskit::Role::ComboBox, "Runs")
        .click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_label("At an interval").is_some()
    });
    harness.get_by_label("At an interval").click_accesskit();
    step_until(&mut harness, |h| {
        h.query_by_role_and_label(egui::accesskit::Role::ComboBox, "Runs")
            .is_some_and(|n| n.accesskit_node().value().as_deref() == Some("At an interval"))
    });
    harness.get_by_label("Save").click_accesskit();
    step_until(&mut harness, |_| {
        f.settings().jobs[0]
            .schedule
            .as_ref()
            .is_some_and(|s| s.to_string() == "every 6h")
    });
    assert_eq!(
        f.reloads(),
        1,
        "the tray is told, as after the editor's Save"
    );

    // What the tray does on that Reload: its task follows the new schedule.
    let exe = f.temp.path().join("not-run").join("replica-tray.exe");
    reconcile_tasks(&f.tasks.0, &f.settings(), &exe).unwrap();
    let after = docs(&f);
    assert_ne!(after, before, "the task's trigger changed");
    assert_eq!(after.as_deref(), Some("every 6h"));
}
