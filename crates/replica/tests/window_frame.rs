//! The window's frame, driven through egui's accessibility tree with `egui_kittest`: the menu
//! bar's commands each reach their action, the shortcuts work, the status bar follows the tray
//! and is turned off and on from View (remembered in `settings.toml`), and the sidebar's five
//! pages are in View too, About in Help. Against a fake backend that records what the
//! window asks (the tray, Explorer, the browser) instead of doing it.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::accesskit::{Role, Toggled};
use eframe::egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use replica::ui::Shared;
use replica::ui::backend::{Backend, LastRun, Places, RestoreJob, Status, TargetCheck, UiRequest};
use replica::ui::geometry::WindowState;
use replica::ui::shell::{Shell, Tab};
use replica_core::cancel::CancelToken;
use replica_core::history::{HistoryFilter, RunRecord, TestRestoreRecord};
use replica_core::job::{Job, Source, Target, VolumeRef};
use replica_core::platform::fake::MemorySecretStore;
use replica_core::platform::{Secret, SecretStore};
use replica_core::restic::{Node, RestoreSummary, Snapshot};
use replica_core::run::Outcome;
use replica_core::settings::Settings;
use replica_win::keys::{Entered, NotSaved};
use replica_win::scheduler::ScheduledTask;
use replica_win::targets::VolumeInfo;

/// What the frame asked for that leaves the window.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Tray(UiRequest),
    Open(PathBuf),
    Url(String),
    TaskScheduler,
    JobsFile,
}

#[derive(Default)]
struct Fake {
    dir: PathBuf,
    status: Mutex<Status>,
    asked: Mutex<Vec<Asked>>,
    secrets: MemorySecretStore,
}

impl Fake {
    fn ask(&self, asked: Asked) {
        self.asked.lock().unwrap().push(asked);
    }
    fn asked(&self) -> Vec<Asked> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
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
        // Reload follows every saved change: not a command of the menus'.
        if request != UiRequest::Reload {
            self.ask(Asked::Tray(request));
        }
    }
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    fn secrets(&self) -> &dyn SecretStore {
        &self.secrets
    }
    fn volumes(&self) -> Result<Vec<VolumeInfo>, String> {
        Ok(Vec::new())
    }
    fn has_task(&self, _: &str) -> bool {
        false
    }
    fn delete_task(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn check_target(&self, _: &Target) -> TargetCheck {
        TargetCheck::Failed("not in these tests".into())
    }
    fn init_repository(&self, _: &Target, _: Secret) -> Result<(), String> {
        Err("not in these tests".into())
    }
    fn save_entered_password(&self, _: &Target, _: Secret, _: bool) -> Result<Entered, NotSaved> {
        Err(NotSaved::Failed("not in these tests".into()))
    }
    fn pick_folder(&self) -> Option<PathBuf> {
        None
    }
    fn pick_file(&self) -> Option<PathBuf> {
        None
    }
    fn save_jobs_file(&self) -> Option<PathBuf> {
        None
    }
    fn open_jobs_file(&self) -> Option<PathBuf> {
        self.ask(Asked::JobsFile);
        None
    }
    fn runs(&self, _: &HistoryFilter, _: usize) -> Result<Vec<RunRecord>, String> {
        Ok(Vec::new())
    }
    fn test_restores(&self, _: &HistoryFilter, _: usize) -> Result<Vec<TestRestoreRecord>, String> {
        Ok(Vec::new())
    }
    fn history_jobs(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn places(&self) -> Places {
        Places {
            portable: true,
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
        self.ask(Asked::TaskScheduler);
        Ok(())
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.ask(Asked::Url(url.into()));
        Ok(())
    }
    fn restic_in_use(&self, _: Option<PathBuf>) -> Result<(PathBuf, String), String> {
        Err("not in these tests".into())
    }
    fn open_path(&self, path: &Path) -> Result<(), String> {
        self.ask(Asked::Open(path.into()));
        Ok(())
    }
    fn snapshots(&self, _: &Job, _: &Target) -> Result<Vec<Snapshot>, String> {
        Ok(Vec::new())
    }
    fn list(&self, _: &Target, _: &str, _: &str) -> Result<Vec<Node>, String> {
        Ok(Vec::new())
    }
    fn restore(
        &self,
        _: &Job,
        _: &Target,
        _: &RestoreJob,
        _: &CancelToken,
    ) -> Result<RestoreSummary, String> {
        Err("not in these tests".into())
    }
    fn test_restore(
        &self,
        _: &Job,
        _: &Target,
        _: &CancelToken,
    ) -> Result<TestRestoreRecord, String> {
        Err("not in these tests".into())
    }
    fn set_pinned(&self, _: &Job, _: &Target, _: &str, _: bool) -> Result<String, String> {
        Err("not in these tests".into())
    }
}

/// A job backing up a folder to a drive, on `schedule`.
fn job(name: &str, schedule: Option<&str>, enabled: bool) -> Job {
    Job {
        name: name.into(),
        enabled,
        schedule: schedule.map(|s| s.parse().unwrap()),
        sources: vec![Source {
            path: PathBuf::from(r"C:\Users\Alex").join(name),
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

/// A temp app folder with three jobs (Documents and Photos on, Old off), and the window on it.
struct Fixture {
    _temp: tempfile::TempDir,
    fake: Arc<Fake>,
}

impl Fixture {
    fn new() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake {
            dir: temp.path().to_path_buf(),
            ..Fake::default()
        });
        let settings = Settings {
            jobs: vec![
                job("Documents", Some("daily 02:30"), true),
                job("Photos", Some("every 6h"), true),
                job("Old", Some("daily 21:00"), false),
            ],
            ..Settings::default()
        };
        assert!(settings.validate().is_empty(), "{:?}", settings.validate());
        settings.save(&fake.settings_file()).unwrap();
        Fixture { _temp: temp, fake }
    }

    fn shell(&self) -> Shell {
        Shell::new(
            Arc::clone(&self.fake) as Arc<dyn Backend>,
            Arc::new(Shared::default()),
            WindowState::default(),
            None,
        )
    }

    fn harness(&self) -> Harness<'static, Shell> {
        self.harness_at(1200.0)
    }

    /// The window `width` points wide.
    fn harness_at(&self, width: f32) -> Harness<'static, Shell> {
        let mut harness = Harness::builder()
            .with_size([width, 760.0])
            .with_max_steps(64)
            .with_context_setup(|ctx| {
                replica::ui::fonts::install(ctx).unwrap();
                replica::ui::style(ctx);
            })
            .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), self.shell());
        let _ = harness.try_run();
        harness
    }
}

