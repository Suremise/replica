//! The per-process app log, the global subscriber and cleanup, in temp folders.
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use replica_win::logging::{self, LogConfig, Retention};

const CHILD: &str = "REPLICA_LOGGING_TEST_CHILD";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The folder's file names, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `app`'s log files (not `app-tray`'s).
fn log_files(dir: &Path) -> Vec<PathBuf> {
    names(dir)
        .into_iter()
        .filter(|n| n.starts_with("app-2") && n.ends_with(".log"))
        .map(|n| dir.join(n))
        .collect()
}

/// Polls `condition` until it holds, or fails after a generous bound.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_subscriber_writes_to_this_process_s_own_file() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("logs");
    let path = logging::init(&LogConfig::new(Some(dir.clone()), "replica-test"))
        .unwrap()
        .unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("replica-test-20"), "{name}");
    assert!(
        name.ends_with(&format!("-{}.log", std::process::id())),
        "{name}"
    );
    tracing::info!("hello from the test");
    tracing::debug!("below the level");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("hello from the test"), "{text}");
    assert!(!text.contains("below the level"));
    assert!(!text.contains('\u{1b}'), "no colour codes in the file");
    // A second subscriber is refused.
    assert!(logging::init(&LogConfig::new(None, "again")).is_err());
}

/// Run only as a child of the test below. In steps, each signalled with a marker file: start
/// logging (cleaning up as it starts) and say `ready-<pid>`; once told to `go`, log 50 lines and
/// say `done-<pid>`; stay running, with the log open, until told to `exit`.
#[test]
fn child_logger() {
    let Some(dir) = std::env::var_os(CHILD) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let pid = std::process::id();
    let mut config = LogConfig::new(Some(dir.clone()), "app");
    // Keep only the newest file: the other child's file is open, so it must be skipped.
    config.retention = Retention {
        keep: 1,
        max_age: None,
    };
    logging::init(&config).unwrap();
    fs::write(dir.join(format!("ready-{pid}")), "").unwrap();
    wait_for("the go marker", || dir.join("go").exists());
    for n in 0..50 {
        tracing::info!("process {pid} line {n:02}");
    }
    fs::write(dir.join(format!("done-{pid}")), "").unwrap();
    wait_for("the exit marker", || dir.join("exit").exists());
}

/// Child processes, killed if the test fails before they exit.
struct Children(Vec<Child>);

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Two processes logging at once, each cleaning up as it starts: each writes only its own
/// file, and neither deletes the other's.
#[test]
fn two_processes_log_at_once_without_touching_each_other() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_path_buf();
    let me = std::env::current_exe().unwrap();
    let mut children = Children(
        (0..2)
            .map(|_| {
                Command::new(&me)
                    .args(["--exact", "child_logger", "--test-threads=1"])
                    .env(CHILD, &dir)
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect(),
    );
    let pids: Vec<u32> = children.0.iter().map(|c| c.id()).collect();
    let all = |marker: &str| {
        pids.iter()
            .all(|pid| dir.join(format!("{marker}-{pid}")).exists())
    };
    // Both have started logging and finished their start-up cleanup before either logs.
    wait_for("both ready", || all("ready"));
    fs::write(dir.join("go"), "").unwrap();
    // Both are still running, logs open, when the files are checked.
    wait_for("both done", || all("done"));

    let files = log_files(&dir);
    assert_eq!(files.len(), 2, "{files:?}");
    for pid in &pids {
        let file = files
            .iter()
            .find(|f| f.to_string_lossy().ends_with(&format!("-{pid}.log")))
            .unwrap_or_else(|| panic!("no file for {pid}: {files:?}"));
        let text = fs::read_to_string(file).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 50, "{text}");
        let own = format!("process {pid} line ");
        assert!(lines.iter().all(|l| l.contains(&own)), "{text}");
    }

    fs::write(dir.join("exit"), "").unwrap();
    for child in &mut children.0 {
        assert!(child.wait().unwrap().success());
    }
}

