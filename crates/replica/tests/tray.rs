//! `replica-tray.exe` end to end, from portable copies in temp folders: the hidden `--run`
//! mode's exit codes, and a real Task Scheduler task (in a test-only folder, deleted
//! afterwards) running a job through it.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use replica_core::history::History;
use replica_core::run::Outcome;
use replica_core::settings::Settings;
use replica_win::scheduler::Scheduler;

/// The `[app]` line that turns the Task Scheduler option on.
const TASK_SCHEDULER: &str = "schedule_mode = \"task-scheduler\"";

/// A portable copy of `replica-tray.exe` with a mirror job, `good`, and a job whose source
/// isn't there, `bad`. Their target is a folder in the same temp folder, on its own volume.
struct Portable {
    temp: tempfile::TempDir,
}

impl Portable {
    fn new(extra_app: &str) -> Portable {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let src = temp.path().join("src");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("hello.txt"), "hello\n").unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_replica-tray"),
            app.join("replica-tray.exe"),
        )
        .unwrap();
        std::fs::write(app.join("portable"), "").unwrap();

        let root = temp.path().components().next().unwrap();
        let drive = format!("{}\\", root.as_os_str().to_string_lossy());
        let volume = replica_win::targets::volume_id_of(&drive).unwrap();
        let relative = |p: &Path| p.to_string_lossy()[drive.len()..].to_string();
        let settings = format!(
            "[app]\n{extra_app}\n\n\
             [[jobs]]\nname = \"good\"\nmode = \"mirror\"\nmirror_deletes_confirmed = true\n\
             [[jobs.sources]]\npath = '{src}'\n\
             [[jobs.targets]]\nkind = \"drive\"\nvolume_id = '{volume}'\npath = '{good}'\n\n\
             [[jobs]]\nname = \"bad\"\nmode = \"mirror\"\nmirror_deletes_confirmed = true\n\
             [[jobs.sources]]\npath = '{missing}'\n\
             [[jobs.targets]]\nkind = \"drive\"\nvolume_id = '{volume}'\npath = '{bad}'\n",
            src = src.display(),
            good = relative(&temp.path().join("mirror")),
            bad = relative(&temp.path().join("mirror-bad")),
            missing = temp.path().join("no-such-source").display(),
        );
        std::fs::write(app.join("settings.toml"), settings).unwrap();
        // The file must load as Replica would load it.
        Settings::load(&app.join("settings.toml")).unwrap();
        Portable { temp }
    }

    fn exe(&self) -> PathBuf {
        self.temp.path().join("app").join("replica-tray.exe")
    }

    fn run(&self, job: &str) -> i32 {
        Command::new(self.exe())
            .args(["--run", job])
            .env(replica::tray::LINGER_VAR, "0")
            .status()
            .unwrap()
            .code()
            .unwrap()
    }

    fn runs(&self, job: &str) -> Vec<Outcome> {
        let db = self.temp.path().join("app").join("history.db");
        if !db.exists() {
            return Vec::new();
        }
        History::open(&db)
            .unwrap()
            .recent(Some(job), 10)
            .unwrap()
            .into_iter()
            .map(|r| r.outcome)
            .collect()
    }
}

#[test]
fn run_exits_with_the_job_s_code_and_records_it() {
    let p = Portable::new(TASK_SCHEDULER);
    assert_eq!(p.run("good"), 0);
    assert_eq!(p.runs("good"), [Outcome::Ok]);
    assert!(
        p.temp
            .path()
            .join("mirror")
            .join("src")
            .join("hello.txt")
            .is_file()
    );

    assert_eq!(p.run("bad"), 1);
    assert_eq!(p.runs("bad"), [Outcome::Failed]);

    assert_eq!(p.run("no-such-job"), 4);
}

#[test]
fn run_does_nothing_while_schedules_are_paused() {
    let p = Portable::new(&format!("{TASK_SCHEDULER}\npause_schedules = true"));
    assert_eq!(p.run("good"), 0);
    assert!(p.runs("good").is_empty());
}

