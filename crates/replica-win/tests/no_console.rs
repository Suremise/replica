//! No console windows: a child Replica starts gets no console window, even from a
//! process that has no console of its own (the tray and window are a GUI exe), down every path a
//! child is started by: `replica_core::process::command` alone, the job object restic and steps
//! run in (`JobObjectTree`, which adds `CREATE_SUSPENDED`), and a step's `cmd`.
//!
//! The test starts this test binary again as a **parent** with `DETACHED_PROCESS` (no console,
//! as the tray), which starts it a third time, as a **child**, down each path. Each child writes
//! whether `GetConsoleWindow` found a window. A child started without `CREATE_NO_WINDOW` would get
//! a new console, and a window with it.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use replica_core::platform::{ProcessTree, TreeChild};
use replica_core::process;
use replica_win::jobs::JobObjectTree;

const ROLE: &str = "REPLICA_NO_CONSOLE_ROLE";
const DIR: &str = "REPLICA_NO_CONSOLE_DIR";
const NAME: &str = "REPLICA_NO_CONSOLE_NAME";
/// Windows' `DETACHED_PROCESS`: the parent starts with no console at all.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const LIMIT: Duration = Duration::from_secs(120);

/// The arguments that run only `test` in this binary.
fn only(test: &str) -> [&str; 3] {
    ["--exact", test, "--test-threads=1"]
}

fn role() -> Option<String> {
    std::env::var(ROLE).ok()
}

/// Run only as a child: writes `window` or `none` to `<dir>/<name>`.
#[test]
fn child_reports_its_console_window() {
    if role().as_deref() != Some("child") {
        return;
    }
    let dir = PathBuf::from(std::env::var_os(DIR).unwrap());
    let name = std::env::var(NAME).unwrap();
    let seen = if replica_win::console::has_console_window() {
        "window"
    } else {
        "none"
    };
    std::fs::write(dir.join(name), seen).unwrap();
}

/// Run only as the parent, with no console: starts a child down each path.
#[test]
fn parent_starts_a_child_down_each_path() {
    if role().as_deref() != Some("parent") {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let child_env = |c: &mut Command, name: &str| {
        c.env(ROLE, "child").env(NAME, name).stdout(Stdio::null());
    };

    // The helper alone.
    let mut plain = process::command(&exe);
    plain.args(only("child_reports_its_console_window"));
    child_env(&mut plain, "plain");
    assert!(plain.status().unwrap().success());

    // In a job object, as restic and steps run.
    let mut in_job = process::command(&exe);
    in_job.args(only("child_reports_its_console_window"));
    child_env(&mut in_job, "job");
    finish(JobObjectTree.spawn(in_job).unwrap());

    // A step: `cmd /D /S /C "<the line>"` in a job object, starting the child as typed.
    let line = format!(
        "\"{}\" {}",
        exe.display(),
        only("child_reports_its_console_window").join(" ")
    );
    let mut step = replica_core::steps::shell(&line);
    child_env(&mut step, "step");
    finish(JobObjectTree.spawn(step).unwrap());
}

/// Waits (bounded) for a child in its tree to end successfully.
fn finish(mut child: Box<dyn TreeChild>) {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{status:?}");
            return;
        }
        assert!(start.elapsed() < LIMIT, "the child never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The parent, killed and reaped however the test ends.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| format!("not written: {e}"))
}

#[test]
fn children_get_no_console_window_even_from_a_process_without_one() {
    if role().is_some() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut parent = Command::new(std::env::current_exe().unwrap());
    parent
        .args(only("parent_starts_a_child_down_each_path"))
        .env(ROLE, "parent")
        .env(DIR, dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    {
        use std::os::windows::process::CommandExt;
        parent.creation_flags(DETACHED_PROCESS);
    }
    let mut parent = KillOnDrop(parent.spawn().unwrap());
    let start = Instant::now();
    let status = loop {
        if let Some(status) = parent.0.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < LIMIT, "the parent never ended");
        std::thread::sleep(Duration::from_millis(20));
    };
    let seen: Vec<(&str, String)> = ["plain", "job", "step"]
        .into_iter()
        .map(|name| (name, read(dir.path(), name)))
        .collect();
    assert!(status.success(), "the parent failed: {status:?}, {seen:?}");
    for (name, window) in &seen {
        assert_eq!(window, "none", "{name}: {seen:?}");
    }
}
