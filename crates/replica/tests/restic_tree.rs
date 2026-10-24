//! restic runs in a Windows job object: a cancel stops restic and every program it
//! started, and a program it left running after it exited is stopped with its command. A
//! stand-in restic (a `.cmd` script) starts a long-running PowerShell, which writes its PID, as
//! restic's SFTP or rclone helpers would be started.
//!
//! The proof is the job's own process list: a recording tree over `JobObjectTree` asks the job
//! what's in it just before it's stopped (restic's shell and the PowerShell) and just after (it
//! must be empty), and again when the command ends. A query that fails is a test failure, never
//! "empty". `tasklist` checks the PowerShell's PID as well. Every test stops what it started
//! even when an assertion fails.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use replica_core::cancel::CancelToken;
use replica_core::platform::{ProcessTree, Secret, TreeChild};
use replica_core::restic::{Repository, Restic, ResticError};
use replica_win::jobs::JobObjectTree;

/// A stand-in restic in `dir`: `version` answers as restic does; anything else runs `rest`, a
/// line of the script with `{program}` standing for a PowerShell that writes its PID to
/// `pid_file` and sleeps.
fn fake_restic(dir: &Path, pid_file: &Path, rest: &str) -> PathBuf {
    let program = format!(
        "powershell -NoProfile -NonInteractive -Command \"Set-Content -LiteralPath '{}' -Value $PID; Start-Sleep 120\"",
        pid_file.display()
    );
    let script = format!(
        "@echo off\r\nif \"%1\"==\"version\" (\r\n  echo {{\"message_type\":\"version\",\"version\":\"0.0.0-test\"}}\r\n  exit /b 0\r\n)\r\n{}\r\n",
        rest.replace("{program}", &program)
    );
    let exe = dir.join("restic.cmd");
    std::fs::write(&exe, script).unwrap();
    exe
}

fn repo(dir: &Path) -> Repository {
    // No `locks` folder, so a cancel has no lock files to look for.
    let location = dir.join("repo");
    std::fs::create_dir_all(&location).unwrap();
    Repository::new(location.display().to_string(), Secret::new("pw"))
}

fn read_pid(pid_file: &Path) -> u32 {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(pid_file)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the program didn't start"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Whether a process is running, by `tasklist`. A query that fails isn't an answer: it panics.
fn is_running(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .expect("tasklist runs");
    assert!(
        out.status.success(),
        "tasklist failed, so it can't tell: {out:?}"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.trim().is_empty(),
        "tasklist said nothing, so it can't tell"
    );
    text.contains(&format!("\"{pid}\""))
}

/// Stops the program whose PID is in the file, if it ever started, when dropped: made before
/// anything can fail, so no test outcome leaves it running.
struct KillOnDrop(PathBuf);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Ok(text) = std::fs::read_to_string(&self.0)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output();
        }
    }
}

/// What the job held at a moment: the job's own answer, or why it couldn't say.
type Listing = (&'static str, Result<Vec<u32>, String>);

/// `JobObjectTree`, recording what the job holds just before and after it's stopped, and when
/// the command ends.
#[derive(Default)]
struct Recording {
    restic_pid: Mutex<Option<u32>>,
    listings: Arc<Mutex<Vec<Listing>>>,
}

struct RecordingChild {
    inner: Box<dyn TreeChild>,
    listings: Arc<Mutex<Vec<Listing>>>,
}

impl RecordingChild {
    fn record(&self, when: &'static str) {
        let listed = self.inner.processes().map_err(|e| e.to_string());
        self.listings.lock().unwrap().push((when, listed));
    }
}

impl ProcessTree for Recording {
    fn spawn(&self, command: Command) -> std::io::Result<Box<dyn TreeChild>> {
        let inner = JobObjectTree.spawn(command)?;
        *self.restic_pid.lock().unwrap() = Some(inner.id());
        Ok(Box::new(RecordingChild {
            inner,
            listings: self.listings.clone(),
        }))
    }
}

impl TreeChild for RecordingChild {
    fn id(&self) -> u32 {
        self.inner.id()
    }
    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.inner.take_stdout()
    }
    fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.inner.take_stderr()
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.inner.try_wait()
    }
    fn kill_tree(&mut self) -> std::io::Result<()> {
        self.record("before stop");
        let stopped = self.inner.kill_tree();
        self.record("after stop");
        stopped
    }
    fn processes(&self) -> std::io::Result<Vec<u32>> {
        self.inner.processes()
    }
}

