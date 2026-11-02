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
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use replica_core::job::{Target, VolumeRef};
use replica_core::platform::{Credential, Secret, SecretStore};
use replica_win::credentials::CredentialManager;
use replica_win::keys;

#[path = "support/test_credentials.rs"]
mod test_credentials;

use test_credentials::{EmptyOnDrop, Names, STALE, TestCredentials, is_stale};

/// One test store at a time (see the module's notes).
static TURN: Mutex<()> = Mutex::new(());

/// A Credential Manager under the test's own prefix ([`TestCredentials`]), emptied when
/// dropped. It holds [`TURN`] until then: fields drop in order, the guard last.
struct TestStore(
    CredentialManager,
    #[allow(dead_code)] TestCredentials,
    #[allow(dead_code)] MutexGuard<'static, ()>,
);

impl TestStore {
    fn new(test: &str) -> Self {
        // A test that panicked holding it poisons it; its store was still cleaned up.
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let credentials = TestCredentials::new(test);
        Self(credentials.store(), credentials, turn)
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
    let me = std::process::id();
    // A running test's: never stale, unless it's older than any test lasts.
    assert!(!is_stale(&format!("{me}-{new}-keys/repo"), cutoff));
    assert!(!is_stale(
        &format!("{me}-{new}-repolock-two-runs/nas"),
        cutoff
    ));
    assert!(is_stale(&format!("{me}-{old}-keys/repo"), cutoff));
    // An ended process's: stale however new. (No process has ID 3: IDs are multiples of 4.)
    assert!(is_stale(&format!("3-{new}-keys/repo"), cutoff));
    // Shapes no test makes any more, such as `ReplicaTest-<pid>-<test>/`: stale.
    assert!(is_stale("39376-login/nas-login", cutoff));
    assert!(is_stale("x-1-keys", cutoff));
    assert!(is_stale(&format!("{me}-{new}-/x"), cutoff), "no name");
    assert_eq!(STALE, hour);
}

/// A test that leaves a credential fails itself, whenever in the run it ends (its
/// `TestCredentials` panics on drop), so this one is for what a killed run left: any stale test
/// credential (one whose test has ended). It's removed here too, so the next run starts clean,
/// and named, so the test that left it can be found. A Credential Manager that can't be listed
/// fails it: that's no proof of a clean one. Only `ReplicaTest-…` names: Replica's own
/// `Replica/…` credentials are never read or touched.
#[test]
fn no_test_credentials_are_left_behind() {
    let left = test_credentials::stale().unwrap();
    let swept = test_credentials::sweep(|_| true);
    assert!(
        left.is_empty(),
        "test credentials left behind (now removed: {swept:?}): {left:?}"
    );
}

/// A stand-in store whose every call fails, as a busy or broken Credential Manager's would.
struct Broken;

impl Names for Broken {
    fn list(&self) -> Result<Vec<String>, String> {
        Err("busy".into())
    }

    fn remove(&self, _: &str) -> Result<(), String> {
        Err("busy".into())
    }
}

/// The real store under a prefix, whose deletes do nothing: a cleanup that can't remove.
struct NoDelete(CredentialManager);

impl Names for NoDelete {
    fn list(&self) -> Result<Vec<String>, String> {
        self.0.list()
    }

    fn remove(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn a_store_that_cant_be_listed_is_never_taken_for_a_clean_one() {
    let cutoff = Duration::from_secs(1_800_000_000);
    assert!(test_credentials::stale_in(&Broken, cutoff).is_err());
    assert!(test_credentials::sweep_in(&Broken, cutoff, |_| true).is_err());
    assert!(test_credentials::empty(&Broken).is_err());

    // A cleanup that can't list fails the test, as one that can't delete does.
    let result = std::panic::catch_unwind(|| {
        drop(TestCredentials::with_cleanup("broken", |_| {
            Box::new(Broken)
        }));
    });
    let message = panic_text(result.unwrap_err());
    assert!(message.contains("left behind"), "{message}");
    assert!(
        message.contains("couldn't list the test credentials: busy"),
        "{message}"
    );
}

#[test]
fn a_test_that_leaves_a_credential_fails_and_says_which() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let prefix = Mutex::new(String::new());
    // A second, working cleanup, in place before the probe is written: the probe is removed
    // however this test ends, failing assertions and unwraps below included.
    let _cleanup = EmptyOnDrop(&prefix);
    // The test itself passes; its own cleanup can't remove what it wrote.
    let result = std::panic::catch_unwind(|| {
        let leaky = TestCredentials::with_cleanup("leaky", |prefix| {
            Box::new(NoDelete(CredentialManager::with_prefix(prefix)))
        });
        *prefix.lock().unwrap() = leaky.prefix().to_owned();
        leaky.store().set("probe", &login(None, "x")).unwrap();
    });
    let prefix = prefix.lock().unwrap().clone();
    // The real probe is still there, as the failure says.
    let left = CredentialManager::with_prefix(&prefix).names().ok();
    let message = result.err().map(panic_text).unwrap_or_default();
    assert!(
        message.contains("left behind"),
        "the test didn't fail: {message:?}"
    );
    assert!(
        message.contains(&prefix) && message.contains("probe"),
        "{message}"
    );
    assert_eq!(left, Some(vec!["probe".to_string()]));
}

/// Busy for its first `busy` lists, then the names it holds; a removal sticks or doesn't.
struct Flaky {
    busy: Mutex<usize>,
    names: Mutex<Vec<String>>,
    sticks: bool,
}

impl Flaky {
    fn new(busy: usize, sticks: bool) -> Self {
        Flaky {
            busy: Mutex::new(busy),
            names: Mutex::new(vec!["probe".into()]),
            sticks,
        }
    }
}

impl Names for Flaky {
    fn list(&self) -> Result<Vec<String>, String> {
        let mut busy = self.busy.lock().unwrap();
        if *busy > 0 {
            *busy -= 1;
            return Err("busy".into());
        }
        Ok(self.names.lock().unwrap().clone())
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        if self.sticks {
            self.names.lock().unwrap().retain(|n| n != name);
        }
        Ok(())
    }
}

#[test]
fn a_cleanup_is_clean_only_when_it_s_seen_to_be() {
    // Busy until its last round, whose delete works: the final look sees it empty.
    let late = Flaky::new(test_credentials::ATTEMPTS - 1, true);
    assert_eq!(test_credentials::empty(&late), Ok(()));
    assert!(late.names.lock().unwrap().is_empty());
    // Busy every round, so nothing was deleted: the final look says what's there.
    let never = Flaky::new(test_credentials::ATTEMPTS, true);
    assert_eq!(
        test_credentials::empty(&never),
        Err(r#"["probe"]"#.to_string())
    );
    // Deletes that never stick.
    let stuck = Flaky::new(0, false);
    assert_eq!(
        test_credentials::empty(&stuck),
        Err(r#"["probe"]"#.to_string())
    );
}

#[test]
fn a_failing_test_keeps_its_own_failure_when_its_cleanup_fails_too() {
    let result = std::panic::catch_unwind(|| {
        let _store = TestCredentials::with_cleanup("both", |_| Box::new(Broken));
        panic!("the test's own failure");
    });
    // Not an abort, and not the cleanup's panic: the test's (the leftover goes to stderr).
    assert_eq!(panic_text(result.unwrap_err()), "the test's own failure");
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
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
