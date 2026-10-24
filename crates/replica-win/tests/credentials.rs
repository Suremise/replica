//! The real Windows Credential Manager, under a test-only prefix. Every credential a test
//! makes is removed when it ends, pass or fail; what a run that was killed left behind (no
//! `Drop` runs then) is swept by the next run.
//!
//! The tests take turns: writes and deletes made at the same moment race in Credential Manager
//! (a delete reports success and the entry is still there afterwards), so each store holds
//! [`TURN`] until its cleanup is done. `CredentialManager` now holds a per-user named mutex for
//! each call too; `parallel_writes_and_deletes_…` tests it with threads of its own, and
//! `parallel_processes_…` with copies of this test binary.
#![cfg(windows)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use replica_core::job::{Target, VolumeRef};
use replica_core::platform::{Credential, Secret, SecretStore};
use replica_win::credentials::CredentialManager;
use replica_win::keys;

/// Every test store's names start with this.
const TEST_PREFIX: &str = "ReplicaTest-";
/// A test store older than this is from a run that was killed: no run lasts this long.
const STALE: Duration = Duration::from_secs(60 * 60);

fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

/// One test store at a time (see the module's notes).
static TURN: Mutex<()> = Mutex::new(());

/// A Credential Manager under `ReplicaTest-<pid>-<time>-<test>/`, emptied when dropped. It
/// holds [`TURN`] (its second field) until then: fields drop after `Drop::drop` has run.
struct TestStore(
    CredentialManager,
    #[allow(dead_code)] MutexGuard<'static, ()>,
);

impl TestStore {
    fn new(test: &str) -> Self {
        // A test that panicked holding it poisons it; its store was still cleaned up.
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        static SWEEP: Once = Once::new();
        SWEEP.call_once(sweep_stale);
        let nanos = now().as_nanos();
        let prefix = format!("{TEST_PREFIX}{}-{nanos}-{test}/", std::process::id());
        Self(CredentialManager::with_prefix(&prefix), turn)
    }
}

/// Deletes the test credentials (`ReplicaTest-<pid>-<time>-…`) made over [`STALE`] ago. Only
/// names of that shape: anything else under `ReplicaTest-` is left alone.
fn sweep_stale() {
    let all = CredentialManager::with_prefix(TEST_PREFIX);
    for name in all.names().unwrap_or_default() {
        if made_before(&name, now().saturating_sub(STALE)) {
            let _ = all.delete(&name);
        }
    }
}

