//! Repository passwords for versioned targets.
//!
//! A versioned target is an encrypted restic repository: **losing its password loses the
//! backup**, and Credential Manager dies with the PC. So the flow is:
//!
//! 1. [`generate`] makes a strong random password for a target. Nothing is stored yet.
//! 2. The CLI or the window shows it once ([`NewPassword::reveal`], with the
//!    `password-save-*` strings) and asks the user to confirm they've saved a copy.
//! 3. Only that confirmation, [`NewPassword::confirm_saved`], stores it and hands it back for
//!    `restic init`. A user who stops there leaves nothing behind, and no repository.
//! 4. [`show`] is the deliberate way to see it again (`replica key show <job>`). A run opens
//!    the repository with core's `restic::Repository::from_store`, which reads the same name
//!    ([`key_name`]); [`for_restic`] is the same read for callers that want the secret alone.
//! 5. [`save_entered`] puts a saved copy back (`replica key set <job>`, the window's "Enter a
//!    saved password…"), on a new PC or after the entry was removed: only once restic says it
//!    opens the repository.
//!
//! A password that opens the repository is never replaced, except by one that opens it too and
//! only when asked to (`--force`): replacing it with a wrong one would lock Replica out of the
//! backup there.

use std::fmt;

use replica_core::job::Target;
use replica_core::platform::{Credential, PlatformError, Secret, SecretStore};
use replica_core::restic::{ResticError, password_secret_name};
use replica_core::tr;

use crate::win32;

/// Letters and digits that can't be mistaken for one another when copied by hand (no `0`, `o`,
/// `1`, `l` or `i`): 31 symbols, so about 4.95 bits each.
const ALPHABET: &[u8; 31] = b"abcdefghjkmnpqrstuvwxyz23456789";
/// Six groups of five: 30 symbols, about 148 bits.
const GROUPS: usize = 6;
const GROUP_LEN: usize = 5;

/// How long a confirmation waits for another to finish storing.
const KEYS_MUTEX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The named mutex that makes storing a new password check-and-write in one step:
/// `Global\Replica.RepositoryPasswords.<user SID>`.
///
/// Its scope is the stored credential's: Credential Manager keeps a password for this user on
/// this PC, visible in every one of the user's logon sessions (console, RDP, a scheduled
/// task), so the mutex is in the `Global\` namespace, which spans sessions, and named by the
/// user's SID, which is the same in all of them and differs between users.
fn keys_mutex_name(user_sid: &str) -> String {
    format!(r"Global\Replica.RepositoryPasswords.{user_sid}")
}

fn lock_keys() -> Result<win32::NamedMutexGuard, PlatformError> {
    let busy = |e: std::io::Error| PlatformError(tr!("error-key-busy", detail = e.to_string()));
    let sid = win32::current_user_sid().map_err(busy)?;
    win32::lock_named_mutex(&keys_mutex_name(&sid), KEYS_MUTEX_WAIT).map_err(busy)
}

/// The secret-store name of a target's repository password: core's
/// [`password_secret_name`], which [`replica_core::restic::Repository::from_store`] reads, so
/// a password stored here is the one a run finds.
///
/// It follows the repository, not the job or the target's display name: a drive by its volume
/// ID (or label) and path, a share by its UNC path, in lower case. Renaming a job or a target
/// keeps its password, and two jobs that share a repository share its password, as restic
/// requires.
pub fn key_name(target: &Target) -> String {
    password_secret_name(target)
}

/// A password made by [`generate`] and not stored yet.
pub struct NewPassword {
    name: String,
    target: String,
    secret: Secret,
}

impl std::fmt::Debug for NewPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewPassword")
            .field("name", &self.name)
            .field("secret", &self.secret)
            .finish()
    }
}

impl NewPassword {
    /// Its name in the secret store.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The password, to show the user once.
    pub fn reveal(&self) -> &str {
        self.secret.expose()
    }

    /// The user has confirmed they've saved a copy: stores the password and gives it back,
    /// for `restic init`. Refuses if a password was stored for this target meanwhile.
    ///
    /// Credential Manager has no "create if absent", so the check and the write run under a
    /// named mutex shared by every Replica process of this user, in any session
    /// ([`keys_mutex_name`]): of two confirmations at once, only one stores its password, and
    /// the other is refused before `restic init`.
    pub fn confirm_saved(self, store: &dyn SecretStore) -> Result<Secret, PlatformError> {
        let _lock = lock_keys()?;
        if store.get(&self.name)?.is_some() {
            return Err(PlatformError(tr!("error-key-exists", target = self.target)));
        }
        store.set(
            &self.name,
            &Credential {
                username: None,
                secret: self.secret.clone(),
            },
        )?;
        tracing::info!(key = %self.name, "stored a new repository password, confirmed saved by the user");
        Ok(self.secret)
    }
}