/// Opens the bar's menu `menu` and chooses `item` in it, as a screen reader would.
fn choose(harness: &mut Harness<'_, Shell>, menu: &str, item: &str) {
    harness
        .get_by_role_and_label(Role::MenuItem, menu)
        .click_accesskit();
    let _ = harness.try_run();
    let node = harness
        .get_all_by_label(item)
        .find(|n| {
            let node = n.accesskit_node();
            // An item in the menu, not a menu in the bar ("Jobs" is both).
            matches!(
                node.role(),
                Role::MenuItem | Role::MenuItemRadio | Role::MenuItemCheckBox
            ) && node.data().has_popup().is_none()
        })
        .unwrap_or_else(|| panic!("{item} in {menu}"));
    node.click_accesskit();
    let _ = harness.try_run();
}

/// Whether `menu`'s `item` can be chosen now (the menu is closed again after).
fn enabled(harness: &mut Harness<'_, Shell>, menu: &str, item: &str) -> bool {
    harness
        .get_by_role_and_label(Role::MenuItem, menu)
        .click_accesskit();
    let _ = harness.try_run();
    let on = !harness
        .get_by_role_and_label(Role::MenuItem, item)
        .accesskit_node()
        .is_disabled();
    harness.key_press(Key::Escape);
    let _ = harness.try_run();
    on
}