/// Whether `name` (after `ReplicaTest-`) is `<pid>-<nanos>-…` with its time before `cutoff`.
fn made_before(name: &str, cutoff: Duration) -> bool {
    let mut parts = name.splitn(3, '-');
    let (Some(pid), Some(nanos), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    pid.parse::<u32>().is_ok()
        && nanos
            .parse::<u64>()
            .is_ok_and(|n| Duration::from_nanos(n) < cutoff)
}

impl Drop for TestStore {
    fn drop(&mut self) {
        for name in self.0.names().unwrap_or_default() {
            let _ = self.0.delete(&name);
        }
        let left = self.0.names().unwrap_or_default();
        // Not an assert in Drop (it could hide the test's own failure); this one is visible.
        if !left.is_empty() {
            eprintln!("test credentials left behind: {left:?}");
        }
    }
}

fn login(user: Option<&str>, secret: &str) -> Credential {
    Credential {
        username: user.map(Into::into),
        secret: Secret::new(secret),
    }
}

#[test]
fn set_get_replace_and_delete() {
    let store = TestStore::new("roundtrip");
    let s = &store.0;
    assert_eq!(s.get("nas").unwrap(), None);

    s.set("nas", &login(Some(r"NAS\alex-test"), "first"))
        .unwrap();
    assert_eq!(
        s.get("nas").unwrap(),
        Some(login(Some(r"NAS\alex-test"), "first"))
    );

    s.set("nas", &login(Some("other"), "second")).unwrap();
    assert_eq!(s.get("nas").unwrap(), Some(login(Some("other"), "second")));

    s.set("repo", &login(None, "pässwörd 🔑 with spaces"))
        .unwrap();
    assert_eq!(
        s.get("repo").unwrap(),
        Some(login(None, "pässwörd 🔑 with spaces"))
    );

    let mut names = s.names().unwrap();
    names.sort();
    assert_eq!(names, ["nas", "repo"]);

    s.delete("nas").unwrap();
    s.delete("nas").unwrap();
    assert_eq!(s.get("nas").unwrap(), None);
    assert_eq!(s.names().unwrap(), ["repo"]);
}

#[test]
fn the_cleanup_removes_everything_under_the_prefix() {
    let prefix;
    {
        let store = TestStore::new("cleanup");
        store.0.set("a", &login(None, "1")).unwrap();
        store.0.set("b/c", &login(Some("u"), "2")).unwrap();
        prefix = store.0.target_name("");
    }
    assert!(
        CredentialManager::with_prefix(&prefix)
            .names()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_panicking_test_leaves_nothing() {
    let prefix = std::sync::Mutex::new(String::new());
    let result = std::panic::catch_unwind(|| {
        let store = TestStore::new("panic");
        *prefix.lock().unwrap() = store.0.target_name("");
        store.0.set("a", &login(None, "1")).unwrap();
        panic!("the test fails here");
    });
    assert!(result.is_err());
    let prefix = prefix.into_inner().unwrap();
    assert!(
        CredentialManager::with_prefix(&prefix)
            .names()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_store_from_a_killed_run_is_swept_and_others_are_not() {
    let hour = Duration::from_secs(60 * 60);
    let cutoff = Duration::from_secs(1_800_000_000);
    let old = (cutoff - hour).as_nanos();
    let new = (cutoff + hour).as_nanos();
    assert!(made_before(&format!("123-{old}-keys/repo"), cutoff));
    assert!(!made_before(&format!("123-{new}-keys/repo"), cutoff));
    // Shapes the sweep doesn't own: the CLI's `ReplicaTest-<pid>-<test>/` and anything else.
    assert!(!made_before("123-roundtrip/repo", cutoff));
    assert!(!made_before("x-1-keys", cutoff));
    assert!(!made_before("", cutoff));
}

#[test]
fn a_repository_password_is_stored_only_once_confirmed_and_shown_on_request() {
    let store = TestStore::new("keys");
    let target = Target::Drive {
        name: Some("external".into()),
        volume: VolumeRef {
            volume_id: None,
            label: Some("BACKUP".into()),
        },
        path: PathBuf::from("Replica"),
    };
    let new = keys::generate(&target, &store.0).unwrap();
    let shown = new.reveal().to_owned();
    assert!(store.0.names().unwrap().is_empty(), "nothing stored yet");

    let secret = new.confirm_saved(&store.0).unwrap();
    assert_eq!(secret.expose(), shown);
    assert_eq!(store.0.names().unwrap(), [keys::key_name(&target)]);
    assert_eq!(keys::show(&target, &store.0).unwrap().expose(), shown);
    assert!(keys::generate(&target, &store.0).is_err(), "never replaced");
}

/// What `cmdkey /list` shows under `prefix`: Windows' own view, not this process's.
fn cmdkey_names(prefix: &str) -> Vec<String> {
    let out = std::process::Command::new("cmdkey")
        .arg("/list")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split("target=").nth(1))
        .filter(|t| t.starts_with(prefix))
        .map(|t| t.trim().to_owned())
        .collect()
}

#[test]
fn parallel_writes_and_deletes_from_one_process_lose_nothing() {
    const THREADS: usize = 8;
    const EACH: usize = 12;
    let store = TestStore::new("parallel");
    let prefix = store.0.target_name("");
    let manager = store.0.clone();
    let run = |work: fn(&CredentialManager, &str)| {
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let manager = manager.clone();
                scope.spawn(move || {
                    for k in 0..EACH {
                        work(&manager, &format!("t{t}/k{k}"));
                    }
                });
            }
        });
    };
    // Every write lands.
    run(|m, name| m.set(name, &login(Some("u"), name)).unwrap());
    let fresh = CredentialManager::with_prefix(&prefix);
    assert_eq!(fresh.names().unwrap().len(), THREADS * EACH);
    assert_eq!(
        cmdkey_names(&prefix).len(),
        THREADS * EACH,
        "as Windows sees it"
    );
    // Every delete lands, with reads going on beside them.
    run(|m, name| {
        assert!(m.get(name).unwrap().is_some());
        m.delete(name).unwrap();
        assert!(m.get(name).unwrap().is_none());
    });
    assert!(fresh.names().unwrap().is_empty(), "{:?}", fresh.names());
    assert!(
        cmdkey_names(&prefix).is_empty(),
        "{:?}",
        cmdkey_names(&prefix)
    );
}

/// Set for the copies of this test binary that [`Children`] starts: `<what>|<prefix>|<n>`.
const CHILD: &str = "REPLICA_TEST_CREDENTIAL_CHILD";
/// How many credentials each of those copies writes and deletes.
const CHILD_EACH: usize = 8;

/// One copy's part of a test with [`Children`]; nothing in an ordinary run. `hold` writes like
/// `set`, then stays running (a copy that hangs) until it's stopped, or for ten minutes.
#[test]
fn child_process_part() {
    let Ok(job) = std::env::var(CHILD) else {
        return;
    };
    let [what, prefix, n] = job.splitn(3, '|').collect::<Vec<_>>()[..] else {
        panic!("{CHILD}={job}");
    };
    let manager = CredentialManager::with_prefix(prefix);
    for k in 0..CHILD_EACH {
        let name = format!("p{n}/k{k}");
        match what {
            "set" | "hold" => manager.set(&name, &login(Some("u"), &name)).unwrap(),
            "delete" => manager.delete(&name).unwrap(),
            _ => panic!("{CHILD}={job}"),
        }
    }
    if what == "hold" {
        std::thread::sleep(Duration::from_secs(10 * 60));
    }
}

/// Copies of this test binary, each doing its part of `child_process_part` under `prefix`.
/// Every copy started is stopped and reaped when this is dropped, also when a spawn fails or the
/// test panics, so none writes after the test's store is cleaned up: make it after the store.
struct Children(Vec<std::process::Child>);

impl Children {
    fn start(what: &str, prefix: &str, count: usize) -> Self {
        let mut children = Self(Vec::new());
        for n in 0..count {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_process_part", "--quiet"])
                .env(CHILD, format!("{what}|{prefix}|{n}"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            children.0.push(child);
        }
        children
    }

    fn ids(&self) -> Vec<u32> {
        self.0.iter().map(std::process::Child::id).collect()
    }

    /// Waits for every copy, and only then says whether each one succeeded.
    fn all_succeed(mut self) -> bool {
        let statuses: Vec<_> = self.0.iter_mut().map(|c| c.wait().unwrap()).collect();
        statuses.iter().all(std::process::ExitStatus::success)
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            // A copy that has ended already can't be stopped: wait returns its status.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Whether a process with this ID is running (`tasklist`, Windows' own view).
fn running(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

/// Polls `done` until it's true, failing after a minute (a hung run).
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(
            std::time::Instant::now() < until,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn parallel_processes_lose_nothing() {
    const PROCESSES: usize = 8;
    let store = TestStore::new("processes");
    let prefix = store.0.target_name("");
    let run = |what: &str| {
        let children = Children::start(what, &prefix, PROCESSES);
        assert!(children.all_succeed(), "a copy failed to {what}");
    };
    run("set");
    let fresh = CredentialManager::with_prefix(&prefix);
    assert_eq!(fresh.names().unwrap().len(), PROCESSES * CHILD_EACH);
    assert_eq!(cmdkey_names(&prefix).len(), PROCESSES * CHILD_EACH);
    run("delete");
    assert!(fresh.names().unwrap().is_empty(), "{:?}", fresh.names());
    assert!(
        cmdkey_names(&prefix).is_empty(),
        "{:?}",
        cmdkey_names(&prefix)
    );
}

#[test]
fn a_failing_test_stops_its_copies_and_leaves_nothing() {
    const PROCESSES: usize = 4;
    let started = Mutex::new((String::new(), Vec::new(), false));
    let result = std::panic::catch_unwind(|| {
        let store = TestStore::new("copies");
        let prefix = store.0.target_name("");
        let children = Children::start("hold", &prefix, PROCESSES);
        *started.lock().unwrap() = (prefix, children.ids(), false);
        // Every copy has written and is still running when the test fails.
        wait_for("the copies' writes", || {
            store.0.names().unwrap().len() == PROCESSES * CHILD_EACH
        });
        started.lock().unwrap().2 = true;
        panic!("the test fails here");
    });
    assert!(result.is_err());
    let (prefix, pids, held) = started.into_inner().unwrap();
    assert!(
        held,
        "the copies wrote and were running when the test failed"
    );
    for pid in pids {
        assert!(!running(pid), "copy {pid} is still running");
    }
    assert!(
        CredentialManager::with_prefix(&prefix)
            .names()
            .unwrap()
            .is_empty()
    );
    assert!(
        cmdkey_names(&prefix).is_empty(),
        "{:?}",
        cmdkey_names(&prefix)
    );
}
