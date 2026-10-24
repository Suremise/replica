//! The platform traits: how the core reaches the OS.
//!
//! `replica-core` never calls the OS for secrets, volumes, shares or notifications. It goes
//! through these three traits, which `replica-win` implements for Windows (Credential Manager,
//! volume GUIDs and labels, `WNetAddConnection2`, toast notifications). [`fake`] has in-memory
//! versions for tests.
//!
//! # For implementers (`replica-win`)
//!
//! - Every method may be called from any thread: the traits are `Send + Sync`.
//! - Report an OS failure as [`PlatformError`] with a short, plain message (it's shown to the
//!   user and written to the run's log). A missing secret, volume or share is **not** an error:
//!   return `Ok(None)` or [`ShareStatus::NotAvailable`], which the run turns into "skipped:
//!   target not available" rather than "failed".
//! - Never log a [`Secret`]'s value. Its `Debug` output is redacted; keep it that way.
//! - [`resolve_target`] puts the pieces together for the run; implementers don't need to.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{ChildStderr, ChildStdout, Command, ExitStatus};

use thiserror::Error;

use crate::job::{self, Target, VolumeRef};
use crate::tr;

/// An OS call that failed (not a thing that's merely missing).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct PlatformError(pub String);

/// A secret's value. Its `Debug` and `Display` never show it; read it with [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The value, for handing to restic or the OS. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// What the secret store holds under a name: a secret, and a user name for logins.
///
/// A share login has both. A restic repository password has only the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub username: Option<String>,
    pub secret: Secret,
}

/// Where secrets live: Windows Credential Manager in `replica-win`.
///
/// Names are the references in `settings.toml` (a target's `credential`) and names the core
/// makes for repository passwords. The store adds its own prefix if it needs one.
pub trait SecretStore: Send + Sync {
    /// The credential stored under `name`, or `None` if there isn't one.
    fn get(&self, name: &str) -> Result<Option<Credential>, PlatformError>;
    /// Stores a credential under `name`, replacing any there.
    fn set(&self, name: &str, credential: &Credential) -> Result<(), PlatformError>;
    /// Removes the credential under `name`. Removing one that isn't there is not an error.
    fn delete(&self, name: &str) -> Result<(), PlatformError>;
}

/// Whether a share could be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareStatus {
    /// It's reachable, now that it's connected.
    Connected,
    /// It can't be reached (the NAS is off, the network is down, the share is gone, the
    /// connection timed out). The reason is for the log and the window.
    ///
    /// A share that refuses its login (a wrong password, the account locked or expired), or is
    /// already connected with a different login, is reachable: that's a [`PlatformError`], and
    /// the run fails rather than skips.
    NotAvailable(String),
}

/// Finds targets: drives by volume, and shares by connecting to them.
pub trait TargetResolver: Send + Sync {
    /// The current root of a volume (`E:\`), found by its ID first and then its label, or
    /// `None` if no connected volume matches.
    fn locate_volume(&self, volume: &VolumeRef) -> Result<Option<PathBuf>, PlatformError>;

    /// Connects a UNC share (`\\nas\share`, or a path within it) for this user, with a login if
    /// given. Already connected is [`ShareStatus::Connected`]; a refused login is an error (see
    /// [`ShareStatus::NotAvailable`]).
    fn connect_share(
        &self,
        unc: &str,
        login: Option<&Credential>,
    ) -> Result<ShareStatus, PlatformError>;
}

/// How important a notification is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

/// A notification for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub level: Level,
    /// The job it's about, if any.
    pub job: Option<String>,
    pub title: String,
    pub body: String,
}

/// Shows notifications: Windows toasts in `replica-win`.
pub trait Notifier: Send + Sync {
    fn notify(&self, notification: &Notification) -> Result<(), PlatformError>;
}

/// Runs a program so that it and everything it starts can be stopped as one: on Windows, a job
/// object with kill-on-close (`replica-win`). The run's command steps use it.
///
/// # For implementers
///
/// - [`ProcessTree::spawn`] starts the program already inside its tree, so nothing it starts
///   can escape (on Windows: created suspended, assigned to the job, then resumed). If the tree
///   can't be made or the program can't be put in it, that's an error and nothing is left
///   running.
/// - [`TreeChild::kill_tree`] stops every process in the tree and returns only once the tree
///   reports none left; if it can't, that's an error, never a quiet success.
/// - Dropping a [`TreeChild`] stops whatever is still in the tree (kill-on-close), so a step's
///   programs never outlive it, even if Replica itself stops.
pub trait ProcessTree: Send + Sync {
    /// Starts `command` (its stdio already set up) in a new tree of its own.
    fn spawn(&self, command: Command) -> io::Result<Box<dyn TreeChild>>;
}

/// A program started in a [`ProcessTree`], and the tree it's in.
pub trait TreeChild: Send {
    /// The program's process ID.
    fn id(&self) -> u32;
    fn take_stdout(&mut self) -> Option<ChildStdout>;
    fn take_stderr(&mut self) -> Option<ChildStderr>;
    /// The program's exit status, if it has exited (programs it started may still run).
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    /// Stops every process in the tree, and returns once the tree has none left.
    fn kill_tree(&mut self) -> io::Result<()>;
    /// The IDs of the processes in the tree now, as the tree itself reports them.
    fn processes(&self) -> io::Result<Vec<u32>>;
}