/// Makes a new repository password for `target`, without storing it. Refuses if one is stored
/// already (another job may share the repository: it's initialised, and has its password).
pub fn generate(target: &Target, store: &dyn SecretStore) -> Result<NewPassword, PlatformError> {
    let name = key_name(target);
    if store.get(&name)?.is_some() {
        return Err(PlatformError(tr!(
            "error-key-exists",
            target = target.display_name()
        )));
    }
    let password = new_password(win32::random_bytes)
        .map_err(|e| PlatformError(tr!("error-random-failed", detail = e.to_string())))?;
    Ok(NewPassword {
        name,
        target: target.display_name(),
        secret: Secret::new(password),
    })
}

/// The stored password, to show the user again (`replica key show <job>`). An error if there
/// isn't one.
pub fn show(target: &Target, store: &dyn SecretStore) -> Result<Secret, PlatformError> {
    let name = key_name(target);
    let found = store
        .get(&name)?
        .ok_or_else(|| PlatformError(tr!("error-key-missing", target = target.display_name())))?;
    tracing::info!(key = %name, "showed a repository password to the user");
    Ok(found.secret)
}

/// The stored password, for restic, or `None` if there isn't one. A run normally opens the
/// repository with [`replica_core::restic::Repository::from_store`], which reads the same name.
pub fn for_restic(
    target: &Target,
    store: &dyn SecretStore,
) -> Result<Option<Secret>, PlatformError> {
    Ok(store.get(&key_name(target))?.map(|c| c.secret))
}

/// What [`save_entered`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entered {
    /// Saved: none was saved for this target.
    Saved,
    /// Saved in place of one that doesn't open the repository (or, forced, of one that does).
    Replaced,
    /// That very password was saved already.
    Unchanged,
}

/// Why [`save_entered`] saved nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSaved {
    /// The password typed doesn't open the target's repository.
    Wrong { target: String },
    /// The saved password opens the repository already, and replacing it wasn't asked for.
    SavedWorks { target: String },
    /// It couldn't be checked or saved: why.
    Failed(String),
}

impl fmt::Display for NotSaved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotSaved::Wrong { target } => {
                f.write_str(&tr!("error-key-entered-wrong", target = target.as_str()))
            }
            NotSaved::SavedWorks { target } => {
                f.write_str(&tr!("error-key-saved-works", target = target.as_str()))
            }
            NotSaved::Failed(why) => f.write_str(why),
        }
    }
}

/// Saves a repository password the user typed (the copy they kept), for `target`, once
/// `opens` says it opens the repository (`Restic::opens` on the target's folder).
///
/// A password saved already is replaced only if `opens` says it doesn't open the repository, or
/// with `force`. Typing the saved password again changes nothing. The check of the saved one
/// and the write run under the same lock as [`NewPassword::confirm_saved`], so a password
/// stored by another Replica meanwhile is checked before it's replaced.
pub fn save_entered(
    target: &Target,
    entered: Secret,
    store: &dyn SecretStore,
    force: bool,
    opens: &dyn Fn(&Secret) -> Result<(), ResticError>,
) -> Result<Entered, NotSaved> {
    let shown = target.display_name();
    if entered.expose().is_empty() {
        return Err(NotSaved::Wrong { target: shown });
    }
    match opens(&entered) {
        Ok(()) => {}
        Err(ResticError::WrongPassword { .. }) => return Err(NotSaved::Wrong { target: shown }),
        Err(e) => return Err(NotSaved::Failed(e.to_string())),
    }
    let failed = |e: PlatformError| NotSaved::Failed(e.0);
    let _lock = lock_keys().map_err(failed)?;
    let name = key_name(target);
    let entered_as = match store.get(&name).map_err(failed)? {
        None => Entered::Saved,
        Some(saved) if saved.secret == entered => return Ok(Entered::Unchanged),
        Some(_) if force => Entered::Replaced,
        Some(saved) => match opens(&saved.secret) {
            Ok(()) => return Err(NotSaved::SavedWorks { target: shown }),
            Err(ResticError::WrongPassword { .. }) => Entered::Replaced,
            Err(e) => return Err(NotSaved::Failed(e.to_string())),
        },
    };
    store
        .set(
            &name,
            &Credential {
                username: None,
                secret: entered,
            },
        )
        .map_err(failed)?;
    tracing::info!(key = %name, replaced = entered_as == Entered::Replaced,
        "stored a repository password the user entered, after it opened the repository");
    Ok(entered_as)
}