#[test]
fn the_menu_bar_has_its_menus_and_each_item_names_its_shortcut() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    harness.get_by_role_and_label(Role::MenuBar, "Menu bar");
    for menu in ["File", "Jobs", "View", "Tools", "Help"] {
        harness.get_by_role_and_label(Role::MenuItem, menu);
    }
    harness
        .get_by_role_and_label(Role::MenuItem, "File")
        .click_accesskit();
    let _ = harness.try_run();
    let new_job = harness.get_by_role_and_label(Role::MenuItem, "New job…");
    assert_eq!(
        new_job.accesskit_node().data().keyboard_shortcut(),
        Some("Ctrl+N"),
        "the shortcut is read apart from the name"
    );
    harness.key_press(Key::Escape);
    let _ = harness.try_run();
    harness
        .get_by_role_and_label(Role::MenuItem, "View")
        .click_accesskit();
    let _ = harness.try_run();
    for (page, shortcut) in [
        ("Jobs", "Ctrl+1"),
        ("History", "Ctrl+2"),
        ("Restore", "Ctrl+3"),
        ("Schedules", "Ctrl+4"),
        ("Settings", "Ctrl+,"),
    ] {
        assert_eq!(
            harness
                .get_by_role_and_label(Role::MenuItemRadio, page)
                .accesskit_node()
                .data()
                .keyboard_shortcut(),
            Some(shortcut),
            "{page}"
        );
    }
    harness.check_accessibility();
}

#[test]
fn schedules_and_settings_are_in_the_sidebar_and_about_in_help() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    let tabs: Vec<String> = harness
        .get_all_by_role(Role::Tab)
        .map(|t| t.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(
        tabs,
        ["Jobs", "History", "Restore", "Schedules", "Settings"]
    );

    for (page, subtitle) in [
        ("Schedules", "Replica's tasks in Windows Task Scheduler"),
        ("Settings", "How Replica runs, and where it keeps its files"),
    ] {
        harness.get_by_role_and_label(Role::Tab, page).click();
        let _ = harness.try_run();
        assert_eq!(harness.state().tab.title(), page);
        let tab = harness.get_by_role_and_label(Role::Tab, page);
        assert_eq!(tab.accesskit_node().is_selected(), Some(true), "{page}");
        harness.get_by_label(subtitle);
    }

    choose(&mut harness, "Help", "About Replica");
    assert_eq!(harness.state().tab, Tab::About);
    // About isn't in the sidebar: none of its pages is the one shown.
    for page in ["Jobs", "History", "Restore", "Schedules", "Settings"] {
        let tab = harness.get_by_role_and_label(Role::Tab, page);
        assert_eq!(tab.accesskit_node().is_selected(), Some(false), "{page}");
    }
    harness.get_by_label_contains(env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_schedules_page_has_the_tasks_and_settings_only_the_options() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    choose(&mut harness, "View", "Settings");
    assert_eq!(harness.state().tab, Tab::Settings);
    harness.get_by_label("Pause schedules");
    assert!(harness.query_by_label("Task Scheduler tasks").is_none());
    assert!(harness.query_by_label("Remove all Replica tasks").is_none());

    // Settings links to the page with the tasks.
    harness
        .get_by_label("See Replica's tasks in Task Scheduler on the Schedules page")
        .click_accesskit();
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::Schedules);
    harness.get_by_role_and_label(Role::Heading, "Task Scheduler tasks");
    harness.get_by_label("Replica has no tasks in Task Scheduler.");
    harness
        .get_by_label("Open Task Scheduler")
        .click_accesskit();
    let _ = harness.try_run();
    assert_eq!(fixture.fake.asked(), [Asked::TaskScheduler]);
    assert!(harness.query_by_label("Pause schedules").is_none());
    harness.check_accessibility();
}

#[test]
fn file_s_commands_reach_their_actions() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    harness.state_mut().tab = Tab::History;

    choose(&mut harness, "File", "New job…");
    assert_eq!(harness.state().tab, Tab::Jobs);
    assert!(harness.state().jobs.editing(), "the editor, on a new job");
    // Not while a job is being edited: that would lose it.
    assert!(!enabled(&mut harness, "File", "New job…"));
    assert!(!enabled(&mut harness, "File", "Import jobs…"));
    assert!(!enabled(&mut harness, "File", "Export jobs…"));
    harness.get_by_label("Cancel").click_accesskit();
    let _ = harness.try_run();
    assert!(!harness.state().jobs.editing());

    choose(&mut harness, "File", "Import jobs…");
    assert_eq!(
        fixture.fake.asked(),
        [Asked::JobsFile],
        "asks for a jobs file"
    );

    harness.state_mut().tab = Tab::Restore;
    choose(&mut harness, "File", "Export jobs…");
    assert_eq!(harness.state().tab, Tab::Jobs);
    harness.get_by_label("Export 3 jobs");
    harness.get_by_label("Cancel").click_accesskit();
    let _ = harness.try_run();

    choose(&mut harness, "File", "Open settings folder");
    choose(&mut harness, "File", "Open log folder");
    assert_eq!(
        fixture.fake.asked(),
        [
            Asked::Open(fixture.fake.dir.clone()),
            Asked::Open(fixture.fake.dir.join("logs")),
        ]
    );

    choose(&mut harness, "File", "Exit Replica");
    assert_eq!(fixture.fake.asked(), [Asked::Tray(UiRequest::Quit)]);

    // Close window goes to the tray, as the close button does: the first time, saying so.
    choose(&mut harness, "File", "Close window");
    harness.get_by_label("Replica is still running");
}