impl Drop for RecordingChild {
    fn drop(&mut self) {
        self.record("at end");
    }
}

impl Recording {
    fn listing(&self, when: &str) -> Vec<u32> {
        let listings = self.listings.lock().unwrap();
        let (_, listed) = listings
            .iter()
            .find(|(w, _)| *w == when)
            .unwrap_or_else(|| panic!("no listing {when}: {listings:?}"));
        listed
            .clone()
            .unwrap_or_else(|e| panic!("the job couldn't list its processes {when}: {e}"))
    }

    /// The job held the program (and restic's shell, if it was still running) before it was
    /// stopped, and nothing after.
    fn assert_stopped_through_the_job(&self, program: u32, restic_running: bool) {
        let restic = self.restic_pid.lock().unwrap().expect("restic was started");
        let before = self.listing("before stop");
        assert!(
            before.contains(&program),
            "before it was stopped, the job held the program ({program}): {before:?}"
        );
        assert_eq!(
            before.contains(&restic),
            restic_running,
            "restic ({restic}) in the job before it was stopped: {before:?}"
        );
        let after = self.listing("after stop");
        assert!(after.is_empty(), "the job is empty once stopped: {after:?}");
        let at_end = self.listing("at end");
        assert!(at_end.is_empty(), "and when the command ended: {at_end:?}");
    }
}

/// Cancels `prune` once the program has written its PID (it's running in the job by then).
fn cancel_prune(restic: &Restic, repo: &Repository, pid_file: &Path) -> ResticError {
    let cancel = CancelToken::new();
    let canceller = {
        let (cancel, pid_file) = (cancel.clone(), pid_file.to_path_buf());
        thread::spawn(move || {
            read_pid(&pid_file);
            cancel.cancel();
        })
    };
    let result = restic.prune(repo, &cancel);
    canceller.join().unwrap();
    result.expect_err("a cancelled prune is an error")
}

#[test]
fn a_cancel_stops_restic_and_what_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid.txt");
    let _cleanup = KillOnDrop(pid_file.clone());
    let exe = fake_restic(dir.path(), &pid_file, "{program}");
    let tree = Arc::new(Recording::default());
    let restic = Restic::new(&exe).unwrap().with_tree(tree.clone());
    let error = cancel_prune(&restic, &repo(dir.path()), &pid_file);
    assert_eq!(error, ResticError::Cancelled);
    let program = read_pid(&pid_file);
    tree.assert_stopped_through_the_job(program, true);
    assert!(!is_running(program), "and the program is gone");
}

/// What the tree is for: without it, a cancel stops restic alone. (This also shows the test
/// above can fail.)
#[test]
fn without_the_tree_a_cancel_stops_restic_alone() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid.txt");
    let _cleanup = KillOnDrop(pid_file.clone());
    let exe = fake_restic(dir.path(), &pid_file, "{program}");
    let restic = Restic::new(&exe).unwrap();
    let error = cancel_prune(&restic, &repo(dir.path()), &pid_file);
    assert_eq!(error, ResticError::Cancelled);
    assert!(
        is_running(read_pid(&pid_file)),
        "only restic itself was stopped"
    );
}

#[test]
fn a_program_left_running_when_restic_exits_is_stopped_with_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid.txt");
    let _cleanup = KillOnDrop(pid_file.clone());
    // It starts the program in the background, holding restic's output open, and exits.
    let exe = fake_restic(dir.path(), &pid_file, "start /B {program}\r\nexit /b 0");
    let tree = Arc::new(Recording::default());
    let restic = Restic::new(&exe).unwrap().with_tree(tree.clone());
    let start = Instant::now();
    let result = restic.unlock_stale(&repo(dir.path()));
    assert_eq!(result, Ok(()));
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "the command waited for the program: {:?}",
        start.elapsed()
    );
    let program = read_pid(&pid_file);
    tree.assert_stopped_through_the_job(program, false);
    assert!(!is_running(program), "and the program is gone");
}
