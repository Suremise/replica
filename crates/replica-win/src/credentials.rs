//! [`SecretStore`] on Windows Credential Manager.
//!
//! Each secret is a generic credential named `Replica/<name>` (the prefix is a parameter, so
//! tests use their own), kept for this user on this PC: it doesn't roam with the profile. The
//! secret is stored as UTF-16, as Windows' own Credential Manager window stores passwords, so a
//! NAS login can also be added there by hand: a generic credential with the address
//! `Replica/<name>`, a user name and a password.
//!
//! Secrets never leave this module except as a [`Secret`], whose `Debug` output is redacted.
//!
//! **One call at a time, across Replica's processes** ([`in_turn`]): Credential Manager loses
//! changes made at the same moment, from parallel threads of one process or from separate
//! processes (a write or a delete reports success and isn't kept). The tray's job thread
//! reads credentials while its window writes them, and the CLI or a scheduled run can write
//! beside the tray. So each call takes a per-user named mutex, and a change is read back and made
//! again if it wasn't kept (another app may write its own credentials at the same moment).

use std::io;
use std::sync::OnceLock;
use std::time::Duration;

use replica_core::platform::{Credential, PlatformError, Secret, SecretStore};
use replica_core::tr;

use crate::win32;

/// The prefix of Replica's credentials.
pub const PREFIX: &str = "Replica/";

/// How long a call waits for another one (from any of this user's Replica processes) to end.
const CALLS_WAIT: Duration = Duration::from_secs(30);
/// How many times a change Credential Manager didn't keep is made.
const ATTEMPTS: usize = 3;

/// The named mutex every call into Credential Manager holds: `Global\Replica.Credentials.<user
/// SID>`, spanning the user's logon sessions as their credentials do (as `keys`' mutex does).
fn calls_mutex_name(user_sid: &str) -> String {
    format!(r"Global\Replica.Credentials.{user_sid}")
}

/// Runs `call` holding the calls' mutex (see the module's notes).
fn in_turn<T>(call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    static SID: OnceLock<String> = OnceLock::new();
    let sid = match SID.get() {
        Some(sid) => sid,
        None => {
            let sid = win32::current_user_sid()?;
            SID.get_or_init(|| sid)
        }
    };
    let _turn = win32::lock_named_mutex(&calls_mutex_name(sid), CALLS_WAIT).map_err(busy)?;
    call()
}

/// The calls' mutex timing out, in Replica's words (the helper's own text is English and names
/// the mutex); any other error, which is Windows' own, as it is.
fn busy(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::TimedOut {
        io::Error::new(io::ErrorKind::TimedOut, tr!("error-credential-busy"))
    } else {
        e
    }
}

/// A change Credential Manager reported done and didn't keep, every time it was made.
fn not_kept() -> io::Error {
    io::Error::other(tr!("error-credential-not-kept"))
}

/// Credential Manager, under a name prefix.
#[derive(Debug, Clone)]
pub struct CredentialManager {
    prefix: String,
}

impl Default for CredentialManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialManager {
    /// Replica's credentials: `Replica/<name>`.
    pub fn new() -> Self {
        Self::with_prefix(PREFIX)
    }

    /// Credentials named `<prefix><name>`: for tests, which use a prefix of their own.
    pub fn with_prefix(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
        }
    }

    /// The full Credential Manager name for `name`.
    pub fn target_name(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// The names (without the prefix) of every credential under this prefix.
    pub fn names(&self) -> Result<Vec<String>, PlatformError> {
        let filter = format!("{}*", self.prefix);
        let names = in_turn(|| win32::cred_list(&filter)).map_err(|e| {
            PlatformError(tr!(
                "error-credential-read",
                name = filter.as_str(),
                detail = e.to_string()
            ))
        })?;
        Ok(names
            .into_iter()
            .filter_map(|n| n.strip_prefix(&self.prefix).map(str::to_owned))
            .collect())
    }
}