#[test]
fn the_jobs_menu_runs_the_jobs_that_are_on_and_stops_them() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    assert!(
        !enabled(&mut harness, "Jobs", "Stop running jobs"),
        "nothing runs"
    );

    choose(&mut harness, "Jobs", "Run all");
    assert_eq!(
        fixture.fake.asked(),
        [Asked::Tray(UiRequest::Run(vec![
            "Documents".into(),
            "Photos".into()
        ]))]
    );

    fixture.fake.status.lock().unwrap().running = Some(("Documents".into(), 10));
    let _ = harness.try_run();
    choose(&mut harness, "Jobs", "Stop running jobs");
    assert_eq!(fixture.fake.asked(), [Asked::Tray(UiRequest::Stop)]);
}

#[test]
fn view_tools_and_help_reach_their_pages_and_pages() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    for (item, tab) in [
        ("History", Tab::History),
        ("Restore", Tab::Restore),
        ("Schedules", Tab::Schedules),
        ("Settings", Tab::Settings),
        ("Jobs", Tab::Jobs),
    ] {
        choose(&mut harness, "View", item);
        assert_eq!(harness.state().tab, tab, "{item}");
    }
    // The page shown is ticked.
    harness
        .get_by_role_and_label(Role::MenuItem, "View")
        .click_accesskit();
    let _ = harness.try_run();
    let pages = |h: &Harness<'_, Shell>, toggled: Option<Toggled>| -> Vec<String> {
        h.get_all_by_role(Role::MenuItemRadio)
            .filter(|n| toggled.is_none_or(|t| n.accesskit_node().toggled() == Some(t)))
            .map(|n| n.accesskit_node().label().unwrap_or_default())
            .collect()
    };
    assert_eq!(
        pages(&harness, None),
        ["Jobs", "History", "Restore", "Schedules", "Settings"]
    );
    assert_eq!(pages(&harness, Some(Toggled::True)), ["Jobs"]);
    harness.key_press(Key::Escape);
    let _ = harness.try_run();

    // Tools holds Windows' own Task Scheduler, and nothing else.
    harness
        .get_by_role_and_label(Role::MenuItem, "Tools")
        .click_accesskit();
    let _ = harness.try_run();
    let tools: Vec<String> = harness
        .get_all_by_role(Role::MenuItem)
        .filter(|n| n.accesskit_node().data().has_popup().is_none())
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(tools, ["Windows Task Scheduler"]);
    harness.key_press(Key::Escape);
    let _ = harness.try_run();

    choose(&mut harness, "Tools", "Windows Task Scheduler");
    choose(&mut harness, "Help", "Replica on GitHub");
    assert_eq!(
        fixture.fake.asked(),
        [
            Asked::TaskScheduler,
            Asked::Url(replica::ui::about::PROJECT_URL.into())
        ]
    );
}

#[test]
fn the_shortcuts_work() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    for (key, tab) in [
        (Key::Num2, Tab::History),
        (Key::Num3, Tab::Restore),
        (Key::Num4, Tab::Schedules),
        (Key::Num1, Tab::Jobs),
        (Key::Comma, Tab::Settings),
    ] {
        harness.key_press_modifiers(Modifiers::COMMAND, key);
        let _ = harness.try_run();
        assert_eq!(harness.state().tab, tab, "{key:?}");
    }
    harness.key_press_modifiers(Modifiers::COMMAND, Key::N);
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::Jobs);
    assert!(harness.state().jobs.editing());
}