/// A task left over after Task Scheduler was turned off doesn't run its job.
#[test]
fn run_does_nothing_while_task_scheduler_is_off() {
    let p = Portable::new(r#"schedule_mode = "timer""#);
    assert_eq!(p.run("good"), 0);
    assert!(p.runs("good").is_empty());
}

fn test_folder(name: &str) -> TestFolder {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    TestFolder(Scheduler::in_folder(&format!(
        r"\ReplicaTest-{}-{nanos}-{name}",
        std::process::id()
    )))
}

fn scheduled(names: &[&str]) -> Vec<replica_core::job::Job> {
    names
        .iter()
        .map(|n| replica_core::job::Job {
            name: (*n).into(),
            schedule: Some("daily 03:00".parse().unwrap()),
            ..Default::default()
        })
        .collect()
}

/// Task Scheduler was turned off while the tray wasn't running: when the tray next loads the
/// settings (at start), the tasks still on are removed, and one the user turned off stays.
/// Turned back on, the tasks come back.
#[test]
fn the_saved_mode_is_honoured_when_the_tray_loads_the_settings() {
    use replica::tray::runner::{Reconciled, reconcile_tasks};
    use replica_core::settings::ScheduleMode;

    let folder = test_folder("reconcile");
    let exe = std::env::temp_dir()
        .join("replica-test-not-run")
        .join("replica-tray.exe");
    let mut settings = Settings {
        jobs: scheduled(&["on", "kept"]),
        ..Settings::default()
    };
    settings.app.schedule_mode = ScheduleMode::TaskScheduler;
    reconcile_tasks(&folder.0, &settings, &exe).unwrap();
    folder.0.set_enabled("kept", false).unwrap();

    settings.app.schedule_mode = ScheduleMode::Timer;
    assert_eq!(
        reconcile_tasks(&folder.0, &settings, &exe).unwrap(),
        Reconciled::Removed(1)
    );
    let left: Vec<_> = folder.0.list(&["on", "kept"]).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "kept");
    assert!(!left[0].enabled);
    // Nothing more to remove the next time.
    assert_eq!(
        reconcile_tasks(&folder.0, &settings, &exe).unwrap(),
        Reconciled::Removed(0)
    );

    settings.app.schedule_mode = ScheduleMode::TaskScheduler;
    let Reconciled::Synced(report) = reconcile_tasks(&folder.0, &settings, &exe).unwrap() else {
        panic!("synced")
    };
    assert_eq!(report.created, ["on"]);
    let kept = folder
        .0
        .list(&[])
        .unwrap()
        .into_iter()
        .find(|t| t.name == "kept")
        .unwrap();
    assert!(!kept.enabled, "still the user's choice");
}

/// A test-only Task Scheduler folder, emptied and removed when dropped.
struct TestFolder(Scheduler);

impl Drop for TestFolder {
    fn drop(&mut self) {
        if let Err(error) = self.0.delete_all() {
            eprintln!("test tasks left behind in {}: {error}", self.0.folder());
        }
    }
}

