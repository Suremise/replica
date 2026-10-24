//! Jobs that share a repository take turns across processes: `replica.exe` runs started
//! together, as Task Scheduler or the CLI beside the tray would start them, with the pinned
//! restic, in temp folders only.
//!
//! Each test has its own `APPDATA`, `LOCALAPPDATA` (so restic's cache is in the temp folder too)
//! and Credential Manager prefix (deleted when it ends), and drive targets on the temp folder's
//! own volume, by volume ID, as `tests/cli.rs` does. Every `replica.exe` a test starts is killed
//! and reaped however the test ends ([`Running`]). To make
//! runs meet for certain, the test itself first holds the repository's lock (the same named
//! mutex every Replica process uses), starts the runs, waits until each says it's waiting, and
//! only then lets go. Nothing waits for a length of time.

#![cfg(windows)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use replica_core::platform::SecretStore;
use replica_core::repo_lock::{RepositoryLocks, lock_key};
use replica_core::settings::Settings;
use replica_win::credentials::CredentialManager;
use replica_win::repo_locks::NamedMutexLocks;

/// How long a test waits for a run to say something before it gives up: generous, for a slow
/// machine; it never waits this long when things work.
const LIMIT: Duration = Duration::from_secs(120);

struct Fixture {
    temp: tempfile::TempDir,
    prefix: String,
    restic: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Option<Fixture> {
        let restic = restic()?;
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("appdata/Replica")).unwrap();
        std::fs::create_dir_all(temp.path().join("localappdata")).unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/a.txt"), "a").unwrap();
        Some(Fixture {
            prefix: format!("ReplicaTest-{}-repolock-{name}/", std::process::id()),
            restic,
            temp,
        })
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    /// A drive target on the temp folder's own volume, at `rel` in the temp folder.
    fn drive_target(&self, rel: &str) -> String {
        let full = std::fs::canonicalize(self.temp.path()).unwrap().join(rel);
        let text = full
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        let id = replica_win::targets::volume_id_of(&text[..3]).unwrap();
        format!(
            "kind = \"drive\"\nvolume_id = '{id}'\npath = '{}'\n",
            &text[3..]
        )
    }

    /// Versioned jobs, each `(name, repository folder)`, and the app settings `app`.
    fn settings(&self, app: &str, jobs: &[(&str, &str)]) {
        let mut text = format!("[app]\n{app}\n");
        for (name, repo) in jobs {
            text.push_str(&format!(
                "[[jobs]]\nname = \"{name}\"\n[[jobs.sources]]\npath = '{}'\nexclude = []\n[[jobs.targets]]\n{}\n",
                self.path("src").display(),
                self.drive_target(repo)
            ));
        }
        std::fs::write(self.path("appdata/Replica/settings.toml"), text).unwrap();
    }

    /// The lock key of `job`'s first target, from the settings as a run reads them.
    fn key_of(&self, job: &str) -> String {
        let settings = Settings::load(&self.path("appdata/Replica/settings.toml"))
            .unwrap()
            .settings;
        let job = settings.jobs.iter().find(|j| j.name == job).unwrap();
        lock_key(&job.targets[0])
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_replica"));
        cmd.args(args)
            .env("APPDATA", self.path("appdata"))
            // restic keeps its cache under LOCALAPPDATA: here, not in the user's.
            .env("LOCALAPPDATA", self.path("localappdata"))
            .env(replica::cli::CREDENTIAL_PREFIX_VAR, &self.prefix)
            .env("REPLICA_RESTIC", &self.restic)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Runs `replica.exe` to the end: (exit code, stdout, stderr).
    fn replica(&self, args: &[&str], stdin: &str) -> (i32, String, String) {
        let mut child = self.command(args).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Sets up the repository `job` uses: its password is stored, shared by every job on it.
    fn init(&self, job: &str) {
        let (code, out, err) = self.replica(&["init", job], "saved\n");
        assert_eq!(code, 0, "init {job}: {out}\n{err}");
    }

    /// Starts `replica.exe` with `args`, its output read line by line as it comes.
    fn start(&self, args: &[&str]) -> Running {
        self.start_with(args, "")
    }

    /// As [`Self::start`], with `stdin` fed to it (then closed).
    fn start_with(&self, args: &[&str], stdin: &str) -> Running {
        let mut child = self.command(args).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin.as_bytes()).unwrap();
        drop(input);
        let (send, lines) = channel();
        let mut readers = Vec::new();
        for (is_stdout, stream) in [
            (
                true,
                Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            ),
            (false, Box::new(child.stderr.take().unwrap())),
        ] {
            let send = send.clone();
            readers.push(std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let _ = send.send((is_stdout, line));
                }
            }));
        }
        Running {
            name: args.join(" "),
            child,
            lines,
            seen: Vec::new(),
            readers,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let store = CredentialManager::with_prefix(&self.prefix);
        for name in store.names().unwrap_or_default() {
            let _ = store.delete(&name);
        }
    }
}