#[test]
fn the_status_bar_is_turned_off_and_on_from_view_and_remembered() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    let status_bar_ticked = |h: &mut Harness<'_, Shell>| {
        h.get_by_role_and_label(Role::MenuItem, "View")
            .click_accesskit();
        let _ = h.try_run();
        let ticked = h
            .get_by_role_and_label(Role::MenuItemCheckBox, "Status bar")
            .accesskit_node()
            .toggled();
        h.key_press(Key::Escape);
        let _ = h.try_run();
        ticked
    };
    assert!(harness.state().status_bar_shown(), "on by default");
    assert_eq!(status_bar_ticked(&mut harness), Some(Toggled::True));
    harness.get_by_label("Ready");

    choose(&mut harness, "View", "Status bar");
    assert!(!harness.state().status_bar_shown());
    assert!(harness.query_by_label("Ready").is_none());
    assert_eq!(status_bar_ticked(&mut harness), Some(Toggled::False));
    let saved = Settings::load(&fixture.fake.settings_file())
        .unwrap()
        .settings;
    assert!(!saved.app.show_status_bar, "saved in settings.toml");
    assert_eq!(saved.jobs.len(), 3, "nothing else changed");

    // A new window (the app started again) remembers it.
    let again = fixture.harness();
    assert!(!again.state().status_bar_shown());
    assert!(again.query_by_label("Ready").is_none());

    choose(&mut harness, "View", "Status bar");
    assert!(harness.state().status_bar_shown());
    assert!(
        Settings::load(&fixture.fake.settings_file())
            .unwrap()
            .settings
            .app
            .show_status_bar
    );
}

#[test]
fn the_status_bar_shows_a_running_jobs_progress_and_the_next_run() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    harness.get_by_label("Ready");
    // Documents runs daily at 02:30 and Photos every 6 h, counted from the tray's start.
    let now = chrono::Utc::now();
    fixture.fake.status.lock().unwrap().since = Some(now);
    let _ = harness.try_run();
    harness.get_by_label_contains("Next: ");
    harness.get_by_label("Built-in timer");
    harness.get_by_label("Portable");

    {
        let mut status = fixture.fake.status.lock().unwrap();
        status.running = Some(("Photos".into(), 42));
        status.queued = vec!["Documents".into()];
    }
    let _ = harness.try_run();
    harness.get_by_label("Running Photos: 42%");
    harness.get_by_label("1 more queued");
    assert!(harness.query_by_label("Ready").is_none());
    // Neither is next: one runs and the other is queued.
    assert!(harness.query_by_label_contains("Next: ").is_none());

    // It follows the tray as the run goes on.
    fixture.fake.status.lock().unwrap().running = Some(("Photos".into(), 87));
    let _ = harness.try_run();
    harness.get_by_label("Running Photos: 87%");
}

#[test]
fn the_status_bars_problems_go_to_history() {
    let fixture = Fixture::new();
    let now = chrono::Utc::now();
    let last = |outcome| LastRun {
        at: now,
        outcome,
        reason: None,
    };
    {
        let mut status = fixture.fake.status.lock().unwrap();
        status
            .last
            .insert("documents".into(), last(Outcome::Failed));
        status.last.insert("photos".into(), last(Outcome::Ok));
    }
    let mut harness = fixture.harness();
    harness
        .get_by_role_and_label(Role::Button, "1 failed")
        .click_accesskit();
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::History);
    assert_eq!(
        harness.state().history.job.as_deref(),
        Some("Documents"),
        "the one job with a problem"
    );

    fixture
        .fake
        .status
        .lock()
        .unwrap()
        .last
        .insert("photos".into(), last(Outcome::Skipped));
    harness.state_mut().tab = Tab::Jobs;
    let _ = harness.try_run();
    harness
        .get_by_role_and_label(Role::Button, "1 failed, 1 skipped")
        .click_accesskit();
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::History);
    assert_eq!(harness.state().history.job, None, "every job");
}

/// Steps until `done`, for at most 10 s: the frame reads a changed settings file within a
/// second.
fn until(harness: &mut Harness<'_, Shell>, done: impl Fn(&Harness<'_, Shell>) -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(harness) {
        assert!(std::time::Instant::now() < end, "timed out");
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = harness.try_run();
}