/// Where a target is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetLocation {
    /// Its folder, ready to use.
    Available(PathBuf),
    /// It isn't connected or reachable: the run is "skipped", not "failed".
    NotAvailable(String),
}

/// Finds a target's current folder: a drive's volume root joined with its path, or a share
/// connected with its login from the secret store.
pub fn resolve_target(
    target: &Target,
    resolver: &dyn TargetResolver,
    secrets: &dyn SecretStore,
) -> Result<TargetLocation, PlatformError> {
    match target {
        // Validation refuses these too; checked again so a joined path can never leave the volume.
        Target::Drive { path, .. } if !job::is_inside_volume(path) => Err(PlatformError(tr!(
            "error-job-drive-path-outside",
            path = path.display().to_string()
        ))),
        Target::Drive { volume, path, .. } => Ok(match resolver.locate_volume(volume)? {
            Some(root) => TargetLocation::Available(root.join(path)),
            None => TargetLocation::NotAvailable(tr!(
                "error-drive-not-connected",
                volume = describe(volume)
            )),
        }),
        Target::Unc {
            path, credential, ..
        } => {
            let login = match credential {
                Some(name) => match secrets.get(name)? {
                    Some(c) => Some(c),
                    None => {
                        return Err(PlatformError(tr!(
                            "error-credentials-missing",
                            target = target.display_name()
                        )));
                    }
                },
                None => None,
            };
            Ok(match resolver.connect_share(path, login.as_ref())? {
                ShareStatus::Connected => TargetLocation::Available(PathBuf::from(path)),
                ShareStatus::NotAvailable(why) => TargetLocation::NotAvailable(why),
            })
        }
    }
}

fn describe(volume: &VolumeRef) -> String {
    match (&volume.label, &volume.volume_id) {
        (Some(label), _) => format!("\"{label}\""),
        (None, Some(id)) => id.clone(),
        // Validation requires one or the other.
        (None, None) => "?".to_string(),
    }
}

/// A process "tree" of the program alone: it stops only the program itself, not what that
/// started. restic runs in it when no platform tree is given ([`crate::restic::Restic::with_tree`]),
/// and tests use it where stopping the tree doesn't matter. Real runs use the platform's tree
/// (`replica-win`'s job object).
#[derive(Debug, Default, Clone, Copy)]
pub struct PlainTree;

impl ProcessTree for PlainTree {
    fn spawn(&self, mut command: Command) -> io::Result<Box<dyn TreeChild>> {
        Ok(Box::new(PlainChild(command.spawn()?)))
    }
}

struct PlainChild(std::process::Child);

impl TreeChild for PlainChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.0.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.0.stderr.take()
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.0.try_wait()
    }

    fn kill_tree(&mut self) -> io::Result<()> {
        let _ = self.0.kill();
        self.0.wait().map(|_| ())
    }

    fn processes(&self) -> io::Result<Vec<u32>> {
        Ok(vec![self.0.id()])
    }
}

