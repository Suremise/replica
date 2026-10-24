//! Running one restic command: its environment, its output, cancellation.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::messages::{ExitError, Line};
use super::{Repository, Restic, ResticError};
use crate::cancel::CancelToken;

/// How many lines of restic's non-JSON output are kept for an error message.
const TAIL_LINES: usize = 20;

/// How often restic reports backup progress.
const PROGRESS_FPS: &str = "2";

/// How long a program restic started may keep its output open after restic has exited, before
/// the tree is stopped so the command can end.
const LEFTOVER_GRACE: Duration = Duration::from_secs(5);

/// How often a running command checks for a cancel and for restic's exit.
const POLL: Duration = Duration::from_millis(50);

/// What a finished command left.
#[derive(Debug, Default)]
pub(crate) struct Finished {
    pub code: i32,
    /// All of stdout, if asked for.
    pub stdout: String,
    /// The `exit_error` restic printed, if any.
    pub exit_error: Option<ExitError>,
    /// The last lines of output that weren't JSON, for errors.
    pub tail: Vec<String>,
}

impl Finished {
    /// restic's own message for a failure, or its last output.
    pub fn message(&self) -> String {
        match &self.exit_error {
            Some(e) if !e.message.is_empty() => e.message.trim().to_string(),
            _ => self.tail.join("\n"),
        }
    }
}

/// The arguments as they're logged: never anything secret, since the password is only ever in
/// the environment.
pub(crate) fn describe(args: &[OsString]) -> String {
    let parts: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    format!("restic {}", parts.join(" "))
}

