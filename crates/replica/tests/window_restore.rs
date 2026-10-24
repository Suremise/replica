//! The window's restore and test restore against a real repository made by `replica.exe`, with
//! the pinned restic, in temp folders only: what the window restores is byte for byte what
//! `replica restore` does, and the test restore finds a damaged file.
//!
//! Credential Manager entries go under a test-only prefix, removed when the test ends, and
//! Task Scheduler is never touched.

#![cfg(windows)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::channel;

use replica::ui::backend::{Backend, LiveBackend, RestoreJob, RestoreTo};
use replica_core::cancel::CancelToken;
use replica_core::history::HistoryFilter;
use replica_core::platform::{Credential, Secret, SecretStore};
use replica_core::restic::password_secret_name;
use replica_core::restic::snapshot_path;
use replica_core::run::Outcome;
use replica_core::settings::Settings;
use replica_win::credentials::CredentialManager;
use replica_win::paths::AppPaths;
use replica_win::scheduler::Scheduler;

struct Fixture {
    temp: tempfile::TempDir,
    prefix: String,
}

impl Fixture {
    fn new(name: &str) -> Option<Fixture> {
        let restic =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/restic/bin/restic.exe");
        if !restic.is_file() {
            assert!(
                std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
                "restic is required (REPLICA_REQUIRE_RESTIC=1) but wasn't found"
            );
            return None;
        }
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("appdata/Replica")).unwrap();
        Some(Fixture {
            prefix: format!("ReplicaTest-{}-{name}/", std::process::id()),
            temp,
        })
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    /// A drive target on the temp folder's own volume, by its volume ID.
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

