//! Command steps run in a Windows job object: a timeout, a cancel or
//! the end of the step stops the whole process tree, and the tests check that through the job's
//! own process list. A query that fails is a test failure, never "not running", and every test
//! stops what it started even when an assertion fails.

#![cfg(windows)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use replica_core::cancel::CancelToken;
use replica_core::platform::{ProcessTree, TreeChild};
use replica_core::steps::{StepError, run_command, shell};
use replica_win::jobs::JobObjectTree;

/// A command whose shell starts a long-running PowerShell, which writes its own PID to
/// `pid_file`: a program the shell started, a grandchild of Replica.
fn shell_starting_a_program(pid_file: &Path) -> String {
    format!(
        "powershell -NoProfile -NonInteractive -Command \"Set-Content -LiteralPath '{}' -Value $PID; Start-Sleep 120\"",
        pid_file.display()
    )
}

/// The PID the program wrote, if it has.
fn written_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

fn read_pid(pid_file: &Path) -> u32 {
    let start = Instant::now();
    loop {
        if let Some(pid) = written_pid(pid_file) {
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

/// Runs a step given its timeout and the PID file of the program its shell starts, first with a
/// short timeout and then with longer ones, until the program had started (written its PID)
/// before the step ended: (the step's result, the program's PID). PowerShell can take seconds
/// to start under load, and a timeout fixed long enough for that would be waited out in full on
/// every run. A program that hadn't started yet was stopped with its step's job.
fn with_a_started_program<T>(mut step: impl FnMut(Duration, &Path) -> T) -> (T, KillOnDrop) {
    let mut timeout = Duration::from_secs(2);
    loop {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let result = step(timeout, &pid_file);
        if let Some(pid) = written_pid(&pid_file) {
            return (result, KillOnDrop(pid));
        }
        assert!(
            timeout < Duration::from_secs(60),
            "the program never started"
        );
        timeout *= 2;
    }
}

/// Stops a process when dropped, whatever the test's outcome.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/PID", &self.0.to_string(), "/T", "/F"])
            .output();
    }
}

fn spawn_in_job(cmd: &str) -> Box<dyn TreeChild> {
    let mut command = shell(cmd);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    JobObjectTree
        .spawn(command)
        .expect("the job object is made and the shell put in it")
}

#[test]
fn everything_the_shell_starts_is_in_the_job_and_stopping_it_empties_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid.txt");
    let mut child = spawn_in_job(&shell_starting_a_program(&pid_file));
    let program = read_pid(&pid_file);
    let _cleanup = KillOnDrop(program);

    let inside = child.processes().expect("the job lists its processes");
    assert!(
        inside.contains(&child.id()),
        "the shell is in the job: {inside:?}"
    );
    assert!(
        inside.contains(&program),
        "the program it started is in the job: {inside:?}"
    );

    child
        .kill_tree()
        .expect("the job is terminated and empties");
    let after = child.processes().expect("the job lists its processes");
    assert!(after.is_empty(), "the job is empty: {after:?}");
    assert!(!is_running(program), "and the program is gone");
}

#[test]
fn closing_the_job_stops_what_is_left_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid.txt");
    let child = spawn_in_job(&shell_starting_a_program(&pid_file));
    let program = read_pid(&pid_file);
    let _cleanup = KillOnDrop(program);
    assert!(child.processes().unwrap().contains(&program));
    drop(child);
    let start = Instant::now();
    while is_running(program) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "kill-on-close didn't stop {program}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_step_that_times_out_stops_its_whole_tree() {
    let (result, program) = with_a_started_program(|timeout, pid_file| {
        run_command(
            &shell_starting_a_program(pid_file),
            timeout,
            &CancelToken::new(),
            &JobObjectTree,
            &mut |_| {},
        )
    });
    assert!(
        matches!(result, Err(StepError::TimedOut { .. })),
        "{result:?}"
    );
    assert!(
        !is_running(program.0),
        "the program the shell started was stopped with it"
    );
}

#[test]
fn a_cancelled_step_stops_its_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid.txt");
    let cmd = shell_starting_a_program(&pid_file);
    let cancel = CancelToken::new();
    let canceller = {
        let (cancel, pid_file) = (cancel.clone(), pid_file.clone());
        thread::spawn(move || {
            let pid = read_pid(&pid_file);
            cancel.cancel();
            pid
        })
    };
    let result = run_command(
        &cmd,
        Duration::from_secs(120),
        &cancel,
        &JobObjectTree,
        &mut |_| {},
    );
    let program = canceller.join().unwrap();
    let _cleanup = KillOnDrop(program);
    assert_eq!(result, Err(StepError::Cancelled));
    assert!(
        !is_running(program),
        "the program the shell started was stopped with it"
    );
}

#[test]
fn a_program_left_running_by_a_finished_step_is_stopped_with_the_step() {
    // The shell starts the program in the background and exits at once; the program holds the
    // step's output open, so the step waits for it until the timeout.
    let ((result, lines), program) = with_a_started_program(|timeout, pid_file| {
        let mut lines = Vec::new();
        let result = run_command(
            &format!("start /B {}", shell_starting_a_program(pid_file)),
            timeout,
            &CancelToken::new(),
            &JobObjectTree,
            &mut |l| lines.push(l.to_string()),
        );
        (result, lines)
    });
    assert_eq!(result, Ok(()), "{lines:?}");
    let program = program.0;
    let start = Instant::now();
    while is_running(program) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{program} outlived its step"
        );
        thread::sleep(Duration::from_millis(50));
    }
}