/// Runs restic with `args` against `repo`.
///
/// - The repository and its password go in `RESTIC_REPOSITORY` and `RESTIC_PASSWORD`, never on
///   the command line. Any other `RESTIC_*` variables inherited from the environment are removed,
///   so a stray `RESTIC_PASSWORD_COMMAND` can't change what runs.
/// - `on_line` gets each JSON line of stdout and stderr as it arrives.
/// - restic runs in `restic`'s process tree. If `cancel` fires, the whole tree is stopped and
///   [`ResticError::Cancelled`] returned, after the locks restic held are removed (see
///   [`super::remove_locks_of`]). A program restic started that still holds the output open
///   [`LEFTOVER_GRACE`] after restic exits is stopped with the tree, and anything left in the
///   tree when the command ends is stopped too. A tree that had to be stopped and couldn't be is
///   [`ResticError::NotStopped`], at once, never a success or a cancel.
pub(crate) fn run(
    restic: &Restic,
    repo: &Repository,
    args: &[OsString],
    collect_stdout: bool,
    cancel: Option<&CancelToken>,
    on_line: &mut dyn FnMut(&Line),
) -> Result<Finished, ResticError> {
    if cancel.is_some_and(CancelToken::is_cancelled) {
        return Err(ResticError::Cancelled);
    }
    let exe = restic.path();
    let mut cmd = crate::process::command(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RESTIC_")
        {
            cmd.env_remove(key);
        }
    }
    cmd.env("RESTIC_REPOSITORY", &repo.location)
        .env("RESTIC_PASSWORD", repo.password.expose())
        .env("RESTIC_PROGRESS_FPS", PROGRESS_FPS);
    if let Some(cache) = repo.cache_dir.as_ref().or(restic.cache_dir.as_ref()) {
        cmd.env("RESTIC_CACHE_DIR", cache);
    }

    tracing::info!(target: "replica::restic", repository = %repo.location, "{}", describe(args));
    let spawn_error = |e: std::io::Error| ResticError::Spawn {
        path: exe.to_path_buf(),
        detail: e.to_string(),
    };
    // Dropping it at the end stops anything still in its tree.
    let mut child = restic.tree.spawn(cmd).map_err(spawn_error)?;
    let pid = child.id();

    // Both streams are read on threads, so a cancel is seen even while restic is quiet. (A tree
    // may hand back no stream: then there's nothing to read from it.)
    let (tx, rx) = mpsc::channel::<(bool, String)>();
    let streams: [(bool, Option<Box<dyn Read + Send>>); 2] = [
        (true, child.take_stdout().map(|s| Box::new(s) as _)),
        (false, child.take_stderr().map(|s| Box::new(s) as _)),
    ];
    for (is_stdout, stream) in streams {
        let Some(stream) = stream else { continue };
        let tx = tx.clone();
        thread::spawn(move || {
            for line in lines(stream) {
                if tx.send((is_stdout, line)).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let mut out = Finished::default();
    let mut handle = |is_stdout: bool, line: String| {
        if is_stdout && collect_stdout {
            out.stdout.push_str(&line);
            out.stdout.push('\n');
        }
        match serde_json::from_str::<Line>(&line) {
            Ok(parsed) => {
                if let Line::ExitError(e) = &parsed {
                    out.exit_error = Some(e.clone());
                }
                on_line(&parsed);
            }
            Err(_)
                if !line.trim().is_empty()
                    && !line.trim_start().starts_with(['{', '[', '}', ']', '"']) =>
            {
                tracing::debug!(target: "replica::restic", "{line}");
                out.tail.push(line);
                if out.tail.len() > TAIL_LINES {
                    out.tail.remove(0);
                }
            }
            Err(_) => {}
        }
    };

    // The command is done once restic has exited and its output has closed (nothing it started
    // holds it open). Until then, the cancel and restic's exit are checked every POLL, whether or
    // not the output is still open: a program can close its output and keep running.
    let mut output_open = true;
    let mut exited: Option<(std::process::ExitStatus, Instant)> = None;
    let mut leftovers_stopped = false;
    // So the first pass checks at once.
    let mut last_check = Instant::now()
        .checked_sub(POLL)
        .unwrap_or_else(Instant::now);
    loop {
        if output_open {
            match rx.recv_timeout(POLL) {
                Ok((is_stdout, line)) => handle(is_stdout, line),
                Err(mpsc::RecvTimeoutError::Disconnected) => output_open = false,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        } else {
            thread::sleep(POLL.saturating_sub(last_check.elapsed()));
        }
        // Checked between lines too, so a stream of output can't hold off a cancel.
        if last_check.elapsed() < POLL {
            continue;
        }
        last_check = Instant::now();
        if exited.is_none()
            && let Some(status) = child.try_wait().map_err(spawn_error)?
        {
            exited = Some((status, Instant::now()));
        }
        match exited {
            Some((status, _)) if !output_open => {
                out.code = status.code().unwrap_or(-1);
                break;
            }
            _ if cancel.is_some_and(CancelToken::is_cancelled) => {
                // A tree that couldn't be stopped is said so, never passed off as stopped.
                if let Err(e) = child.kill_tree() {
                    let detail = e.to_string();
                    tracing::warn!(target: "replica::restic", "cancelled, but restic's tree (PID {pid}) couldn't be stopped: {detail}");
                    return Err(ResticError::NotStopped { detail });
                }
                tracing::info!(target: "replica::restic", "cancelled; restic (PID {pid}) and its tree were stopped");
                drop(child);
                super::remove_locks_of(restic, repo, pid);
                return Err(ResticError::Cancelled);
            }
            Some((_, at)) if !leftovers_stopped && at.elapsed() >= LEFTOVER_GRACE => {
                tracing::info!(target: "replica::restic", "restic exited, but a program it started kept its output open; stopping its tree");
                leftovers_stopped = true;
                if let Err(e) = child.kill_tree() {
                    let detail = e.to_string();
                    tracing::warn!(target: "replica::restic", "restic's tree (PID {pid}) couldn't be stopped: {detail}");
                    return Err(ResticError::NotStopped { detail });
                }
            }
            // The tree is stopped, and what still holds the output open isn't in it (a plain
            // tree holds restic alone): don't wait for it any longer.
            Some((status, at)) if at.elapsed() >= 2 * LEFTOVER_GRACE => {
                tracing::warn!(target: "replica::restic", "restic's output is still open after it exited; not waiting for it");
                out.code = status.code().unwrap_or(-1);
                break;
            }
            _ => {}
        }
    }
    if out.code != 0 {
        tracing::info!(target: "replica::restic", code = out.code, "restic exited: {}", out.message());
    }
    Ok(out)
}

/// Lines of a stream, decoded leniently (restic writes UTF-8; anything else is replaced).
fn lines(stream: impl Read) -> impl Iterator<Item = String> {
    BufReader::new(stream)
        .split(b'\n')
        .map_while(Result::ok)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .trim_end_matches('\r')
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn descriptions_are_the_arguments_only() {
        let args: Vec<OsString> = ["backup", "--json", "C:\\src"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(describe(&args), r"restic backup --json C:\src");
    }

    #[test]
    fn a_failure_message_prefers_restics_own() {
        let f = Finished {
            code: 12,
            exit_error: Some(ExitError {
                code: 12,
                message: "Fatal: wrong password\n".into(),
            }),
            tail: vec!["noise".into()],
            ..Default::default()
        };
        assert_eq!(f.message(), "Fatal: wrong password");
        let g = Finished {
            code: 1,
            tail: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert_eq!(g.message(), "a\nb");
    }

    use crate::platform::{PlainTree, ProcessTree, Secret, TreeChild};
    use std::sync::{Arc, Mutex};

    /// How a [`Watched`] tree behaves.
    #[derive(Default, Clone, Copy)]
    struct Setup {
        /// Start the program with no output streams.
        no_output: bool,
        /// Fail to stop the tree.
        refuse_kill: bool,
        /// Say restic has exited from the start, while the program (then standing for a helper
        /// restic left) keeps running and holds the output open.
        reports_exited: bool,
    }

    /// A plain tree (the program alone) that says when the program has started, and records,
    /// at each stop, whether the program was still running. Dropping its child stops the
    /// program, whatever the test's outcome, so no test leaves one running.
    struct Watched {
        started: Mutex<mpsc::Sender<u32>>,
        setup: Setup,
        running_at_stop: Arc<Mutex<Vec<bool>>>,
    }

    struct WatchedChild {
        inner: Box<dyn TreeChild>,
        setup: Setup,
        running_at_stop: Arc<Mutex<Vec<bool>>>,
    }

    impl ProcessTree for Watched {
        fn spawn(&self, mut command: Command) -> std::io::Result<Box<dyn TreeChild>> {
            if self.setup.no_output {
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
            let inner = PlainTree.spawn(command)?;
            let _ = self.started.lock().unwrap().send(inner.id());
            Ok(Box::new(WatchedChild {
                inner,
                setup: self.setup,
                running_at_stop: self.running_at_stop.clone(),
            }))
        }
    }

    impl TreeChild for WatchedChild {
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
            if self.setup.reports_exited {
                return Ok(Some(std::process::ExitStatus::default()));
            }
            self.inner.try_wait()
        }
        fn kill_tree(&mut self) -> std::io::Result<()> {
            let running = self.inner.try_wait()?.is_none();
            self.running_at_stop.lock().unwrap().push(running);
            if self.setup.refuse_kill {
                return Err(std::io::Error::other("the tree refused"));
            }
            self.inner.kill_tree()
        }
        fn processes(&self) -> std::io::Result<Vec<u32>> {
            self.inner.processes()
        }
    }

    /// A "restic" in a [`Watched`] tree; the receiver told when it has started; and, for each
    /// stop of its tree, whether the program was still running then.
    fn restic_in(exe: &str, setup: Setup) -> (Restic, mpsc::Receiver<u32>, Arc<Mutex<Vec<bool>>>) {
        let (tx, rx) = mpsc::channel();
        let running_at_stop = Arc::new(Mutex::new(Vec::new()));
        let tree = Watched {
            started: Mutex::new(tx),
            setup,
            running_at_stop: running_at_stop.clone(),
        };
        let restic = Restic {
            path: exe.into(),
            version: String::new(),
            tree: Arc::new(tree),
            cache_dir: None,
        };
        (restic, rx, running_at_stop)
    }

    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// A program that runs until it's stopped, as one process, and its arguments.
    fn slow() -> (&'static str, Vec<OsString>) {
        if cfg!(windows) {
            ("ping", args(&["-n", "60", "127.0.0.1"]))
        } else {
            ("sleep", args(&["60"]))
        }
    }

    fn repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::new(dir.path().display().to_string(), Secret::new("pw"));
        (dir, repo)
    }

    /// Runs it, cancelling once the program has started (never before). Returns the result and
    /// whether the program had started when the cancel came.
    fn cancelled_run(
        restic: &Restic,
        started: mpsc::Receiver<u32>,
        args: &[OsString],
    ) -> (Result<Finished, ResticError>, bool) {
        let (_dir, repo) = repo();
        let cancel = CancelToken::new();
        let canceller = {
            let cancel = cancel.clone();
            thread::spawn(move || {
                let was_started = started.recv_timeout(Duration::from_secs(60)).is_ok();
                cancel.cancel();
                was_started
            })
        };
        let result = run(restic, &repo, args, false, Some(&cancel), &mut |_| {});
        (result, canceller.join().unwrap())
    }

    #[test]
    fn a_cancel_stops_it_even_while_its_quiet() {
        let (exe, args) = slow();
        let (restic, started, _) = restic_in(exe, Setup::default());
        let start = Instant::now();
        let (result, was_started) = cancelled_run(&restic, started, &args);
        assert!(was_started, "the cancel came once it was running");
        assert!(matches!(result, Err(ResticError::Cancelled)), "{result:?}");
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_cancel_stops_a_program_that_closed_its_output_and_kept_running() {
        // Started with no output at all: its output is "closed" from the start, while it runs.
        let (exe, args) = slow();
        let (restic, started, _) = restic_in(
            exe,
            Setup {
                no_output: true,
                ..Setup::default()
            },
        );
        let start = Instant::now();
        let (result, was_started) = cancelled_run(&restic, started, &args);
        assert!(was_started);
        assert!(matches!(result, Err(ResticError::Cancelled)), "{result:?}");
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "it waited for the program to end by itself"
        );
    }

    #[test]
    fn a_tree_that_cant_be_stopped_on_a_cancel_is_reported_at_once_not_passed_off() {
        let (exe, args) = slow();
        let (restic, started, _) = restic_in(
            exe,
            Setup {
                refuse_kill: true,
                ..Setup::default()
            },
        );
        let start = Instant::now();
        let (result, was_started) = cancelled_run(&restic, started, &args);
        assert!(was_started);
        let Err(ResticError::NotStopped { detail }) = result else {
            panic!("{result:?}")
        };
        assert!(detail.contains("the tree refused"));
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "it didn't wait for restic to end by itself"
        );
    }

    #[test]
    fn a_helper_left_holding_the_output_whose_tree_cant_be_stopped_is_not_a_success() {
        // restic "has exited" from the start, while the program, standing for a helper it left,
        // runs far longer than the grace and holds the output open. The test owns it: dropping
        // the tree's child stops it, whatever the outcome.
        let (exe, args) = slow();
        let (restic, _started, running_at_stop) = restic_in(
            exe,
            Setup {
                refuse_kill: true,
                reports_exited: true,
                ..Setup::default()
            },
        );
        let (_dir, repo) = repo();
        let start = Instant::now();
        let result = run(&restic, &repo, &args, false, None, &mut |_| {});
        let Err(ResticError::NotStopped { detail }) = result else {
            panic!("{result:?}")
        };
        assert!(detail.contains("the tree refused"));
        assert_eq!(
            *running_at_stop.lock().unwrap(),
            [true],
            "one stop, made while the helper still held the output open"
        );
        assert!(start.elapsed() < Duration::from_secs(30), "hung");
    }

    /// A tree that can't be made.
    struct Unmakeable;

    impl ProcessTree for Unmakeable {
        fn spawn(&self, _: Command) -> std::io::Result<Box<dyn TreeChild>> {
            Err(std::io::Error::other("no job object"))
        }
    }

    #[test]
    fn a_tree_that_cant_be_made_is_a_spawn_error() {
        let (exe, args) = slow();
        let restic = Restic {
            path: exe.into(),
            version: String::new(),
            tree: Arc::new(Unmakeable),
            cache_dir: None,
        };
        let (_dir, repo) = repo();
        let result = run(&restic, &repo, &args, false, None, &mut |_| {});
        assert!(
            matches!(&result, Err(ResticError::Spawn { detail, .. }) if detail.contains("no job object")),
            "{result:?}"
        );
    }
}