/// A `replica.exe` started by [`Fixture::start`]. Dropping it kills and reaps it if it's still
/// running, so a test that fails never leaves it waiting on a repository.
struct Running {
    name: String,
    child: Child,
    /// Each line, and whether it's from stdout.
    lines: Receiver<(bool, String)>,
    seen: Vec<(bool, String)>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Running {
    /// Waits for a line of its output (stdout or stderr) that contains `text`.
    fn wait_for(&mut self, text: &str) {
        if self.seen.iter().any(|(_, l)| l.contains(text)) {
            return;
        }
        loop {
            match self.lines.recv_timeout(LIMIT) {
                Ok(line) => {
                    let found = line.1.contains(text);
                    self.seen.push(line);
                    if found {
                        return;
                    }
                }
                Err(e) => panic!(
                    "{}: no \"{text}\" ({e:?}) in:\n{}",
                    self.name,
                    self.text(None)
                ),
            }
        }
    }

    /// Its output so far: stdout (`Some(true)`), stderr (`Some(false)`) or both.
    fn text(&self, stdout: Option<bool>) -> String {
        let lines: Vec<&str> = self
            .seen
            .iter()
            .filter(|(out, _)| stdout.is_none_or(|s| s == *out))
            .map(|(_, l)| l.as_str())
            .collect();
        lines.join("\n")
    }

    /// Waits for it to end: (exit code, all its output, its stdout alone).
    fn finish(mut self) -> (i32, String, String) {
        let code = self.child.wait().unwrap().code().unwrap_or(-1);
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        let rest: Vec<(bool, String)> = self.lines.try_iter().collect();
        self.seen.extend(rest);
        (code, self.text(None), self.text(Some(true)))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The pinned restic, as the other tests find it; `None` skips (unless restic is required).
fn restic() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("REPLICA_RESTIC") {
        return Some(PathBuf::from(path));
    }
    let bundled =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/restic/bin/restic.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    assert!(
        std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
        "restic is required (REPLICA_REQUIRE_RESTIC=1) but wasn't found"
    );
    None
}

fn says_locked(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("already locked") || lower.contains("in use by another restic")
}

#[test]
fn two_jobs_on_one_repository_started_together_both_succeed_in_turn() {
    let Some(f) = Fixture::new("two") else {
        return;
    };
    f.settings("", &[("one", "drive/repo"), ("two", "drive/repo")]);
    f.init("one");
    let held = NamedMutexLocks::new()
        .try_lock(&f.key_of("one"))
        .unwrap()
        .expect("the repository is free");

    let mut one = f.start(&["run", "one"]);
    let mut two = f.start(&["run", "two", "--json"]);
    one.wait_for("Waiting for another job to finish with");
    two.wait_for("Waiting for another job to finish with");
    drop(held);
    let (one_code, one_out, _) = one.finish();
    let (two_code, two_out, two_doc) = two.finish();

    assert_eq!(one_code, 0, "{one_out}");
    assert_eq!(two_code, 0, "{two_out}");
    assert!(
        !says_locked(&one_out) && !says_locked(&two_out),
        "{one_out}\n{two_out}"
    );
    assert!(
        one_out.contains("Waiting for two to finish")
            || two_out.contains("Waiting for one to finish"),
        "one waited for the other, by name:\n{one_out}\n---\n{two_out}"
    );
    // --json: the wait is in the document (stdout), and its message went to stderr.
    let doc: serde_json::Value = serde_json::from_str(&two_doc).unwrap();
    assert!(
        !two_doc.contains("Waiting for"),
        "with --json, the message is on stderr: {two_doc}"
    );
    assert_eq!(doc["outcome"], "ok", "{doc}");
    let waits = doc["jobs"][0]["waits"].as_array().unwrap();
    assert!(!waits.is_empty(), "{doc}");
    assert_eq!(waits[0]["kind"], "turn");
    assert_eq!(waits[0]["ended"], "ok");
    assert!(waits[0]["duration_ms"].as_u64().is_some());

    let status = f.replica(&["list", "--json"], "");
    let list: serde_json::Value = serde_json::from_str(&status.1).unwrap();
    for job in list["jobs"].as_array().unwrap() {
        assert_eq!(job["last_run"]["outcome"], "ok", "{job}");
    }
}

#[test]
fn three_jobs_on_one_repository_queue_while_one_elsewhere_runs_meanwhile() {
    let Some(f) = Fixture::new("three") else {
        return;
    };
    f.settings(
        "",
        &[
            ("s1", "drive/repo"),
            ("s2", "drive/repo"),
            ("s3", "drive/repo"),
            ("elsewhere", "drive/other"),
        ],
    );
    f.init("s1");
    f.init("elsewhere");
    let held = NamedMutexLocks::new()
        .try_lock(&f.key_of("s1"))
        .unwrap()
        .expect("the repository is free");

    let mut shared: Vec<Running> = ["s1", "s2", "s3"]
        .iter()
        .map(|job| f.start(&["run", job]))
        .collect();
    for run in &mut shared {
        run.wait_for("Waiting for another job to finish with");
    }
    // The three wait; a job on another repository runs to the end meanwhile.
    let (code, out, err) = f.replica(&["run", "elsewhere"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(!out.contains("Waiting"), "{out}");
    drop(held);
    let outputs: Vec<(i32, String, String)> = shared.into_iter().map(Running::finish).collect();
    for (code, out, _) in &outputs {
        assert_eq!(*code, 0, "{out}");
        assert!(!says_locked(out), "{out}");
    }

    // Run all, in one process: one job after another, nothing waits or races.
    let (code, out, err) = f.replica(&["run", "--all", "--json"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    let jobs = doc["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 4);
    for job in jobs {
        assert_eq!(job["outcome"], "ok", "{job}");
        assert_eq!(job["waits"], serde_json::json!([]), "{job}");
    }
}

#[test]
fn a_run_that_waits_past_the_setting_fails_clearly() {
    let Some(f) = Fixture::new("bound") else {
        return;
    };
    f.settings(
        "repository_wait_minutes = 0",
        &[("one", "drive/repo"), ("two", "drive/repo")],
    );
    f.init("one");
    let _held = NamedMutexLocks::new()
        .try_lock(&f.key_of("one"))
        .unwrap()
        .expect("the repository is free");
    let (code, out, err) = f.replica(&["run", "two"], "");
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(
        err.contains("for another job to finish with") && err.contains("didn't start"),
        "{err}"
    );
    let list = f.replica(&["list", "--json"], "");
    let list: serde_json::Value = serde_json::from_str(&list.1).unwrap();
    let two = list["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["name"] == "two")
        .unwrap();
    assert_eq!(two["last_run"]["outcome"], "failed", "recorded: {two}");
}

/// The real Run all entry point with its first job's repository held by another process: the
/// job on a free repository runs to the end meanwhile, and the shared ones run once it's let go
/// (round 1's finding: a waiting job held up the whole of Run all).
#[test]
fn run_all_with_its_first_repository_busy_runs_the_others_meanwhile() {
    let Some(f) = Fixture::new("runall") else {
        return;
    };
    f.settings(
        "",
        &[
            ("s1", "drive/repo"),
            ("s2", "drive/repo"),
            ("elsewhere", "drive/other"),
        ],
    );
    f.init("s1");
    f.init("elsewhere");
    let held = NamedMutexLocks::new()
        .try_lock(&f.key_of("s1"))
        .unwrap()
        .expect("the repository is free");

    let mut all = f.start(&["run", "--all"]);
    // The last job in the list, on a free repository, finishes while the first two can't start.
    all.wait_for("elsewhere: OK");
    assert!(
        !all.text(None).contains("s1: OK") && !all.text(None).contains("s2: OK"),
        "{}",
        all.text(None)
    );
    // Then it comes back to the shared ones, which wait until they're let go.
    all.wait_for("Waiting for another job to finish with");
    drop(held);
    let (code, out, _) = all.finish();
    assert_eq!(code, 0, "{out}");
    for job in ["s1", "s2", "elsewhere"] {
        assert!(out.contains(&format!("{job}: OK")), "{job}: {out}");
    }
    assert!(!says_locked(&out), "{out}");
}

/// restic work outside a run takes its turn too: `verify`, `restore` and `init`, each
/// started while another process holds the repository, wait (saying so) and succeed once it's
/// let go.
#[test]
fn verify_restore_and_init_wait_their_turn() {
    let Some(f) = Fixture::new("outside") else {
        return;
    };
    f.settings("", &[("one", "drive/repo"), ("fresh", "drive/fresh")]);
    f.init("one");
    let (code, out, err) = f.replica(&["run", "one", "--json"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    let snapshot = doc["jobs"][0]["targets"][0]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let a = f.path("src/a.txt");
    let to = f.path("restored");
    let restore_args = [
        "restore",
        "one",
        snapshot.as_str(),
        a.to_str().unwrap(),
        "--to",
        to.to_str().unwrap(),
    ];

    for (args, stdin, key_of) in [
        (vec!["verify", "one"], "", "one"),
        (restore_args.to_vec(), "", "one"),
        (vec!["init", "fresh"], "saved\n", "fresh"),
    ] {
        let held = NamedMutexLocks::new()
            .try_lock(&f.key_of(key_of))
            .unwrap()
            .expect("the repository is free");
        let mut running = f.start_with(&args, stdin);
        running.wait_for("Waiting for another job to finish with");
        drop(held);
        let (code, out, _) = running.finish();
        assert_eq!(code, 0, "{args:?}: {out}");
        assert!(!says_locked(&out), "{args:?}: {out}");
    }
    assert_eq!(std::fs::read_to_string(to.join("a.txt")).unwrap(), "a");
    assert!(
        f.path("drive/fresh/config").is_file(),
        "init made the repository"
    );
}
