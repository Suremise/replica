//! Command steps: a job's `before` and `after` commands, with a timeout.
//!
//! A command line runs through the system shell (`cmd /D /S /C` on Windows, `sh -c`
//! elsewhere), so it can use the shell's syntax. Its output goes to the run's log. A non-zero
//! exit code, the timeout or a cancel ends it with a [`StepError`].
//!
//! - **The command runs in a [`ProcessTree`]**: on Windows a job object
//!   with kill-on-close, from `replica-win`. On a timeout or a cancel the whole tree is stopped,
//!   through the tree itself, and the step returns only once the tree has no process left. If
//!   the tree can't be made, or can't be stopped, that's the step's error, never ignored. When
//!   the step ends, anything still in the tree is stopped.
//! - **Waiting for output is bounded by the timeout too.** A program the command left running
//!   in the background can keep its output open after the shell exits; the step stops reading
//!   at the deadline instead of waiting for it.

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::cancel::CancelToken;
use crate::platform::ProcessTree;
use crate::tr;

/// How many lines of a failed command's output go in its error.
const TAIL_LINES: usize = 10;

/// Why a command step failed. Its text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    /// It couldn't be started, or its process tree couldn't be made.
    Spawn { cmd: String, detail: String },
    /// It ran longer than its timeout, and its whole tree was stopped.
    TimedOut { cmd: String, secs: u64 },
    /// It exited with a non-zero code; `output` is its last lines.
    Failed {
        cmd: String,
        code: Option<i32>,
        output: String,
    },
    /// It was cancelled, and its whole tree was stopped.
    Cancelled,
    /// It was to be stopped (timeout or cancel), but its tree couldn't be: its programs may
    /// still be running. `detail` says why.
    NotStopped { cmd: String, detail: String },
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            StepError::Spawn { cmd, detail } => {
                tr!(
                    "error-step-spawn",
                    step = cmd.as_str(),
                    detail = detail.as_str()
                )
            }
            StepError::TimedOut { cmd, secs } => tr!(
                "error-step-timed-out",
                step = cmd.as_str(),
                timeout = format!("{secs} s")
            ),
            StepError::Failed { cmd, code, output } => tr!(
                "error-step-exit-code",
                step = cmd.as_str(),
                code = code.map_or("?".to_string(), |c| c.to_string()),
                output = output.as_str()
            ),
            StepError::Cancelled => crate::i18n::tr("error-cancelled"),
            StepError::NotStopped { cmd, detail } => tr!(
                "error-step-not-stopped",
                step = cmd.as_str(),
                detail = detail.as_str()
            ),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for StepError {}

/// Runs `cmd` through the shell inside `tree`, sending each line of its output to `log`, and
/// waits up to `timeout` for it to finish with exit code 0.
pub fn run_command(
    cmd: &str,
    timeout: Duration,
    cancel: &CancelToken,
    tree: &dyn ProcessTree,
    log: &mut dyn FnMut(&str),
) -> Result<(), StepError> {
    let mut command = shell(cmd);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = tree.spawn(command).map_err(|e| StepError::Spawn {
        cmd: cmd.to_string(),
        detail: e.to_string(),
    })?;
    let (tx, rx) = mpsc::channel::<String>();
    let readers: Vec<_> = [
        child
            .take_stdout()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .take_stderr()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                let line = String::from_utf8_lossy(&line)
                    .trim_end_matches('\r')
                    .to_string();
                if tx.send(line).is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(tx);

    let mut tail: Vec<String> = Vec::new();
    let mut keep = |line: String, log: &mut dyn FnMut(&str)| {
        log(&line);
        tail.push(line);
        if tail.len() > TAIL_LINES {
            tail.remove(0);
        }
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            keep(line, log);
        }
        if let Some(status) = child.try_wait().map_err(|e| StepError::Spawn {
            cmd: cmd.to_string(),
            detail: e.to_string(),
        })? {
            break status;
        }
        let ended = if Instant::now() >= deadline {
            Some(StepError::TimedOut {
                cmd: cmd.to_string(),
                secs: timeout.as_secs(),
            })
        } else if cancel.wait_timeout(Duration::from_millis(50)) {
            Some(StepError::Cancelled)
        } else {
            None
        };
        if let Some(why) = ended {
            // A tree that couldn't be stopped is said so, never passed off as stopped.
            return Err(match child.kill_tree() {
                Ok(()) => why,
                Err(e) => {
                    log(&why.to_string());
                    StepError::NotStopped {
                        cmd: cmd.to_string(),
                        detail: e.to_string(),
                    }
                }
            });
        }
    };
    // The rest of the output, until the readers end or the deadline: a background program the
    // command started may hold the output open after the shell is gone. (Dropping the child at
    // the end stops anything still in its tree.)
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) => keep(line, log),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                for reader in readers {
                    let _ = reader.join();
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The readers end by themselves when the output closes.
                log(&tr!("warning-step-output-open"));
                break;
            }
        }
    }
    if status.success() {
        Ok(())
    } else {
        Err(StepError::Failed {
            cmd: cmd.to_string(),
            code: status.code(),
            output: tail.join("\n"),
        })
    }
}

