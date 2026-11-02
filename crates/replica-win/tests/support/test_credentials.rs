//! Test credentials in the real Windows Credential Manager: the one place tests may write
//! outside their temp folders, and only where a test drives the real API or a real
//! `replica.exe` (which can't be given an in-memory store). Shared by the test files that need
//! it (`#[path]`), in `replica-win` and `replica`.
//!
//! - **Names:** each test's credentials go under `ReplicaTest-<pid>-<nanos>-<name>/`
//!   ([`TestCredentials::new`]), never under Replica's own `Replica/`.
//! - **Cleanup fails the test that left something:** dropping a [`TestCredentials`], as a test
//!   ends, deletes every credential under its prefix, retrying while Credential Manager is busy.
//!   If any is left, or the prefix can't even be listed, the drop panics, so that test fails,
//!   whenever in the run it ends. A test that's already failing (panicking) keeps its own
//!   failure: the leftover is said on stderr instead, as a second panic would abort.
//! - **Sweep:** what a run that was killed left behind (no `Drop` runs then) is deleted when the
//!   next test of that name starts ([`TestCredentials::new`]), and found by
//!   `no_test_credentials_are_left_behind` (`tests/credentials.rs`), which fails on any.
//!   A credential is stale ([`is_stale`]) when the process that made it has ended, or it's
//!   older than [`STALE`]: never one a test still running (here or in another checkout) holds.
//!   Names of any other shape under `ReplicaTest-` are stale too: no test makes them any more.
//! - **Never "clean" by default:** a list that can't be read is an error everywhere here, never
//!   an empty list.
#![allow(dead_code)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use replica_core::platform::SecretStore;
use replica_win::credentials::CredentialManager;
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, STILL_ACTIVE};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Every test credential's name starts with this.
pub const TEST_PREFIX: &str = "ReplicaTest-";
/// A test credential older than this is from a run that was killed: no test lasts this long.
pub const STALE: Duration = Duration::from_secs(60 * 60);
/// How many times a list or a cleanup is tried before it gives up (Credential Manager's calls
/// can time out waiting their turn while many tests use it).
pub const ATTEMPTS: usize = 5;

fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

/// What cleanup and the checks need of a store: listing and deleting. Credential Manager, or a
/// stand-in that fails, for this helper's own tests.
pub trait Names: Send + Sync {
    fn list(&self) -> Result<Vec<String>, String>;
    fn remove(&self, name: &str) -> Result<(), String>;
}

impl Names for CredentialManager {
    fn list(&self) -> Result<Vec<String>, String> {
        CredentialManager::names(self).map_err(|e| e.to_string())
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        SecretStore::delete(self, name).map_err(|e| e.to_string())
    }
}

/// A test's own prefix in Credential Manager, emptied when dropped (see the module's notes).
pub struct TestCredentials {
    prefix: String,
    /// What the drop empties: Credential Manager under the prefix.
    cleanup: Box<dyn Names>,
}

impl TestCredentials {
    /// `ReplicaTest-<pid>-<nanos>-<name>/`, after sweeping stale credentials of tests called
    /// `name`. `name` is letters, digits and `-`. Panics if the sweep can't be done.
    pub fn new(name: &str) -> Self {
        Self::with_cleanup(name, |prefix| {
            Box::new(CredentialManager::with_prefix(prefix))
        })
    }

    /// [`Self::new`], emptied through the store `cleanup` makes for the prefix: for this
    /// helper's own tests, with a store whose deletes fail.
    pub fn with_cleanup(name: &str, cleanup: impl FnOnce(&str) -> Box<dyn Names>) -> Self {
        let mine = |rest: &str| {
            rest.split_once('/')
                .is_some_and(|(owner, _)| owner_name(owner) == Some(name))
        };
        if let Err(e) = sweep(mine) {
            panic!("couldn't sweep the stale test credentials of {name}: {e}");
        }
        let prefix = format!(
            "{TEST_PREFIX}{}-{}-{name}/",
            std::process::id(),
            now().as_nanos()
        );
        let cleanup = cleanup(&prefix);
        Self { prefix, cleanup }
    }

    /// The prefix, with its `/`: for `REPLICA_CREDENTIAL_PREFIX` or `with_prefix`.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The prefix without its `/`: a name of its own (a task folder's, say).
    pub fn tag(&self) -> &str {
        self.prefix.trim_end_matches('/')
    }

    /// A store under the prefix.
    pub fn store(&self) -> CredentialManager {
        CredentialManager::with_prefix(&self.prefix)
    }

    /// A store under a part of the prefix of its own (`<prefix><part>/`): another PC's, say.
    /// Emptied with the rest.
    pub fn store_in(&self, part: &str) -> CredentialManager {
        CredentialManager::with_prefix(&format!("{}{part}/", self.prefix))
    }
}

impl Drop for TestCredentials {
    fn drop(&mut self) {
        fail_if_left(&self.prefix, empty(self.cleanup.as_ref()));
    }
}

/// A cleanup's result, `prefix`'s: what's left fails the test, unless it's failing already
/// (a second panic would abort, and hide why), when it goes to stderr.
pub fn fail_if_left(prefix: &str, cleaned: Result<(), String>) {
    if let Err(left) = cleaned {
        let message = format!("test credentials left behind under {prefix}: {left}");
        if std::thread::panicking() {
            eprintln!("{message}");
        } else {
            panic!("{message}");
        }
    }
}