/// The whole Task Scheduler path: a task made by `sync`, started by Task Scheduler, runs the
/// job through `replica-tray --run`, records it, and reports its exit code to Task Scheduler.
#[test]
fn a_task_scheduler_task_runs_the_job() {
    let p = Portable::new(TASK_SCHEDULER);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let folder = TestFolder(Scheduler::in_folder(&format!(
        r"\ReplicaTest-{}-{nanos}-e2e",
        std::process::id()
    )));
    let settings = Settings::load(&p.temp.path().join("app").join("settings.toml")).unwrap();
    let mut jobs = settings.settings.jobs;
    jobs.iter_mut()
        .for_each(|j| j.schedule = Some("daily 03:00".parse().unwrap()));
    folder.0.sync(&jobs, &p.exe()).unwrap();
    folder.0.run_now("good").unwrap();

    let start = Instant::now();
    loop {
        let task = folder
            .0
            .list(&["good", "bad"])
            .unwrap()
            .into_iter()
            .find(|t| t.name == "good")
            .unwrap();
        if !task.running && task.last_result == Some(0) && !p.runs("good").is_empty() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "the task didn't finish: {task:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(p.runs("good"), [Outcome::Ok]);
    assert!(
        p.temp
            .path()
            .join("mirror")
            .join("src")
            .join("hello.txt")
            .is_file()
    );
}

/// Start with Windows starts the tray in the background, and an entry the previous version
/// wrote (no arguments, so it would open the window at every login) is rewritten when the tray
/// applies the setting at start. In a test-only key, never the Run key.
#[test]
fn start_with_windows_starts_the_tray_in_the_background() {
    use replica_win::autostart::{AutoStart, delete_test_key, test_key};

    /// The tray's entry in the test's own key, which is removed when dropped.
    struct Removed(AutoStart, String);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = delete_test_key(&self.1);
        }
    }
    let name = format!("{}-background", std::process::id());
    let key = test_key(&name);
    let entry = Removed(replica::tray::autostart("Replica").in_key(&key), name);
    let exe = Path::new(r"C:\Program Files\Replica\replica-tray.exe");
    assert_eq!(
        entry.0.command_for(exe),
        format!(r#""{}" {}"#, exe.display(), replica::tray::BACKGROUND)
    );
    assert_eq!(
        replica::tray::Mode::parse(&[replica::tray::BACKGROUND.to_string()]),
        Some(replica::tray::Mode::Tray { show: false })
    );

    AutoStart::new("Replica").in_key(&key).enable(exe).unwrap();
    assert!(!entry.0.starts(exe).unwrap());
    entry.0.apply(true, exe).unwrap();
    assert_eq!(entry.0.command().unwrap(), Some(entry.0.command_for(exe)));
    // Turned off (or uninstalled), the entry with its argument goes.
    entry.0.apply(false, exe).unwrap();
    assert_eq!(entry.0.command().unwrap(), None);
}

// --- The tray itself, end to end ---------------------------------------------------------------

/// What a test tray has of its own ([`replica::tray::runner::TEST_TAG_VAR`]): every process it
/// started, and its tasks and Start with Windows key. Dropped, at the end or in a failed
/// assertion's unwinding, it stops every process still running first, then removes the rest.
/// Drop it before the portable copy, whose exe the processes run.
struct OwnTray {
    tag: String,
    children: Vec<std::process::Child>,
}

impl OwnTray {
    fn scheduler(&self) -> Scheduler {
        Scheduler::in_folder(&format!(r"\{}", self.tag))
    }

    /// Starts `command`, kept until this is dropped. Gives its index.
    fn spawn(&mut self, command: &mut Command) -> usize {
        self.children.push(command.spawn().unwrap());
        self.children.len() - 1
    }

    /// Waits up to `secs` seconds for process `index` to end, and gives how it ended.
    fn wait(&mut self, index: usize, secs: u64, what: &str) -> std::process::ExitStatus {
        let child = &mut self.children[index];
        until(secs, what, || child.try_wait().unwrap())
    }
}