    fn replica(&self, args: &[&str], stdin: &str) -> (i32, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_replica"))
            .args(args)
            .env("APPDATA", self.path("appdata"))
            // restic keeps its cache under LOCALAPPDATA: in the temp folder, not the user's.
            .env("LOCALAPPDATA", self.path("localappdata"))
            .env(replica::cli::CREDENTIAL_PREFIX_VAR, &self.prefix)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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

    /// The window's backend on the same app folder and secrets as `replica.exe` here.
    fn backend(&self) -> LiveBackend {
        let paths =
            AppPaths::resolve("Replica", &self.path("exe"), Some(&self.path("appdata"))).unwrap();
        let (send, _requests) = channel();
        LiveBackend::with(
            paths,
            send,
            Arc::default(),
            CredentialManager::with_prefix(&self.prefix),
            // Never used here; a test-only folder all the same.
            Scheduler::in_folder(&format!(r"\{}", self.prefix.trim_end_matches('/'))),
        )
        .with_restic_cache(self.path("restic-cache"))
    }

    /// Job "hive" backing up `src` (a.txt, sub/b.bin, sub/deeper/c.txt), set up and run once.
    fn backed_up(name: &str) -> Option<Fixture> {
        let f = Fixture::new(name)?;
        f.write("src/a.txt", b"alpha");
        let binary: Vec<u8> = (0..70_000u32).map(|i| (i * 31 % 251) as u8).collect();
        f.write("src/sub/b.bin", &binary);
        f.write("src/sub/deeper/c.txt", "gamma ünïcødé".as_bytes());
        std::fs::write(
            f.path("appdata/Replica/settings.toml"),
            format!(
                "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = '{}'\nexclude = []\n[[jobs.targets]]\n{}\n",
                f.path("src").display(),
                f.drive_target("drive/repo")
            ),
        )
        .unwrap();
        let (code, out, err) = f.replica(&["init", "hive"], "saved\n");
        assert_eq!(code, 0, "{out}\n{err}");
        let (code, out, err) = f.replica(&["run", "hive"], "");
        assert_eq!(code, 0, "{out}\n{err}");
        Some(f)
    }

    fn job(&self) -> replica_core::job::Job {
        Settings::load(&self.path("appdata/Replica/settings.toml"))
            .unwrap()
            .settings
            .jobs
            .remove(0)
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

/// Every file under `dir`, by its path relative to it, with its bytes.
fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in std::fs::read_dir(&p).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.push((path.strip_prefix(dir).unwrap().to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_window_restores_a_file_and_a_folder_as_the_cli_does() {
    let Some(f) = Fixture::backed_up("restorewin") else {
        return;
    };
    let backend = f.backend();
    let job = f.job();
    let target = job.targets[0].clone();
    let snapshots = backend.snapshots(&job, &target).unwrap();
    assert_eq!(snapshots.len(), 1);
    let id = snapshots[0].id.clone();

    // Browsing lists a folder's contents, not the folder itself.
    let src = snapshot_path(&std::fs::canonicalize(f.path("src")).unwrap());
    let src = src.trim_start_matches("//?/").to_string();
    let listed = backend.list(&target, &id, &src).unwrap();
    let mut names: Vec<&str> = listed.iter().map(|n| n.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a.txt", "sub"]);

    // The CLI: the file and the folder, one call each.
    for (path, to) in [("src/a.txt", "cli/file"), ("src/sub", "cli/folder")] {
        let (code, out, err) = f.replica(
            &[
                "restore",
                "hive",
                &id,
                f.path(path).to_str().unwrap(),
                "--to",
                f.path(to).to_str().unwrap(),
            ],
            "",
        );
        assert_eq!(code, 0, "{out}\n{err}");
    }

    // The window: both together, into one new folder.
    let a = listed
        .iter()
        .find(|n| n.name == "a.txt")
        .unwrap()
        .path
        .clone();
    let sub = listed
        .iter()
        .find(|n| n.name == "sub")
        .unwrap()
        .path
        .clone();
    let window = f.path("window");
    backend
        .restore(
            &job,
            &target,
            &RestoreJob {
                snapshot: id.clone(),
                items: vec![a, sub],
                to: RestoreTo::Folder(window.clone()),
            },
            &CancelToken::new(),
        )
        .unwrap();

    let mut cli = tree(&f.path("cli/file"));
    cli.extend(tree(&f.path("cli/folder")));
    cli.sort();
    let restored = tree(&window);
    assert_eq!(restored.len(), 3, "{restored:?}");
    assert_eq!(restored, cli, "the same files and bytes as the CLI");
    assert_eq!(restored, tree(&f.path("src")), "and as the source");
}

#[test]
fn the_window_s_test_restore_passes_then_finds_a_damaged_file() {
    let Some(f) = Fixture::backed_up("testwin") else {
        return;
    };
    let backend = f.backend();
    let job = f.job();
    let target = job.targets[0].clone();

    let good = backend
        .test_restore(&job, &target, &CancelToken::new())
        .unwrap();
    assert_eq!(good.outcome, Outcome::Ok, "{good:?}");
    assert_eq!(good.checked, 3);

    // Damage every source the way a failing disk would: same size and modified time.
    for rel in ["src/a.txt", "src/sub/b.bin", "src/sub/deeper/c.txt"] {
        let p = f.path(rel);
        let time = std::fs::metadata(&p).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[0] ^= 0xFF;
        std::fs::write(&p, &bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }
    let bad = backend
        .test_restore(&job, &target, &CancelToken::new())
        .unwrap();
    assert_eq!(bad.outcome, Outcome::Failed);
    assert_eq!(bad.differing, 3);

    // Both are in the history, and neither is a run.
    let kept = backend
        .test_restores(&HistoryFilter::default(), 10)
        .unwrap();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].outcome, Outcome::Failed, "newest first");
    let runs = backend.runs(&HistoryFilter::default(), 10).unwrap();
    assert_eq!(runs.len(), 1, "just the backup");
    let store = CredentialManager::with_prefix(&f.prefix);
    assert!(
        !store.names().unwrap().is_empty(),
        "the job's password, under the test prefix"
    );
}

#[test]
fn a_test_restore_that_can_t_run_is_recorded_too() {
    let Some(f) = Fixture::backed_up("testfail") else {
        return;
    };
    let backend = f.backend();
    let job = f.job();
    let target = job.targets[0].clone();

    // The wrong password: restic refuses, and that's a failed test restore in History.
    let store = CredentialManager::with_prefix(&f.prefix);
    store
        .set(
            &password_secret_name(&target),
            &Credential {
                username: None,
                secret: Secret::new("not-the-password"),
            },
        )
        .unwrap();
    let error = backend
        .test_restore(&job, &target, &CancelToken::new())
        .unwrap_err();
    let kept = backend
        .test_restores(&HistoryFilter::default(), 10)
        .unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].outcome, Outcome::Failed);
    assert_eq!(kept[0].checked, 0);
    assert_eq!(kept[0].detail.as_deref(), Some(error.as_str()));

    // A target that isn't connected: skipped, as a run would be, and recorded.
    let away = replica_core::job::Target::Drive {
        name: None,
        volume: replica_core::job::VolumeRef {
            volume_id: None,
            label: Some("ReplicaTest-no-such-volume".into()),
        },
        path: "Replica".into(),
    };
    backend
        .test_restore(&job, &away, &CancelToken::new())
        .unwrap_err();
    let kept = backend
        .test_restores(&HistoryFilter::default(), 10)
        .unwrap();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].outcome, Outcome::Skipped);
    assert!(kept[0].detail.is_some());
    assert_eq!(
        backend.runs(&HistoryFilter::default(), 10).unwrap().len(),
        1,
        "neither is a run"
    );
}

/// The window's restore and test restore take their turn for the repository: started
/// while another process holds it, each waits, says what for (shown on the Restore page in
/// place of "Restoring…"), and succeeds once it's let go.
#[test]
fn the_window_s_restore_and_test_restore_wait_their_turn() {
    use replica_core::repo_lock::{RepositoryLocks, lock_key};
    use replica_win::repo_locks::NamedMutexLocks;

    let Some(f) = Fixture::backed_up("turnwin") else {
        return;
    };
    let backend = Arc::new(f.backend());
    let job = f.job();
    let target = job.targets[0].clone();
    let id = backend.snapshots(&job, &target).unwrap()[0].id.clone();
    let src = snapshot_path(&std::fs::canonicalize(f.path("src")).unwrap());
    let a = format!("{}/a.txt", src.trim_start_matches("//?/"));
    let to = f.path("turn");

    // Waits until the backend says it's waiting, then lets go, and returns what it said.
    let wait_then_let_go = |work: Box<dyn FnOnce() -> Result<(), String> + Send>| {
        let held = NamedMutexLocks::new()
            .try_lock(&lock_key(&target))
            .unwrap()
            .expect("the repository is free");
        let worker = std::thread::spawn(work);
        let start = std::time::Instant::now();
        let said = loop {
            if let Some(text) = backend.waiting() {
                break text;
            }
            assert!(!worker.is_finished(), "it didn't wait");
            assert!(
                start.elapsed() < std::time::Duration::from_secs(120),
                "never waited"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        drop(held);
        worker.join().unwrap().unwrap();
        assert_eq!(backend.waiting(), None, "cleared once it went on");
        said
    };

    let restore = {
        let (backend, job, target) = (Arc::clone(&backend), job.clone(), target.clone());
        let request = RestoreJob {
            snapshot: id,
            items: vec![a],
            to: RestoreTo::Folder(to.clone()),
        };
        Box::new(move || {
            backend
                .restore(&job, &target, &request, &CancelToken::new())
                .map(|_| ())
        })
    };
    let said = wait_then_let_go(restore);
    assert!(
        said.contains("Waiting for") && said.contains(&target.display_name()),
        "{said}"
    );
    assert_eq!(std::fs::read(to.join("a.txt")).unwrap(), b"alpha");

    let test = {
        let (backend, job, target) = (Arc::clone(&backend), job.clone(), target.clone());
        Box::new(move || {
            let record = backend.test_restore(&job, &target, &CancelToken::new())?;
            assert_eq!(record.outcome, Outcome::Ok, "{record:?}");
            Ok(())
        })
    };
    wait_then_let_go(test);
}
