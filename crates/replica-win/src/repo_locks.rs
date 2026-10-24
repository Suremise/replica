//! The per-repository locks for Windows: `replica-core`'s [`RepositoryLocks`] as named mutexes.
//!
//! One mutex per repository, `Global\Replica.Repository.<user SID>.<key digest>`: the
//! `Global\` namespace spans the user's logon sessions (console, RDP, a scheduled task), and
//! the SID, the same in all of them, keeps users apart, as their Credential Manager passwords
//! are. It's made with the user's own default security, so no admin rights are needed. A
//! process that dies holding one leaves it abandoned, and the next to try takes it over. Unlike
//! a lock file, it doesn't depend on every Replica using the same app folder.
//!
//! A Windows mutex belongs to the thread that took it, and that thread may take it again, so
//! two runs on one thread wouldn't keep each other out. A run takes and lets go of its locks
//! on its own thread, and the tray runs one job at a time, so that never happens.

use std::sync::OnceLock;
use std::time::Duration;

use replica_core::platform::PlatformError;
use replica_core::repo_lock::{HeldLock, RepositoryLocks, key_digest};

use crate::win32;

/// The mutex for the repository `key`, for the user `user_sid`.
pub fn mutex_name(user_sid: &str, key: &str) -> String {
    format!(r"Global\Replica.Repository.{user_sid}.{}", key_digest(key))
}

/// Repository locks shared by every Replica process of this user, in any session.
#[derive(Debug, Default)]
pub struct NamedMutexLocks {
    sid: OnceLock<String>,
}

impl NamedMutexLocks {
    pub fn new() -> Self {
        Self::default()
    }

    fn sid(&self) -> Result<&str, PlatformError> {
        if let Some(sid) = self.sid.get() {
            return Ok(sid);
        }
        let sid = win32::current_user_sid().map_err(|e| PlatformError(e.to_string()))?;
        Ok(self.sid.get_or_init(|| sid))
    }
}

struct Held(#[allow(dead_code)] win32::NamedMutexGuard);

impl HeldLock for Held {}

impl RepositoryLocks for NamedMutexLocks {
    fn try_lock(&self, key: &str) -> Result<Option<Box<dyn HeldLock>>, PlatformError> {
        match win32::lock_named_mutex(&mutex_name(self.sid()?, key), Duration::ZERO) {
            Ok(guard) => Ok(Some(Box::new(Held(guard)))),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(PlatformError(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    /// A key no other test or Replica uses.
    fn key(test: &str) -> String {
        format!("replica:repo:test:{}:{test}", std::process::id())
    }

    #[test]
    fn the_name_is_per_user_and_per_repository() {
        let a = mutex_name("S-1-5-21-1", "replica:repo:unc:\\\\nas\\a");
        assert!(
            a.starts_with(r"Global\Replica.Repository.S-1-5-21-1."),
            "{a}"
        );
        assert!(
            !a[r"Global\".len()..].contains('\\'),
            "no more backslashes: {a}"
        );
        assert_ne!(a, mutex_name("S-1-5-21-2", "replica:repo:unc:\\\\nas\\a"));
        assert_ne!(a, mutex_name("S-1-5-21-1", "replica:repo:unc:\\\\nas\\b"));
    }

    #[test]
    fn held_on_another_thread_keeps_this_one_out_until_let_go() {
        let locks = NamedMutexLocks::new();
        let key = key("thread");
        let (took, took_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let key = key.clone();
            std::thread::spawn(move || {
                let held = NamedMutexLocks::new().try_lock(&key).unwrap();
                took.send(held.is_some()).unwrap();
                let _ = release_rx.recv();
                drop(held);
            })
        };
        assert!(took_rx.recv().unwrap(), "the other thread took it");
        assert!(locks.try_lock(&key).unwrap().is_none(), "held there");
        assert!(
            locks.try_lock(&format!("{key}-other")).unwrap().is_some(),
            "another repository is free"
        );
        release.send(()).unwrap();
        holder.join().unwrap();
        assert!(locks.try_lock(&key).unwrap().is_some(), "free once let go");
    }

    const CHILD: &str = "REPLICA_REPO_LOCK_TEST_CHILD";

    /// Run only as a child of the test below: takes the lock, says so, and waits to be killed.
    #[test]
    fn child_holds_a_repository_lock() {
        let Some(dir) = std::env::var_os(CHILD) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let key = std::fs::read_to_string(dir.join("key")).unwrap();
        let _held = NamedMutexLocks::new()
            .try_lock(&key)
            .unwrap()
            .expect("free");
        std::fs::write(dir.join("held"), "").unwrap();
        // Killed from outside; bounded so a lost parent doesn't leave it running.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The child test process, killed and reaped however the test ends.
    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A process killed while it holds a repository's lock lets it go: the next run takes it.
    #[test]
    fn the_lock_is_freed_when_its_holder_is_killed() {
        if std::env::var_os(CHILD).is_some() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let key = key("killed");
        std::fs::write(temp.path().join("key"), &key).unwrap();
        let mut child = KillOnDrop(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "repo_locks::tests::child_holds_a_repository_lock",
                    "--test-threads=1",
                ])
                .env(CHILD, temp.path())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        while !temp.path().join("held").exists() {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the child never took the lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let locks = NamedMutexLocks::new();
        let while_held = locks.try_lock(&key).unwrap().is_some();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        // Windows abandons it as the process ends: no wait beyond that is needed.
        let after = locks.try_lock(&key).unwrap().is_some();
        assert!(!while_held, "held by the child");
        assert!(after, "taken over once the child was killed");
    }
}