#[test]
fn the_frame_follows_the_settings_whichever_page_shows() {
    let fixture = Fixture::new();
    fixture.fake.status.lock().unwrap().since = Some(chrono::Utc::now());
    let mut harness = fixture.harness();
    // Not the Jobs page, which reads the settings itself only while it shows.
    harness.state_mut().tab = Tab::History;
    let _ = harness.try_run();
    // Documents (daily) or Photos (every 6 h), whichever is sooner now.
    harness.get_by_label_contains("Next: ");
    assert!(harness.query_by_label_contains("Next: Music").is_none());

    // Changed outside the window (by hand, the CLI): Photos turned off, Music added, and
    // Documents every 24 h rather than daily at 02:30, so Music (every hour) is next at any
    // time of day.
    let file = fixture.fake.settings_file();
    let mut settings = Settings::load(&file).unwrap().settings;
    settings.jobs[0].schedule = Some("every 24h".parse().unwrap());
    settings.jobs[1].enabled = false;
    settings.jobs.push(job("Music", Some("every 1h"), true));
    settings.save(&file).unwrap();
    until(&mut harness, |h| {
        h.query_by_label_contains("Next: Music").is_some()
    });
    assert_eq!(harness.state().tab, Tab::History);

    choose(&mut harness, "Jobs", "Run all");
    assert_eq!(
        fixture.fake.asked(),
        [Asked::Tray(UiRequest::Run(vec![
            "Documents".into(),
            "Music".into()
        ]))],
        "the jobs on now, not as the Jobs page last saw them"
    );
}

#[test]
fn the_status_bar_fits_the_narrowest_window() {
    let long = "documents-and-every-project-folder-from-the-old-laptop-2026";
    let fixture = Fixture::new();
    let file = fixture.fake.settings_file();
    let mut settings = Settings::load(&file).unwrap().settings;
    settings.jobs.push(job(long, Some("every 1h"), true));
    settings.save(&file).unwrap();
    let now = chrono::Utc::now();
    let last = |outcome| LastRun {
        at: now,
        outcome,
        reason: None,
    };
    {
        let mut status = fixture.fake.status.lock().unwrap();
        status.since = Some(now);
        status.running = Some((long.into(), 42));
        status.waiting = Some(format!(
            "Waiting for {long} to finish with WD-MyPassport-4TB-Backup"
        ));
        status.queued = vec!["Photos".into()];
        status
            .last
            .insert("documents".into(), last(Outcome::Failed));
        status.last.insert("photos".into(), last(Outcome::Skipped));
    }
    let [width, _] = replica::ui::geometry::MIN_SIZE;
    let mut harness = fixture.harness_at(width);
    // The harness keeps an 8-point margin round the window: the bar is within 40 of its
    // bottom, and the window's right edge 8 in from the harness's.
    let bar = 760.0 - 40.0;
    let edge = width - 8.0 + 0.5;
    // Everything in the bar is inside the window, nothing past its right edge.
    let in_bar: Vec<(String, eframe::egui::Rect)> = harness
        .query_all(
            egui_kittest::kittest::By::new()
                .include_labels()
                .predicate(|n| n.bounding_box().is_some()),
        )
        .map(|n| {
            let node = n.accesskit_node();
            let name = node.label().or_else(|| node.value()).unwrap_or_default();
            (name, n.rect())
        })
        .filter(|(_, r)| r.top() >= bar && r.width() > 0.0)
        .collect();
    assert!(!in_bar.is_empty());
    for (label, rect) in &in_bar {
        assert!(
            rect.left() >= -0.5 && rect.right() <= edge,
            "{label:?} at {rect:?} isn't inside the {width}-point window"
        );
    }
    // The problems stay, and still go to History.
    let problems = harness.get_by_role_and_label(Role::Button, "1 failed, 1 skipped");
    assert!(problems.rect().right() <= edge);
    // The running job is cut short on the screen, but read whole.
    let running = format!("Running {long}: 42%");
    let running = harness.get_by_label(&running);
    assert!(running.rect().right() <= edge);
    harness
        .get_by_role_and_label(Role::Button, "1 failed, 1 skipped")
        .click_accesskit();
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::History);
}