/// A password of [`GROUPS`] groups of [`GROUP_LEN`] symbols from [`ALPHABET`], joined by `-`,
/// from `random`. Rejection sampling keeps every symbol equally likely.
fn new_password(
    mut random: impl FnMut(&mut [u8]) -> std::io::Result<()>,
) -> std::io::Result<String> {
    // The largest multiple of 31 that fits in a byte: bytes at or above it are thrown away.
    const LIMIT: u8 = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    let mut symbols = Vec::with_capacity(GROUPS * GROUP_LEN);
    let mut buffer = [0u8; 64];
    while symbols.len() < GROUPS * GROUP_LEN {
        random(&mut buffer)?;
        symbols.extend(
            buffer
                .iter()
                .filter(|&&b| b < LIMIT)
                .map(|&b| ALPHABET[usize::from(b) % ALPHABET.len()]),
        );
    }
    buffer.fill(0);
    symbols.truncate(GROUPS * GROUP_LEN);
    let password = symbols
        .chunks(GROUP_LEN)
        .map(|g| String::from_utf8_lossy(g).into_owned())
        .collect::<Vec<_>>()
        .join("-");
    symbols.fill(0);
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica_core::job::VolumeRef;
    use replica_core::platform::fake::MemorySecretStore;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn target() -> Target {
        Target::Drive {
            name: Some("external".into()),
            volume: VolumeRef {
                volume_id: None,
                label: Some("BACKUP".into()),
            },
            path: PathBuf::from("Replica"),
        }
    }

    #[test]
    fn passwords_are_six_groups_of_five_unambiguous_symbols() {
        let mut seen = HashSet::new();
        for _ in 0..200 {
            let p = new_password(win32::random_bytes).unwrap();
            assert_eq!(p.len(), 35, "{p}");
            let groups: Vec<_> = p.split('-').collect();
            assert_eq!(groups.len(), 6);
            assert!(groups.iter().all(|g| g.len() == 5));
            assert!(p.bytes().all(|b| b == b'-' || ALPHABET.contains(&b)), "{p}");
            assert!(seen.insert(p), "a repeat");
        }
    }

    #[test]
    fn bytes_past_the_last_whole_alphabet_are_thrown_away() {
        // 248..=255 would bias the first symbols; they're skipped.
        let mut calls = 0;
        let p = new_password(|b| {
            calls += 1;
            b.fill(if calls == 1 { 250 } else { 0 });
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(p, "aaaaa-aaaaa-aaaaa-aaaaa-aaaaa-aaaaa");
    }

    #[test]
    fn nothing_is_stored_until_the_user_confirms() {
        let store = MemorySecretStore::default();
        let new = generate(&target(), &store).unwrap();
        assert_eq!(new.name(), "replica:repo:drive:label=backup:replica");
        assert!(for_restic(&target(), &store).unwrap().is_none());
        assert!(show(&target(), &store).is_err());
        let shown = new.reveal().to_owned();
        assert!(!format!("{new:?}").contains(&shown), "Debug hides it");

        let secret = new.confirm_saved(&store).unwrap();
        assert_eq!(secret.expose(), shown);
        assert_eq!(show(&target(), &store).unwrap().expose(), shown);
        assert_eq!(
            for_restic(&target(), &store).unwrap().unwrap().expose(),
            shown
        );
    }

    /// A store whose `get` reads, then waits (up to a bound) for a second caller to have read
    /// too, so two unguarded confirmations would both read "none" before either writes. (With
    /// the lock off, this test fails: both store.)
    #[derive(Default)]
    struct Rendezvous {
        inner: MemorySecretStore,
        arrived: std::sync::Mutex<usize>,
        signal: std::sync::Condvar,
    }

    impl SecretStore for Rendezvous {
        fn get(&self, name: &str) -> Result<Option<Credential>, PlatformError> {
            let found = self.inner.get(name);
            let mut arrived = self.arrived.lock().unwrap();
            *arrived += 1;
            self.signal.notify_all();
            let _ = self
                .signal
                .wait_timeout_while(arrived, std::time::Duration::from_millis(300), |n| *n < 2)
                .unwrap();
            found
        }

        fn set(&self, name: &str, credential: &Credential) -> Result<(), PlatformError> {
            self.inner.set(name, credential)
        }

        fn delete(&self, name: &str) -> Result<(), PlatformError> {
            self.inner.delete(name)
        }
    }

    #[test]
    fn of_two_confirmations_at_once_only_one_stores() {
        let store = Rendezvous::default();
        let first = generate(&target(), &store.inner).unwrap();
        let second = generate(&target(), &store.inner).unwrap();
        let results: Vec<_> = std::thread::scope(|s| {
            let a = s.spawn(|| first.confirm_saved(&store));
            let b = s.spawn(|| second.confirm_saved(&store));
            vec![a.join().unwrap(), b.join().unwrap()]
        });
        let stored: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(stored.len(), 1, "exactly one confirmation stores");
        let kept = store.inner.get(&key_name(&target())).unwrap().unwrap();
        assert_eq!(
            &kept.secret, stored[0],
            "the stored password is the one handed back"
        );
    }

    /// One name for every session of a user (a Global mutex named by the SID), and a different
    /// one for another user.
    #[test]
    fn the_lock_spans_sessions_and_is_per_user() {
        let sid = win32::current_user_sid().unwrap();
        let name = keys_mutex_name(&sid);
        assert_eq!(name, format!(r"Global\Replica.RepositoryPasswords.{sid}"));
        assert_eq!(name, keys_mutex_name(&win32::current_user_sid().unwrap()));
        assert_ne!(name, keys_mutex_name("S-1-5-21-1-2-3-1002"));
    }

    const CHILD: &str = "REPLICA_KEYS_TEST_CHILD";

    /// Run only as a child of the test below: holds the real keys lock until told to let go.
    #[test]
    fn child_holds_the_keys_lock() {
        let Some(dir) = std::env::var_os(CHILD) else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let _lock = lock_keys().unwrap();
        std::fs::write(dir.join("held"), "").unwrap();
        let start = std::time::Instant::now();
        while !dir.join("release").exists() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Another process holding the lock keeps this one out until it lets go.
    #[test]
    fn another_process_holding_the_lock_keeps_this_one_out() {
        if std::env::var_os(CHILD).is_some() {
            return;
        }
        let temp = std::env::temp_dir().join(format!("replica-keys-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "keys::tests::child_holds_the_keys_lock",
                "--test-threads=1",
            ])
            .env(CHILD, &temp)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !temp.join("held").exists() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(60),
                "the child never took the lock"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let sid = win32::current_user_sid().unwrap();
        let name = keys_mutex_name(&sid);
        let blocked = win32::lock_named_mutex(&name, std::time::Duration::from_millis(200));
        let blocked_kind = blocked.as_ref().err().map(|e| e.kind());
        drop(blocked);
        std::fs::write(temp.join("release"), "").unwrap();
        let exited = child.wait().unwrap();
        let after = win32::lock_named_mutex(&name, std::time::Duration::from_secs(10)).is_ok();
        let _ = std::fs::remove_dir_all(&temp);
        assert_eq!(blocked_kind, Some(std::io::ErrorKind::TimedOut));
        assert!(exited.success());
        assert!(after, "free once the other process let go");
    }

    /// The password `replica init` stores is the one a run finds: core's
    /// `Repository::from_store` reads the name `keys` writes, and gets the value it stored.
    #[test]
    fn a_run_opens_the_repository_with_the_password_init_stored() {
        use replica_core::restic::{Repository, ResticError};
        let store = MemorySecretStore::default();
        let folder = std::path::Path::new(r"F:\Replica");
        assert!(matches!(
            Repository::from_store(folder, &target(), &store),
            Err(ResticError::PasswordMissing { .. })
        ));

        let secret = generate(&target(), &store)
            .unwrap()
            .confirm_saved(&store)
            .unwrap();
        assert_eq!(key_name(&target()), password_secret_name(&target()));
        let stored = store
            .get(&password_secret_name(&target()))
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.secret, secret,
            "under core's name, the value init returned"
        );
        let repository = Repository::from_store(folder, &target(), &store).unwrap();
        assert_eq!(repository.location, folder.to_string_lossy());
        assert_eq!(for_restic(&target(), &store).unwrap(), Some(secret));
    }

    /// Renaming the job or the target doesn't lose the password: the name follows the
    /// repository.
    #[test]
    fn a_renamed_target_keeps_its_password() {
        let store = MemorySecretStore::default();
        let secret = generate(&target(), &store)
            .unwrap()
            .confirm_saved(&store)
            .unwrap();
        let renamed = Target::Drive {
            name: Some("the big disk".into()),
            volume: VolumeRef {
                volume_id: None,
                label: Some("BACKUP".into()),
            },
            path: PathBuf::from("Replica"),
        };
        assert_eq!(show(&renamed, &store).unwrap(), secret);
    }

    /// Stands in for `Restic::opens`: the repository opens with any of `keys`.
    fn opens_with(keys: &'static [&'static str]) -> impl Fn(&Secret) -> Result<(), ResticError> {
        move |s: &Secret| {
            if keys.contains(&s.expose()) {
                Ok(())
            } else {
                Err(ResticError::WrongPassword {
                    location: r"F:\Replica".into(),
                })
            }
        }
    }

    fn stored(store: &MemorySecretStore) -> Option<String> {
        for_restic(&target(), store)
            .unwrap()
            .map(|s| s.expose().to_owned())
    }

    #[test]
    fn an_entered_password_is_saved_only_if_it_opens_the_repository() {
        let store = MemorySecretStore::default();
        let opens = opens_with(&["right"]);
        let wrong = save_entered(&target(), Secret::new("wrong"), &store, false, &opens);
        assert!(matches!(wrong, Err(NotSaved::Wrong { .. })), "{wrong:?}");
        let empty = save_entered(&target(), Secret::new(""), &store, true, &opens);
        assert!(matches!(empty, Err(NotSaved::Wrong { .. })), "{empty:?}");
        assert_eq!(stored(&store), None, "nothing saved");

        let saved = save_entered(&target(), Secret::new("right"), &store, false, &opens);
        assert_eq!(saved, Ok(Entered::Saved));
        assert_eq!(stored(&store).as_deref(), Some("right"));
        let again = save_entered(&target(), Secret::new("right"), &store, false, &opens);
        assert_eq!(again, Ok(Entered::Unchanged));
    }

    #[test]
    fn a_saved_password_that_opens_the_repository_is_replaced_only_when_forced() {
        let store = MemorySecretStore::default();
        let opens = opens_with(&["right", "also-right"]);
        save_entered(&target(), Secret::new("right"), &store, false, &opens).unwrap();
        let refused = save_entered(&target(), Secret::new("also-right"), &store, false, &opens);
        assert!(
            matches!(refused, Err(NotSaved::SavedWorks { .. })),
            "{refused:?}"
        );
        assert_eq!(stored(&store).as_deref(), Some("right"));
        let wrong = save_entered(&target(), Secret::new("wrong"), &store, true, &opens);
        assert!(
            matches!(wrong, Err(NotSaved::Wrong { .. })),
            "forced or not, a wrong one isn't saved"
        );
        assert_eq!(stored(&store).as_deref(), Some("right"));

        let forced = save_entered(&target(), Secret::new("also-right"), &store, true, &opens);
        assert_eq!(forced, Ok(Entered::Replaced));
        assert_eq!(stored(&store).as_deref(), Some("also-right"));
    }

    #[test]
    fn a_saved_password_that_doesnt_open_the_repository_is_replaced() {
        let store = MemorySecretStore::default();
        store
            .set(
                &key_name(&target()),
                &Credential {
                    username: None,
                    secret: Secret::new("stale"),
                },
            )
            .unwrap();
        let replaced = save_entered(
            &target(),
            Secret::new("right"),
            &store,
            false,
            &opens_with(&["right"]),
        );
        assert_eq!(replaced, Ok(Entered::Replaced));
        assert_eq!(stored(&store).as_deref(), Some("right"));
    }

    #[test]
    fn a_password_that_cant_be_checked_isnt_saved() {
        let store = MemorySecretStore::default();
        let no_repository = |_: &Secret| {
            Err(ResticError::RepoNotFound {
                location: r"F:\Replica".into(),
            })
        };
        let result = save_entered(&target(), Secret::new("x"), &store, true, &no_repository);
        assert!(matches!(result, Err(NotSaved::Failed(_))), "{result:?}");
        assert_eq!(stored(&store), None);
    }

    #[test]
    fn a_stored_password_is_never_replaced() {
        let store = MemorySecretStore::default();
        let first = generate(&target(), &store).unwrap();
        let second = generate(&target(), &store).unwrap();
        let kept = first.confirm_saved(&store).unwrap();
        assert!(second.confirm_saved(&store).is_err(), "stored meanwhile");
        assert!(generate(&target(), &store).is_err());
        assert_eq!(show(&target(), &store).unwrap(), kept);
    }
}