impl Drop for OwnTray {
    fn drop(&mut self) {
        for child in &mut self.children {
            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Err(error) = self.scheduler().delete_all() {
            eprintln!("test tasks left behind in \\{}: {error}", self.tag);
        }
        let _ = replica_win::autostart::delete_test_key(&self.tag);
    }
}

/// Polls `done` until it gives something, for up to `secs` seconds.
fn until<T>(secs: u64, what: &str, mut done: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(found) = done() {
            return found;
        }
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The real tray, `replica-tray --show` from a portable copy with a test's own name: its
/// window opens; its tasks and Start with Windows entry are its own (never `\Replica` or the
/// Run key); closing the window sends it to the tray; Open (a second launch, as the menu's
/// Open does) brings it back; `--quit` ends it; and the log follows each step.
///
/// Ignored in the suite: it opens a real window and tray icon on the desktop. Run it with
/// `cargo nextest run -p replica --test tray --run-ignored only`.
#[test]
#[ignore = "opens a real window and tray icon: run on purpose"]
fn the_real_tray_opens_hides_shows_and_quits_with_nothing_of_an_installed_one_touched() {
    use replica::tray::runner::TEST_TAG_VAR;
    use replica_win::autostart::{AutoStart, RUN_KEY, test_key};
    use replica_win::window::{post_close, process_windows};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tag = format!("ReplicaTest-{}-{nanos}-tray", std::process::id());
    let p = Portable::new(&format!("{TASK_SCHEDULER}\nstart_with_windows = true"));
    let app = p.temp.path().join("app");
    let settings_file = app.join("settings.toml");
    let mut settings = Settings::load(&settings_file).unwrap().settings;
    for job in &mut settings.jobs {
        job.schedule = Some("daily 03:00".parse().unwrap());
    }
    settings.save(&settings_file).unwrap();
    // Closing goes straight to the tray, without the notice the first time.
    std::fs::write(app.join("window.json"), r#"{ "tray_notice_off": true }"#).unwrap();
    // Only read: the Run key's Replica entry, to show it's left as it was.
    let run_entry = || AutoStart::new("Replica").in_key(RUN_KEY).command().unwrap();
    let run_before = run_entry();

    let tray = |args: &[&str]| {
        let mut command = Command::new(p.exe());
        command
            .args(args)
            .env(TEST_TAG_VAR, &tag)
            .env(replica::tray::LINGER_VAR, "0");
        command
    };
    let mut own = OwnTray {
        tag: tag.clone(),
        children: Vec::new(),
    };
    let first = own.spawn(&mut tray(&["--show"]));
    let pid = own.children[first].id();
    // The window, once it shows: the tray has hidden windows of its own.
    let hwnd = until(60, "the window to open", || {
        process_windows(pid)
            .into_iter()
            .find(|(_, title, shown)| title == "Replica" && *shown)
            .map(|(hwnd, _, _)| hwnd)
    });
    // Whether that window shows, when it's `visible`.
    let window = |visible: bool| {
        process_windows(pid)
            .into_iter()
            .any(|(h, _, shown)| h == hwnd && shown == visible)
            .then_some(hwnd)
    };
    let tasks = until(30, "the tasks in its own folder", || {
        let tasks = own.scheduler().list(&["good", "bad"]).ok()?;
        (tasks.len() == 2).then_some(tasks)
    });
    assert!(tasks.iter().all(|t| t.enabled));
    let entry = AutoStart::new("Replica").in_key(&test_key(&tag));
    let command = until(30, "Start with Windows in its own key", || {
        entry.command().ok().flatten()
    });
    assert!(command.contains("replica-tray.exe") && command.ends_with(replica::tray::BACKGROUND));
    assert_eq!(run_entry(), run_before, "the Run key isn't touched");

    // Closed, it goes to the tray; Open brings it back.
    post_close(hwnd).unwrap();
    until(30, "the window to go to the tray", || window(false));
    let second = own.spawn(&mut tray(&[]));
    let second = own.wait(second, 30, "a second launch to hand over and end");
    assert_eq!(
        second.code(),
        Some(0),
        "a second launch hands over and ends"
    );
    let shown = until(30, "Open to bring it back", || window(true));
    assert_eq!(shown, hwnd, "the same window");

    // Quit ends it.
    let quit = own.spawn(&mut tray(&["--quit"]));
    let quit = own.wait(quit, 60, "--quit to see the tray end");
    assert_eq!(quit.code(), Some(0), "it saw the tray end");
    let ended = own.wait(first, 30, "the tray to end");
    assert_eq!(ended.code(), Some(0));

    let log = std::fs::read_dir(app.join("logs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("replica-tray-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .expect("the tray's log");
    let mut rest = log.as_str();
    for line in [
        "brought the scheduled tasks up to date",
        "starting the window",
        "the window started",
        "the window is showing",
        "the window was closed",
        "the window went to the tray",
        "opening the window",
        "the window is showing",
        "quitting: asked to by another process",
    ] {
        match rest.find(line) {
            Some(at) => rest = &rest[at + line.len()..],
            None => panic!("the log has no {line:?} after the lines before it:\n{log}"),
        }
    }
    assert!(!log.contains("panicked"), "{log}");
}