impl Drop for PlainChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// In-memory implementations for tests, here and in other crates.
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    pub use super::PlainTree;

    /// A secret store in a map.
    #[derive(Default)]
    pub struct MemorySecretStore {
        pub entries: Mutex<HashMap<String, Credential>>,
    }

    impl SecretStore for MemorySecretStore {
        fn get(&self, name: &str) -> Result<Option<Credential>, PlatformError> {
            Ok(self.entries.lock().unwrap().get(name).cloned())
        }

        fn set(&self, name: &str, credential: &Credential) -> Result<(), PlatformError> {
            self.entries
                .lock()
                .unwrap()
                .insert(name.to_string(), credential.clone());
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<(), PlatformError> {
            self.entries.lock().unwrap().remove(name);
            Ok(())
        }
    }

    /// Volumes and shares set up by the test.
    #[derive(Default)]
    pub struct FakeResolver {
        /// Connected volumes: (ID, label, root).
        pub volumes: Vec<(Option<String>, Option<String>, PathBuf)>,
        /// Reachable shares, by UNC prefix, with the user name they need (`None`: no login). Any
        /// other login is refused, an error as on Windows.
        pub shares: Vec<(String, Option<String>)>,
        /// Every `connect_share` call: the path and the user name given.
        pub connects: Mutex<Vec<(String, Option<String>)>>,
    }

    impl TargetResolver for FakeResolver {
        fn locate_volume(&self, volume: &VolumeRef) -> Result<Option<PathBuf>, PlatformError> {
            let by_id = volume
                .volume_id
                .as_ref()
                .and_then(|id| self.volumes.iter().find(|v| v.0.as_ref() == Some(id)));
            let by_label = || {
                volume.label.as_ref().and_then(|l| {
                    self.volumes
                        .iter()
                        .find(|v| v.1.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(l)))
                })
            };
            Ok(by_id.or_else(by_label).map(|v| v.2.clone()))
        }

        fn connect_share(
            &self,
            unc: &str,
            login: Option<&Credential>,
        ) -> Result<ShareStatus, PlatformError> {
            let user = login.and_then(|l| l.username.clone());
            self.connects
                .lock()
                .unwrap()
                .push((unc.to_string(), user.clone()));
            let lower = unc.to_ascii_lowercase();
            match self
                .shares
                .iter()
                .find(|s| lower.starts_with(&s.0.to_ascii_lowercase()))
            {
                Some((_, needed)) if needed.is_none() || *needed == user => {
                    Ok(ShareStatus::Connected)
                }
                Some(_) => Err(PlatformError("the share refused the login".into())),
                None => Ok(ShareStatus::NotAvailable(
                    "the share can't be reached".into(),
                )),
            }
        }
    }

    /// Records notifications.
    #[derive(Default)]
    pub struct RecordingNotifier {
        pub sent: Mutex<Vec<Notification>>,
    }

    impl Notifier for RecordingNotifier {
        fn notify(&self, notification: &Notification) -> Result<(), PlatformError> {
            self.sent.lock().unwrap().push(notification.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn drive(id: Option<&str>, label: Option<&str>) -> Target {
        Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: id.map(Into::into),
                label: label.map(Into::into),
            },
            path: PathBuf::from("Replica").join("Hive"),
        }
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let c = Credential {
            username: Some("alex".into()),
            secret: Secret::new("hunter2"),
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(c.secret.expose(), "hunter2");
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemorySecretStore::default();
        let c = Credential {
            username: None,
            secret: Secret::new("pw"),
        };
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", &c).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(c));
        store.delete("a").unwrap();
        store.delete("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
    }

    #[test]
    fn drives_resolve_by_id_then_label_whatever_the_letter() {
        let resolver = FakeResolver {
            volumes: vec![
                (
                    Some("vol-1".into()),
                    Some("OTHER".into()),
                    PathBuf::from("E:/"),
                ),
                (
                    Some("vol-2".into()),
                    Some("BACKUP".into()),
                    PathBuf::from("F:/"),
                ),
            ],
            ..Default::default()
        };
        let secrets = MemorySecretStore::default();
        let at = |t| resolve_target(&t, &resolver, &secrets).unwrap();
        assert_eq!(
            at(drive(Some("vol-2"), None)),
            TargetLocation::Available(PathBuf::from("F:/Replica/Hive"))
        );
        assert_eq!(
            at(drive(None, Some("backup"))),
            TargetLocation::Available(PathBuf::from("F:/Replica/Hive"))
        );
        assert_eq!(
            at(drive(Some("gone"), Some("BACKUP"))),
            TargetLocation::Available(PathBuf::from("F:/Replica/Hive")),
            "falls back to the label"
        );
        assert!(matches!(
            at(drive(Some("gone"), None)),
            TargetLocation::NotAvailable(_)
        ));
    }

    #[test]
    fn a_drive_path_can_never_leave_its_volume() {
        let resolver = FakeResolver {
            volumes: vec![(None, Some("BACKUP".into()), PathBuf::from("D:/"))],
            ..Default::default()
        };
        let secrets = MemorySecretStore::default();
        for bad in ["C:outside", "../outside", r"x\..\..\y", r"\abs"] {
            let target = Target::Drive {
                name: None,
                volume: VolumeRef {
                    volume_id: None,
                    label: Some("BACKUP".into()),
                },
                path: bad.into(),
            };
            assert!(
                resolve_target(&target, &resolver, &secrets).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn shares_connect_with_their_stored_login() {
        let resolver = FakeResolver {
            shares: vec![(r"\\nas\backup".into(), Some("alex".into()))],
            ..Default::default()
        };
        let secrets = MemorySecretStore::default();
        let target = |cred: Option<&str>| Target::Unc {
            name: None,
            path: r"\\nas\backup\hive".into(),
            credential: cred.map(Into::into),
        };
        assert!(
            resolve_target(&target(None), &resolver, &secrets).is_err(),
            "a refused login is an error, not \"not available\""
        );
        assert!(
            resolve_target(&target(Some("nas")), &resolver, &secrets).is_err(),
            "a missing credential is an error"
        );
        secrets
            .set(
                "nas",
                &Credential {
                    username: Some("alex".into()),
                    secret: Secret::new("pw"),
                },
            )
            .unwrap();
        assert_eq!(
            resolve_target(&target(Some("nas")), &resolver, &secrets).unwrap(),
            TargetLocation::Available(PathBuf::from(r"\\nas\backup\hive"))
        );
        assert_eq!(
            resolver
                .connects
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .as_deref(),
            Some("alex")
        );
    }

    #[test]
    fn notifier_records() {
        let n = RecordingNotifier::default();
        let note = Notification {
            level: Level::Error,
            job: Some("hive".into()),
            title: "t".into(),
            body: "b".into(),
        };
        n.notify(&note).unwrap();
        assert_eq!(*n.sent.lock().unwrap(), [note]);
    }
}