/// The system shell, running `cmd` as typed.
pub fn shell(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = crate::process::command("cmd");
        // /S with the whole line in quotes: cmd runs what's inside exactly as typed.
        c.args(["/D", "/S", "/C"]).raw_arg(format!("\"{cmd}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = crate::process::command("sh");
        c.args(["-c", cmd]);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::PlainTree;

    fn run(cmd: &str, secs: u64) -> (Result<(), StepError>, Vec<String>) {
        let mut lines = Vec::new();
        let result = run_command(
            cmd,
            Duration::from_secs(secs),
            &CancelToken::new(),
            &PlainTree,
            &mut |l| lines.push(l.to_string()),
        );
        (result, lines)
    }

    /// A command that runs until it's stopped, as the shell process alone (no program of its own),
    /// so stopping the shell leaves nothing running: these tests run without a real process tree.
    /// The tree itself is tested in `crates/replica/tests/command_tree.rs`, with the job object.
    fn slow_command() -> String {
        if cfg!(windows) {
            // A loop inside cmd itself.
            "for /L %i in (0,0,1) do @rem".into()
        } else {
            // exec: the shell becomes sleep, so they're one process.
            "exec sleep 30".into()
        }
    }

    #[test]
    fn output_is_logged_and_success_is_ok() {
        let (result, lines) = run("echo hello && echo \"quoted words\"", 30);
        assert_eq!(result, Ok(()));
        assert_eq!(lines[0].trim(), "hello");
        assert!(lines[1].contains("quoted words"), "{lines:?}");
    }

    #[test]
    fn a_non_zero_exit_code_fails_with_the_last_output() {
        let (result, _) = run("echo something went wrong && exit 3", 30);
        let Err(StepError::Failed { code, output, .. }) = result else {
            panic!("{result:?}")
        };
        assert_eq!(code, Some(3));
        assert!(output.contains("something went wrong"));
    }

    #[test]
    fn a_command_that_runs_too_long_times_out() {
        let start = Instant::now();
        let (result, _) = run(&slow_command(), 1);
        assert!(
            matches!(result, Err(StepError::TimedOut { secs: 1, .. })),
            "{result:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_cancel_stops_it() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = run_command(
            &slow_command(),
            Duration::from_secs(60),
            &cancel,
            &PlainTree,
            &mut |_| {},
        );
        assert_eq!(result, Err(StepError::Cancelled));
    }

    #[test]
    fn a_missing_program_fails() {
        let (result, _) = run("replica-no-such-program-xyz", 30);
        assert!(
            matches!(result, Err(StepError::Failed { .. })),
            "{result:?}"
        );
    }

    /// A tree that can't be stopped.
    struct Stubborn;

    impl ProcessTree for Stubborn {
        fn spawn(&self, command: Command) -> std::io::Result<Box<dyn crate::platform::TreeChild>> {
            struct Child(Box<dyn crate::platform::TreeChild>);
            impl crate::platform::TreeChild for Child {
                fn id(&self) -> u32 {
                    self.0.id()
                }
                fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
                    self.0.take_stdout()
                }
                fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
                    self.0.take_stderr()
                }
                fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
                    self.0.try_wait()
                }
                fn kill_tree(&mut self) -> std::io::Result<()> {
                    Err(std::io::Error::other("the tree refused"))
                }
                fn processes(&self) -> std::io::Result<Vec<u32>> {
                    self.0.processes()
                }
            }
            Ok(Box::new(Child(PlainTree.spawn(command)?)))
        }
    }

    #[test]
    fn a_tree_that_cant_be_stopped_is_reported_not_passed_off() {
        let result = run_command(
            &slow_command(),
            Duration::from_secs(1),
            &CancelToken::new(),
            &Stubborn,
            &mut |_| {},
        );
        let Err(StepError::NotStopped { detail, .. }) = result else {
            panic!("{result:?}")
        };
        assert!(detail.contains("the tree refused"));
    }

    /// A tree that can't be made.
    struct Unmakeable;

    impl ProcessTree for Unmakeable {
        fn spawn(&self, _: Command) -> std::io::Result<Box<dyn crate::platform::TreeChild>> {
            Err(std::io::Error::other("no job object"))
        }
    }

    #[test]
    fn a_tree_that_cant_be_made_is_an_error() {
        let result = run_command(
            "echo hi",
            Duration::from_secs(5),
            &CancelToken::new(),
            &Unmakeable,
            &mut |_| {},
        );
        assert!(
            matches!(&result, Err(StepError::Spawn { detail, .. }) if detail.contains("no job object")),
            "{result:?}"
        );
    }
}