impl SecretStore for CredentialManager {
    fn get(&self, name: &str) -> Result<Option<Credential>, PlatformError> {
        let target = self.target_name(name);
        let Some(stored) = in_turn(|| win32::cred_read(&target)).map_err(|e| {
            PlatformError(tr!(
                "error-credential-read",
                name = target.as_str(),
                detail = e.to_string()
            ))
        })?
        else {
            return Ok(None);
        };
        let secret = decode(&stored.blob).ok_or_else(|| {
            PlatformError(tr!("error-credential-unreadable", name = target.as_str()))
        })?;
        Ok(Some(Credential {
            username: stored.username,
            secret: Secret::new(secret),
        }))
    }

    fn set(&self, name: &str, credential: &Credential) -> Result<(), PlatformError> {
        let target = self.target_name(name);
        let mut blob = encode(credential.secret.expose());
        let username = credential.username.as_deref();
        let result = in_turn(|| {
            for _ in 0..ATTEMPTS {
                win32::cred_write(&target, username, &blob)?;
                if let Some(mut stored) = win32::cred_read(&target)? {
                    let kept = stored.username.as_deref() == username && stored.blob == blob;
                    stored.blob.fill(0);
                    if kept {
                        return Ok(());
                    }
                }
            }
            Err(not_kept())
        });
        blob.fill(0);
        result.map_err(|e| {
            PlatformError(tr!(
                "error-credential-write",
                name = target.as_str(),
                detail = e.to_string()
            ))
        })
    }

    fn delete(&self, name: &str) -> Result<(), PlatformError> {
        let target = self.target_name(name);
        let deleted = in_turn(|| {
            for _ in 0..ATTEMPTS {
                win32::cred_delete(&target)?;
                if win32::cred_read(&target)?.is_none() {
                    return Ok(());
                }
            }
            Err(not_kept())
        });
        deleted.map_err(|e| {
            PlatformError(tr!(
                "error-credential-delete",
                name = target.as_str(),
                detail = e.to_string()
            ))
        })
    }
}

/// UTF-16LE, without a terminating NUL.
fn encode(secret: &str) -> Vec<u8> {
    secret.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// UTF-16LE, as Windows stores passwords; `None` if it isn't (an odd length or bad UTF-16).
fn decode(blob: &[u8]) -> Option<String> {
    if !blob.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = blob
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    let text = String::from_utf16(&units).ok()?;
    // Some tools store a terminating NUL.
    Some(text.trim_end_matches('\0').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_as_utf16_and_back() {
        for secret in ["", "pw", "pässwörd 🔑", "a\0"] {
            let blob = encode(secret);
            assert_eq!(blob.len() % 2, 0);
            assert_eq!(decode(&blob).unwrap(), secret.trim_end_matches('\0'));
        }
        assert_eq!(decode(&[0x41, 0x00, 0x00, 0x00]).unwrap(), "A");
        assert!(decode(&[1, 2, 3]).is_none());
        assert!(
            decode(&0xD800u16.to_le_bytes()).is_none(),
            "a lone surrogate"
        );
    }

    #[test]
    fn a_busy_mutex_is_reported_in_replicas_words() {
        let timed_out = io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{} is held by another process",
                calls_mutex_name("S-1-5-21-1")
            ),
        );
        let shown = busy(timed_out);
        assert_eq!(shown.kind(), io::ErrorKind::TimedOut);
        assert_eq!(shown.to_string(), tr!("error-credential-busy"));
        assert!(!shown.to_string().contains("Global"));

        let windows = io::Error::from_raw_os_error(5);
        let text = windows.to_string();
        assert_eq!(
            busy(windows).to_string(),
            text,
            "Windows' own errors pass through"
        );
    }

    #[test]
    fn names_carry_the_prefix() {
        assert_eq!(CredentialManager::new().target_name("nas"), "Replica/nas");
        assert_eq!(
            CredentialManager::with_prefix("T/").target_name("nas"),
            "T/nas"
        );
    }
}