/// Makes a file with the given age.
fn touch(path: &Path, age: Duration) {
    fs::write(path, "old\n").unwrap();
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

/// Six of `app`'s logs, a day apart (`app-…-0.log` the newest), and files that aren't its.
fn old_logs(dir: &Path) {
    for n in 0..6u32 {
        touch(
            &dir.join(format!("app-2026-10-0{}-120000-{n}.log", 9 - n)),
            DAY * n,
        );
    }
    touch(&dir.join("app-tray-2026-10-01-120000-9.log"), DAY * 30);
    touch(&dir.join("app.log"), DAY * 30);
    touch(&dir.join("notes.txt"), DAY * 30);
    fs::create_dir(dir.join("runs")).unwrap();
}

#[test]
fn cleanup_keeps_the_newest_and_only_touches_its_own_logs() {
    let temp = tempfile::tempdir().unwrap();
    old_logs(temp.path());
    let keep_four = Retention {
        keep: 4,
        max_age: None,
    };
    assert_eq!(
        logging::prune_logs(temp.path(), "app", keep_four).unwrap(),
        2
    );
    assert_eq!(
        names(temp.path()),
        [
            "app-2026-10-06-120000-3.log",
            "app-2026-10-07-120000-2.log",
            "app-2026-10-08-120000-1.log",
            "app-2026-10-09-120000-0.log",
            "app-tray-2026-10-01-120000-9.log",
            "app.log",
            "notes.txt",
            "runs"
        ]
    );
}

#[test]
fn cleanup_drops_files_past_the_age_limit() {
    let temp = tempfile::tempdir().unwrap();
    old_logs(temp.path());
    let two_and_a_half_days = Retention {
        keep: 20,
        max_age: Some(DAY * 5 / 2),
    };
    let deleted = logging::prune_logs(temp.path(), "app", two_and_a_half_days).unwrap();
    assert_eq!(deleted, 3);
    assert_eq!(log_files(temp.path()).len(), 3);
}

/// A log another process still has open can't be deleted: cleanup skips it and carries on.
#[test]
fn cleanup_skips_a_file_that_is_open() {
    let temp = tempfile::tempdir().unwrap();
    old_logs(temp.path());
    // Opened as a running process opens its own log; made the oldest of all.
    let (held_path, held) = logging::create_log_file(temp.path(), "app").unwrap();
    held.set_modified(SystemTime::now() - DAY * 10).unwrap();
    assert!(
        fs::remove_file(&held_path).is_err(),
        "an open log can't be deleted"
    );

    let keep_two = Retention {
        keep: 2,
        max_age: None,
    };
    assert_eq!(
        logging::prune_logs(temp.path(), "app", keep_two).unwrap(),
        4
    );
    let left = log_files(temp.path());
    assert_eq!(left.len(), 3, "{left:?}");
    assert!(left.contains(&held_path));

    drop(held);
    assert_eq!(
        logging::prune_logs(temp.path(), "app", keep_two).unwrap(),
        1
    );
    assert!(!held_path.exists());
}

#[test]
fn prune_keeps_the_newest_run_logs_and_drops_the_old() {
    let temp = tempfile::tempdir().unwrap();
    let runs = temp.path();
    for n in 0..6u32 {
        touch(&runs.join(format!("run-{n}.log")), DAY * n);
    }
    fs::create_dir(runs.join("a-folder")).unwrap();

    let keep_four = Retention {
        keep: 4,
        max_age: None,
    };
    assert_eq!(logging::prune(runs, keep_four).unwrap(), 2);
    assert_eq!(
        names(runs),
        [
            "a-folder",
            "run-0.log",
            "run-1.log",
            "run-2.log",
            "run-3.log"
        ]
    );

    let two_and_a_half_days = Retention {
        keep: 10,
        max_age: Some(DAY * 5 / 2),
    };
    assert_eq!(logging::prune(runs, two_and_a_half_days).unwrap(), 1);
    assert_eq!(
        names(runs),
        ["a-folder", "run-0.log", "run-1.log", "run-2.log"]
    );
}