/// Deletes everything `store` lists, in up to [`ATTEMPTS`] rounds of listing and deleting, then
/// lists it once more to see. `Ok` only on a list that came back empty; `Err` says what's left,
/// or that the store couldn't be listed.
pub fn empty(store: &dyn Names) -> Result<(), String> {
    for _ in 0..ATTEMPTS {
        match store.list() {
            Ok(names) if names.is_empty() => return Ok(()),
            Ok(names) => {
                for name in &names {
                    let _ = store.remove(name);
                }
            }
            // Busy: Credential Manager's calls wait their turn, and may time out under load.
            Err(_) => {}
        }
    }
    // The last round's deletes are seen to, as the earlier ones were by the next round's list.
    match list(store) {
        Ok(names) if names.is_empty() => Ok(()),
        Ok(names) => Err(format!("{names:?}")),
        Err(e) => Err(format!("({e})")),
    }
}

/// Empties `prefix` (in the real Credential Manager) when dropped, failing the test if it
/// can't: a second, working cleanup for a test whose own `TestCredentials` is made not to work.
pub struct EmptyOnDrop<'a>(pub &'a std::sync::Mutex<String>);

impl Drop for EmptyOnDrop<'_> {
    fn drop(&mut self) {
        let prefix = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !prefix.is_empty() {
            fail_if_left(&prefix, empty(&CredentialManager::with_prefix(&prefix)));
        }
    }
}

/// What `store` lists, trying [`ATTEMPTS`] times; `Err` if it never could.
fn list(store: &dyn Names) -> Result<Vec<String>, String> {
    let mut why = String::new();
    for _ in 0..ATTEMPTS {
        match store.list() {
            Ok(names) => return Ok(names),
            Err(e) => why = e,
        }
    }
    Err(format!("couldn't list the test credentials: {why}"))
}

/// The stale names (after `ReplicaTest-`) in `store`, a store under `ReplicaTest-`, as of
/// `cutoff`; `Err` if it can't be listed.
pub fn stale_in(store: &dyn Names, cutoff: Duration) -> Result<Vec<String>, String> {
    Ok(list(store)?
        .into_iter()
        .filter(|rest| is_stale(rest, cutoff))
        .collect())
}

/// The stale test credentials in Credential Manager (their names after `ReplicaTest-`).
pub fn stale() -> Result<Vec<String>, String> {
    stale_in(
        &CredentialManager::with_prefix(TEST_PREFIX),
        now().saturating_sub(STALE),
    )
}

/// Deletes the stale names in `store` (a store under `ReplicaTest-`) that `pick` chooses. `Err`
/// if it can't list them, or names those it couldn't delete.
pub fn sweep_in(
    store: &dyn Names,
    cutoff: Duration,
    pick: impl Fn(&str) -> bool,
) -> Result<(), String> {
    let mut left = Vec::new();
    for rest in stale_in(store, cutoff)?.into_iter().filter(|r| pick(r)) {
        if let Err(e) = store.remove(&rest) {
            left.push(format!("{rest} ({e})"));
        }
    }
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!("couldn't delete {left:?}"))
    }
}

/// [`sweep_in`] on Credential Manager.
pub fn sweep(pick: impl Fn(&str) -> bool) -> Result<(), String> {
    sweep_in(
        &CredentialManager::with_prefix(TEST_PREFIX),
        now().saturating_sub(STALE),
        pick,
    )
}

/// Whether a test credential (its name after `ReplicaTest-`) is stale: its owner
/// `<pid>-<nanos>-<name>` has ended or is older than `cutoff`, or it isn't of that shape.
pub fn is_stale(rest: &str, cutoff: Duration) -> bool {
    let owner = rest.split_once('/').map_or(rest, |(o, _)| o);
    match owner_parts(owner) {
        Some((pid, made, _)) => made < cutoff || !process_running(pid),
        None => true,
    }
}

/// `<pid>-<nanos>-<name>`'s parts.
fn owner_parts(owner: &str) -> Option<(u32, Duration, &str)> {
    let mut parts = owner.splitn(3, '-');
    let pid = parts.next()?.parse::<u32>().ok()?;
    let nanos = parts.next()?.parse::<u64>().ok()?;
    let name = parts.next().filter(|n| !n.is_empty())?;
    Some((pid, Duration::from_nanos(nanos), name))
}

fn owner_name(owner: &str) -> Option<&str> {
    owner_parts(owner).map(|(_, _, name)| name)
}

/// Whether process `pid` is still running. One that can't be asked about (another user's, say)
/// counts as running: it's never swept from under someone.
#[allow(unsafe_code)] // Two Win32 calls on a handle of its own.
pub fn process_running(pid: u32) -> bool {
    // SAFETY: a query-only handle, closed below; the exit code is a live out pointer.
    unsafe {
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => handle,
            // No such process, unless it's one we may not ask about.
            Err(e) => return e.code() == E_ACCESSDENIED,
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code).is_ok();
        let _ = CloseHandle(handle);
        !ok || code == STILL_ACTIVE.0 as u32
    }
}
