//! The restic driver: Replica's engine for versioned targets.
//!
//! It runs restic's CLI with `--json` and turns the output into typed results and errors. It
//! never writes a password on a command line or in a log: the password is only ever in
//! restic's environment (`RESTIC_PASSWORD`).
//!
//! # For the run orchestrator
//!
//! 1. **Find restic** once: [`ResticSearch::find`] (the settings override, then the bundled copy
//!    beside the exe, then, in debug builds only, `third_party/restic/bin/`; never `PATH`), then
//!    [`Restic::new`], which runs `restic version`, and [`Restic::with_tree`] with the
//!    platform's process tree (`replica_win::jobs::JobObjectTree`). If [`Restic::is_pinned`] is
//!    false, warn: it has already been logged.
//! 2. **Open the repository** for a target: [`Repository::from_store`] with the target's folder
//!    (from [`crate::platform::resolve_target`]) and the target; the password comes from the
//!    secret store under [`password_secret_name`]. [`ResticError::PasswordMissing`] means
//!    `replica init` hasn't been run for it.
//! 3. **Run:**
//!    - [`Restic::init`] for a new target;
//!    - [`Restic::backup`] with the job's name (its snapshots' tag), its sources and its
//!      [`ExcludeRules`], a progress callback and a [`CancelToken`]. A snapshot with unreadable
//!      files is `Ok` with [`BackupOutcome::partial`] set and the files in
//!      [`BackupOutcome::unreadable`]: the run is "partly done", not failed;
//!    - [`Restic::forget`] with the job's [`Retention`], then [`Restic::prune`];
//!    - [`Restic::check`] for a verify step;
//!    - [`Restic::snapshots`], [`Restic::ls`] and [`Restic::restore`] for the window and the CLI.
//! 4. **Errors** are [`ResticError`]: map [`ResticError::RepoNotFound`] to "init it first",
//!    [`ResticError::WrongPassword`], [`ResticError::Locked`] (offer [`Restic::unlock_stale`],
//!    which removes only locks restic itself judges stale) and [`ResticError::Cancelled`] to
//!    their own results; anything else is [`ResticError::Failed`] with restic's message.
//!
//! **Cancelling** stops restic's whole process tree: each command runs in the
//! [`ProcessTree`] given to [`Restic::with_tree`], and the cancel returns only once the tree has
//! no process left. When a command ends, anything it left in its tree is stopped. A tree that
//! has to be stopped and can't be is [`ResticError::NotStopped`], never a success or a cancel. For local and UNC repositories restic starts no
//! child processes; SFTP and rclone repositories do. A killed restic can't remove its own lock,
//! and restic on Windows doesn't treat a dead process's lock as stale for 30 minutes, so the
//! driver removes the locks that carry the killed restic's PID itself. The repository stays
//! consistent: restic only ever adds files and writes the index last.

mod messages;
mod process;

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub use messages::{
    BackupProgress, BackupSummary, CheckSummary, ExitError, Node, RestoreSummary, Snapshot,
};
use messages::{ForgetGroup, Line, LockInfo, VersionInfo};

use crate::cancel::CancelToken;
use crate::exclude::ExcludeRules;
use crate::i18n::tr;
use crate::job::{Retention, Target};
use crate::paths;
use crate::platform::{PlainTree, ProcessTree, Secret, SecretStore};
use crate::tr;

/// The exe's file name.
pub const EXE_NAME: &str = if cfg!(windows) {
    "restic.exe"
} else {
    "restic"
};

/// The restic version Replica bundles and is tested with, from `third_party/restic/VERSION`.
pub fn pinned_version() -> &'static str {
    static PINNED: OnceLock<String> = OnceLock::new();
    PINNED.get_or_init(|| {
        include_str!("../../../../third_party/restic/VERSION")
            .lines()
            .find_map(|l| l.trim().strip_prefix("version="))
            .expect("third_party/restic/VERSION has a version= line")
            .trim()
            .to_string()
    })
}

/// restic's exit codes (restic 0.17 and later).
mod exit {
    pub const OK: i32 = 0;
    pub const PARTIAL: i32 = 3;
    pub const NO_REPOSITORY: i32 = 10;
    pub const LOCKED: i32 = 11;
    pub const WRONG_PASSWORD: i32 = 12;
}

/// Something restic couldn't do. Its text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResticError {
    /// No restic where it was looked for. `path` is the first place looked.
    NotFound {
        path: PathBuf,
    },
    /// restic couldn't be started or waited for.
    Spawn {
        path: PathBuf,
        detail: String,
    },
    /// There's no repository at the location: `init` it first.
    RepoNotFound {
        location: String,
    },
    WrongPassword {
        location: String,
    },
    /// No password stored for this target.
    PasswordMissing {
        target: String,
    },
    /// Another restic holds a lock that this command can't share.
    Locked {
        location: String,
        detail: String,
    },
    Cancelled,
    /// Its process tree had to be stopped (a cancel, or a program restic started still running
    /// after restic exited) but couldn't be: restic or programs it started may still be running.
    /// `detail` says why.
    NotStopped {
        detail: String,
    },
    /// The restore folder exists and isn't empty.
    RestoreTargetNotEmpty {
        path: PathBuf,
    },
    /// The restore folder is a source of the job, or inside or around one.
    RestoreOverSource {
        path: PathBuf,
    },
    /// The restore folder (or a source) couldn't be resolved, so it can't be checked.
    RestoreTargetInvalid {
        path: PathBuf,
        detail: String,
    },
    /// The file or folder asked for isn't in the snapshot (or restic didn't restore it).
    RestoreNothing {
        item: String,
    },
    /// Two items to restore into one folder have the same name, so one would land on the other.
    RestoreSameName {
        name: String,
    },
    /// The job has no snapshot by that ID (or `latest`: no snapshot at all). An ID that's
    /// another job's, or the start of more than one, doesn't count.
    SnapshotNotFound {
        job: String,
        snapshot: String,
    },
    /// restic's output wasn't what was expected.
    BadOutput {
        detail: String,
    },
    /// Anything else, with restic's exit code and message.
    Failed {
        code: i32,
        detail: String,
    },
}

impl fmt::Display for ResticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = |p: &PathBuf| p.display().to_string();
        let text = match self {
            ResticError::NotFound { path: p } => tr!("error-restic-missing", path = path(p)),
            ResticError::Spawn { path: p, detail } => {
                tr!(
                    "error-restic-spawn",
                    path = path(p),
                    detail = detail.as_str()
                )
            }
            ResticError::RepoNotFound { location } => {
                tr!("error-repo-not-found", location = location.as_str())
            }
            ResticError::WrongPassword { location } => {
                tr!("error-wrong-password", target = location.as_str())
            }
            ResticError::PasswordMissing { target } => {
                tr!("error-password-missing", target = target.as_str())
            }
            ResticError::Locked { location, detail } => tr!(
                "error-repo-locked",
                location = location.as_str(),
                detail = detail.as_str()
            ),
            ResticError::Cancelled => tr("error-cancelled"),
            ResticError::NotStopped { detail } => {
                tr!("error-restic-not-stopped", detail = detail.as_str())
            }
            ResticError::RestoreTargetNotEmpty { path: p } => {
                tr!("error-restore-target-not-empty", path = path(p))
            }
            ResticError::RestoreOverSource { path: p } => {
                tr!("error-restore-over-source", path = path(p))
            }
            ResticError::RestoreTargetInvalid { path: p, detail } => tr!(
                "error-restore-target-invalid",
                path = path(p),
                detail = detail.as_str()
            ),
            ResticError::RestoreNothing { item } => {
                tr!("error-restore-nothing", item = item.as_str())
            }
            ResticError::RestoreSameName { name } => {
                tr!("error-restore-same-name", name = name.as_str())
            }
            ResticError::SnapshotNotFound { job, snapshot } => tr!(
                "error-snapshot-not-found",
                job = job.as_str(),
                snapshot = snapshot.as_str()
            ),
            ResticError::BadOutput { detail } => {
                tr!("error-restic-bad-output", detail = detail.as_str())
            }
            ResticError::Failed { code, detail } => {
                tr!(
                    "error-restic-failed-code",
                    code = *code,
                    detail = detail.as_str()
                )
            }
        };
        f.write_str(&text)
    }
}

impl std::error::Error for ResticError {}

/// Where to look for restic, in order. `PATH` is never searched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResticSearch {
    /// The settings' `restic_path`. If set, it's the only place looked.
    pub override_path: Option<PathBuf>,
    /// The folder of Replica's exe, where the bundled restic ships.
    pub exe_dir: Option<PathBuf>,
    /// The development copy (`third_party/restic/bin/`).
    pub dev_dir: Option<PathBuf>,
}

impl ResticSearch {
    /// The search for this process: the override, then beside the running exe, then, in debug
    /// builds only, the repository's `third_party/restic/bin/`.
    pub fn for_this_exe(override_path: Option<PathBuf>) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let dev_dir = cfg!(debug_assertions)
            .then(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/restic/bin"));
        ResticSearch {
            override_path,
            exe_dir,
            dev_dir,
        }
    }

    /// The first restic that exists, in search order.
    pub fn find(&self) -> Result<PathBuf, ResticError> {
        if let Some(path) = &self.override_path {
            return if path.is_file() {
                Ok(path.clone())
            } else {
                Err(ResticError::NotFound { path: path.clone() })
            };
        }
        let candidates: Vec<PathBuf> = [&self.exe_dir, &self.dev_dir]
            .into_iter()
            .flatten()
            .map(|dir| dir.join(EXE_NAME))
            .collect();
        candidates
            .iter()
            .find(|p| p.is_file())
            .cloned()
            .ok_or_else(|| ResticError::NotFound {
                path: candidates
                    .first()
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(EXE_NAME)),
            })
    }
}

/// A restic repository and how to open it.
#[derive(Clone)]
pub struct Repository {
    /// The repository: a folder (local, external drive or UNC).
    pub location: String,
    password: Secret,
    /// restic's cache folder; `None` uses restic's default.
    pub cache_dir: Option<PathBuf>,
}

impl fmt::Debug for Repository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Repository")
            .field("location", &self.location)
            .field("password", &self.password)
            .field("cache_dir", &self.cache_dir)
            .finish()
    }
}

impl Repository {
    pub fn new(location: impl Into<String>, password: Secret) -> Self {
        Repository {
            location: location.into(),
            password,
            cache_dir: None,
        }
    }

    /// The repository at `location` (the target's resolved folder), with the target's password
    /// from the secret store.
    pub fn from_store(
        location: &Path,
        target: &Target,
        secrets: &dyn SecretStore,
    ) -> Result<Self, ResticError> {
        let missing = || ResticError::PasswordMissing {
            target: target.display_name(),
        };
        let credential = secrets
            .get(&password_secret_name(target))
            .map_err(|_| missing())?
            .ok_or_else(missing)?;
        Ok(Repository::new(
            location.to_string_lossy(),
            credential.secret,
        ))
    }

    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(dir.into());
        self
    }
}

/// The secret store name of a versioned target's repository password. It follows the target,
/// not its current drive letter: a drive by its volume ID or label and path, a share by its UNC
/// path, all lower case.
pub fn password_secret_name(target: &Target) -> String {
    let key = match target {
        Target::Drive { volume, path, .. } => {
            let vol = volume
                .volume_id
                .as_deref()
                .map(|id| format!("id={id}"))
                .or_else(|| volume.label.as_deref().map(|l| format!("label={l}")))
                .unwrap_or_default();
            format!("drive:{vol}:{}", path.to_string_lossy().replace('/', "\\"))
        }
        Target::Unc { path, .. } => format!("unc:{}", path.replace('/', "\\")),
    };
    format!(
        "{}{}",
        crate::job::REPOSITORY_SECRET_PREFIX,
        key.to_lowercase()
    )
}

/// The tag of a pinned snapshot. Every `forget` Replica runs keeps the snapshots that
/// carry it (`--keep-tag`), whatever the job's retention, in every job: a pin belongs to one
/// snapshot, so it's safe in a repository jobs share.
///
/// A job's snapshots carry its name as a tag, so the pin's tag must never be a job's name: it
/// has a colon, which job names can't (`job::is_valid_name`). Pinning and unpinning add and remove
/// only this tag, never a job's.
pub const PINNED_TAG: &str = "replica:pinned";

/// The file beside a versioned target's repository that says how to restore it without Replica.
pub const RESTORE_GUIDE: &str = "HOW-TO-RESTORE.txt";

/// What [`RESTORE_GUIDE`] says for a repository at `location`: that it's a restic repository
/// made by Replica, readable by any restic with the password, and the commands to list and
/// restore its snapshots with its path filled in. It never holds the password or any secret.
pub fn restore_guide(location: &Path) -> String {
    let mut text = tr!(
        "restore-guide",
        repository = paths::without_verbatim_prefix(location)
    );
    // Windows' own editors expect CRLF.
    text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    text.push_str("\r\n");
    text
}

/// Writes [`RESTORE_GUIDE`] in the repository folder, unless one is there already that differs
/// from what Replica would write: a file the user has edited is left alone. Returns whether it
/// wrote it.
pub fn write_restore_guide(location: &Path) -> std::io::Result<bool> {
    let path = location.join(RESTORE_GUIDE);
    let text = restore_guide(location);
    match std::fs::read(&path) {
        Ok(existing) if existing == text.as_bytes() => Ok(false),
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&path, text)?;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

/// A local path as restic shows it in a snapshot: `C:\Users\x` is `/C/Users/x`.
pub fn snapshot_path(local: &Path) -> String {
    let text = local.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let b = text.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        format!("/{}{}", &text[..1], &text[2..])
    } else {
        text.to_string()
    }
}

/// Where a path in a snapshot was on disk, the reverse of [`snapshot_path`] for Windows paths:
/// `/C/Users/x` is `C:\Users\x` and `//nas/share/x` is `\\nas\share\x`. Any other path is a
/// path as it is (another OS's).
pub fn local_path(in_snapshot: &str) -> PathBuf {
    let b = in_snapshot.as_bytes();
    let drive = b.len() >= 2
        && b[0] == b'/'
        && b[1].is_ascii_alphabetic()
        && (b.len() == 2 || b[2] == b'/');
    if drive {
        let rest = in_snapshot[2..].replace('/', "\\");
        let rest = if rest.is_empty() {
            "\\".to_string()
        } else {
            rest
        };
        PathBuf::from(format!("{}:{rest}", &in_snapshot[1..2]))
    } else if in_snapshot.starts_with("//") {
        PathBuf::from(in_snapshot.replace('/', "\\"))
    } else {
        PathBuf::from(in_snapshot)
    }
}

/// What to back up.
#[derive(Debug, Clone, Copy)]
pub struct BackupRequest<'a> {
    /// The job's name, put on the snapshot as its tag.
    pub job: &'a str,
    pub sources: &'a [PathBuf],
    pub excludes: &'a ExcludeRules,
}

/// A file restic couldn't read; the snapshot was made without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub item: String,
    pub message: String,
}

/// A finished backup.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupOutcome {
    pub summary: BackupSummary,
    /// Some files couldn't be read: the snapshot was made without them ("partly").
    pub partial: bool,
    pub unreadable: Vec<Unreadable>,
}

/// What to restore, and where.
#[derive(Debug, Clone)]
pub struct RestoreRequest<'a> {
    /// The job whose snapshot it is: `snapshot` is looked for among this job's snapshots only,
    /// so a repository two jobs share never gives one job's data to the other.
    pub job: &'a str,
    /// A snapshot ID (or the start of one, such as its short ID), or `latest`: the job's newest.
    pub snapshot: &'a str,
    /// A file or folder in the snapshot, as `ls` shows it (`/C/Users/…`); `None` restores the
    /// whole snapshot. A file or folder is restored as `<target>/<its name>`.
    pub item: Option<&'a str>,
    /// The folder to restore into: it must not exist yet, or be empty.
    pub target: &'a Path,
    /// The job's sources. The target may not be one of them, or inside or around one.
    pub sources: &'a [PathBuf],
    /// Allows a target inside or around a source, and one with files in it (restoring over
    /// the original, `replica restore --overwrite`). Off by default; the window asks first.
    pub allow_over_source: bool,
}

/// What `forget` did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForgetOutcome {
    pub kept: Vec<Snapshot>,
    pub removed: Vec<Snapshot>,
}

/// A restic to run.
#[derive(Clone)]
pub struct Restic {
    path: PathBuf,
    version: String,
    /// What each restic command runs in, so a cancel stops everything it started.
    tree: Arc<dyn ProcessTree>,
    /// restic's cache for repositories that don't set their own
    /// ([`Repository::with_cache_dir`]): restic's own default if `None`.
    cache_dir: Option<PathBuf>,
}

impl fmt::Debug for Restic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Restic")
            .field("path", &self.path)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl Restic {
    /// The restic at `path`, after asking it its version. A version other than
    /// [`pinned_version`] is logged as a warning.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ResticError> {
        let path = path.into();
        let output = crate::process::command(&path)
            .args(["version", "--json"])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| ResticError::Spawn {
                path: path.clone(),
                detail: e.to_string(),
            })?;
        let info: VersionInfo = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|l| serde_json::from_str(l).ok())
            .ok_or_else(|| ResticError::BadOutput {
                detail: "restic version --json".into(),
            })?;
        let restic = Restic {
            path,
            version: info.version,
            tree: Arc::new(PlainTree),
            cache_dir: None,
        };
        if !restic.is_pinned() {
            tracing::warn!(target: "replica::restic", "{}",
                tr!("warning-restic-version", found = restic.version.as_str(), pinned = pinned_version()));
        }
        Ok(restic)
    }

    /// Runs each restic command in `tree` (on Windows, `replica-win`'s job object), so a cancel
    /// stops restic and every program it started, and nothing it started outlives the command.
    /// Without one, a cancel stops restic alone ([`PlainTree`]).
    pub fn with_tree(mut self, tree: Arc<dyn ProcessTree>) -> Self {
        self.tree = tree;
        self
    }

    /// Keeps restic's cache in `dir` for every repository that doesn't set its own, instead of
    /// restic's default. Tests use it so nothing touches the user's own cache.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(dir.into());
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Whether this is the version Replica bundles and is tested with.
    pub fn is_pinned(&self) -> bool {
        self.version == pinned_version()
    }

    /// Creates a repository at `repo`'s location.
    pub fn init(&self, repo: &Repository) -> Result<(), ResticError> {
        let done = self.run(repo, &["init", "--json"], false, None, &mut |_| {})?;
        self.check_exit(repo, &done)
    }

    /// Whether `repo`'s password opens it: `restic cat config`, which takes no lock and reads
    /// only the repository's config, which is encrypted with its key. A wrong password is
    /// [`ResticError::WrongPassword`]; no repository there is [`ResticError::RepoNotFound`].
    pub fn opens(&self, repo: &Repository) -> Result<(), ResticError> {
        let done = self.run(repo, &["cat", "config", "--json"], false, None, &mut |_| {})?;
        self.check_exit(repo, &done)
    }

    /// Sets up a versioned target, as `replica init` and the window both do: makes its folder,
    /// creates the repository there, and writes [`RESTORE_GUIDE`] in it.
    pub fn init_target(&self, repo: &Repository) -> Result<(), ResticError> {
        std::fs::create_dir_all(&repo.location).map_err(|e| ResticError::Failed {
            code: -1,
            detail: e.to_string(),
        })?;
        self.init(repo)?;
        if let Err(e) = write_restore_guide(Path::new(&repo.location)) {
            tracing::warn!(target: "replica::restic", "couldn't write {RESTORE_GUIDE}: {e}");
        }
        Ok(())
    }

    /// Backs up `request`'s sources as a snapshot tagged with the job's name.
    pub fn backup(
        &self,
        repo: &Repository,
        request: &BackupRequest<'_>,
        on_progress: &mut dyn FnMut(&BackupProgress),
        cancel: &CancelToken,
    ) -> Result<BackupOutcome, ResticError> {
        let mut args: Vec<OsString> = ["backup", "--json", "--tag", request.job]
            .iter()
            .map(OsString::from)
            .collect();
        args.extend(
            request
                .excludes
                .restic_args()
                .into_iter()
                .map(OsString::from),
        );
        args.extend(request.sources.iter().map(|s| s.as_os_str().to_owned()));
        let mut summary = None;
        let mut unreadable = Vec::new();
        let done = process::run(
            self,
            repo,
            &args,
            false,
            Some(cancel),
            &mut |line| match line {
                Line::Status(p) => on_progress(p),
                Line::Summary(v) => summary = serde_json::from_value(v.clone()).ok(),
                Line::Error(e) => unreadable.push(Unreadable {
                    item: e.item.clone(),
                    message: e.error.message.clone(),
                }),
                _ => {}
            },
        )?;
        if done.code != exit::OK && done.code != exit::PARTIAL {
            return Err(self.error(repo, &done));
        }
        let summary = summary.ok_or_else(|| ResticError::BadOutput {
            detail: tr("error-restic-no-summary"),
        })?;
        Ok(BackupOutcome {
            summary,
            partial: done.code == exit::PARTIAL,
            unreadable,
        })
    }

    /// The repository's snapshots, oldest first; only those tagged `job`, if given.
    pub fn snapshots(
        &self,
        repo: &Repository,
        job: Option<&str>,
    ) -> Result<Vec<Snapshot>, ResticError> {
        let mut args = vec!["snapshots", "--json"];
        if let Some(job) = job {
            args.extend(["--tag", job]);
        }
        let done = self.run(repo, &args, true, None, &mut |_| {})?;
        self.check_exit(repo, &done)?;
        parse_json(&done.stdout)
    }

    /// Up to `count` of a snapshot's files (not folders), each equally likely, picked as restic
    /// lists them, so the listing is never held whole. `random(n)` gives a number below `n`.
    pub fn sample_files(
        &self,
        repo: &Repository,
        snapshot: &str,
        count: usize,
        random: &mut dyn FnMut(u64) -> u64,
        cancel: &CancelToken,
    ) -> Result<Vec<Node>, ResticError> {
        let mut picked: Vec<Node> = Vec::with_capacity(count);
        let mut seen = 0u64;
        let args = [OsString::from("ls"), "--json".into(), snapshot.into()];
        let done = process::run(self, repo, &args, false, Some(cancel), &mut |line| {
            if let Line::Node(node) = line
                && node.kind == "file"
            {
                // Reservoir sampling: the n-th file replaces a pick with chance count / n.
                seen += 1;
                if picked.len() < count {
                    picked.push(node.clone());
                } else {
                    let at = random(seen) as usize;
                    if at < count {
                        picked[at] = node.clone();
                    }
                }
            }
        })?;
        self.check_exit(repo, &done)?;
        Ok(picked)
    }

    /// The files and folders in a snapshot, under `path` if given (`/C/Users/…`).
    pub fn ls(
        &self,
        repo: &Repository,
        snapshot: &str,
        path: Option<&str>,
    ) -> Result<Vec<Node>, ResticError> {
        let mut args = vec!["ls", "--json", snapshot];
        args.extend(path);
        let mut nodes = Vec::new();
        let done = self.run(repo, &args, true, None, &mut |_| {})?;
        self.check_exit(repo, &done)?;
        for line in done.stdout.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if value["message_type"] == "node" || value.get("struct_type") == Some(&"node".into()) {
                nodes.push(
                    serde_json::from_value(value).map_err(|e| ResticError::BadOutput {
                        detail: e.to_string(),
                    })?,
                );
            }
        }
        Ok(nodes)
    }

    /// Restores a snapshot, or a file or folder in it, into a new or empty folder. It never
    /// restores over a job's source unless the request allows it.
    pub fn restore(
        &self,
        repo: &Repository,
        request: &RestoreRequest<'_>,
        cancel: &CancelToken,
    ) -> Result<RestoreSummary, ResticError> {
        let target = check_restore_target(request)?;
        let snapshot = self.job_snapshot(repo, request.job, request.snapshot)?;
        self.restore_into(repo, &snapshot, request.item, &target, cancel)
    }

    /// Restores several files or folders of a snapshot into one new or empty folder, each as
    /// `<target>/<its name>` as [`Restic::restore`] does with one (the window's selection). The
    /// folder is checked once, before the first; `request.item` is ignored. Two items with the
    /// same name are refused before anything is restored.
    pub fn restore_many(
        &self,
        repo: &Repository,
        request: &RestoreRequest<'_>,
        items: &[&str],
        cancel: &CancelToken,
    ) -> Result<RestoreSummary, ResticError> {
        let mut names = std::collections::HashSet::new();
        for item in items {
            let item = item.trim_end_matches('/');
            let name = item.rsplit_once('/').map_or(item, |(_, n)| n);
            if !names.insert(name.to_lowercase()) {
                return Err(ResticError::RestoreSameName {
                    name: name.to_string(),
                });
            }
        }
        let target = check_restore_target(request)?;
        let snapshot = self.job_snapshot(repo, request.job, request.snapshot)?;
        let mut total = RestoreSummary::default();
        for item in items {
            let done = self.restore_into(repo, &snapshot, Some(item), &target, cancel)?;
            total.total_files += done.total_files;
            total.files_restored += done.files_restored;
            total.files_skipped += done.files_skipped;
            total.total_bytes += done.total_bytes;
            total.bytes_restored += done.bytes_restored;
        }
        Ok(total)
    }

    /// Restores `item` (or the whole snapshot) of the snapshot with the full ID `snapshot` into
    /// `target`, already checked.
    fn restore_into(
        &self,
        repo: &Repository,
        snapshot: &str,
        item: Option<&str>,
        target: &Path,
        cancel: &CancelToken,
    ) -> Result<RestoreSummary, ResticError> {
        let mut args: Vec<String> = vec!["restore".into(), "--json".into()];
        let mut expected = None;
        match item.map(|i| i.trim_end_matches('/')) {
            Some(item) if !item.is_empty() => {
                // The item must be in the snapshot: what's already at the target proves nothing.
                let found = self.ls(repo, snapshot, Some(item))?;
                if !found.iter().any(|n| n.path.eq_ignore_ascii_case(item)) {
                    return Err(ResticError::RestoreNothing {
                        item: item.to_string(),
                    });
                }
                let (parent, name) = item.rsplit_once('/').unwrap_or(("", item));
                let parent = if parent.is_empty() { "/" } else { parent };
                args.push(format!("{snapshot}:{parent}"));
                // --include takes a pattern: the name must match only itself.
                args.push("--include".into());
                args.push(format!("/{}", crate::exclude::escape_literal(name)));
                expected = Some(target.join(name));
            }
            _ => args.push(snapshot.to_string()),
        }
        args.push("--target".into());
        args.push(paths::without_verbatim_prefix(target));
        let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
        let mut summary = None;
        let done = process::run(self, repo, &args, false, Some(cancel), &mut |line| {
            if let Line::Summary(v) = line {
                summary = serde_json::from_value(v.clone()).ok();
            }
        })?;
        self.check_exit(repo, &done)?;
        if let (Some(item), Some(path)) = (item, expected)
            && std::fs::symlink_metadata(&path).is_err()
        {
            return Err(ResticError::RestoreNothing {
                item: item.to_string(),
            });
        }
        Ok(summary.unwrap_or_default())
    }

    /// The full ID of one of `job`'s snapshots: `latest` (its newest), or the one whose ID starts
    /// with `wanted`. Another job's snapshot never matches.
    pub fn job_snapshot(
        &self,
        repo: &Repository,
        job: &str,
        wanted: &str,
    ) -> Result<String, ResticError> {
        let snapshots = self.snapshots(repo, Some(job))?;
        let not_found = || ResticError::SnapshotNotFound {
            job: job.to_string(),
            snapshot: wanted.to_string(),
        };
        if wanted.eq_ignore_ascii_case("latest") {
            return snapshots
                .iter()
                .max_by_key(|s| s.time)
                .map(|s| s.id.clone())
                .ok_or_else(not_found);
        }
        let wanted_lower = wanted.to_ascii_lowercase();
        let mut matching = snapshots
            .iter()
            .filter(|s| !wanted.is_empty() && s.id.to_ascii_lowercase().starts_with(&wanted_lower));
        match (matching.next(), matching.next()) {
            (Some(one), None) => Ok(one.id.clone()),
            _ => Err(not_found()),
        }
    }

    /// Pins the snapshot `id` ([`PINNED_TAG`]), or unpins it. restic rewrites a snapshot whose
    /// tags change, so it gets a new ID: that's what comes back (the same one if nothing changed,
    /// as when it was pinned already).
    pub fn set_pinned(
        &self,
        repo: &Repository,
        id: &str,
        pinned: bool,
    ) -> Result<String, ResticError> {
        let change = if pinned { "--add" } else { "--remove" };
        let done = self.run(
            repo,
            &["tag", "--json", change, PINNED_TAG, id],
            true,
            None,
            &mut |_| {},
        )?;
        self.check_exit(repo, &done)?;
        let new = done.stdout.lines().find_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            (v["message_type"] == "changed")
                .then(|| v["new_snapshot_id"].as_str().map(str::to_owned))?
        });
        Ok(new.unwrap_or_else(|| id.to_string()))
    }

    /// Forgets the job's snapshots that `retention` doesn't keep. It removes no data:
    /// [`Restic::prune`] does that.
    pub fn forget(
        &self,
        repo: &Repository,
        job: &str,
        retention: &Retention,
    ) -> Result<ForgetOutcome, ResticError> {
        let done = self.run_owned(repo, &forget_args(job, retention), true)?;
        self.check_exit(repo, &done)?;
        let groups: Vec<ForgetGroup> = parse_json(&done.stdout)?;
        let mut outcome = ForgetOutcome::default();
        for g in groups {
            outcome.kept.extend(g.keep.unwrap_or_default());
            outcome.removed.extend(g.remove.unwrap_or_default());
        }
        Ok(outcome)
    }

    /// Removes data no snapshot uses any more.
    pub fn prune(&self, repo: &Repository, cancel: &CancelToken) -> Result<(), ResticError> {
        let done = self.run(repo, &["prune"], false, Some(cancel), &mut |_| {})?;
        self.check_exit(repo, &done)
    }

    /// Checks the repository, reading back `read_data_subset` of its data (`5%`, `1/10`, `500M`)
    /// if given. Problems found are `Ok` with [`CheckSummary::num_errors`] above zero.
    pub fn check(
        &self,
        repo: &Repository,
        read_data_subset: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<CheckSummary, ResticError> {
        let mut args = vec!["check".to_string(), "--json".to_string()];
        if let Some(subset) = read_data_subset {
            args.push(format!("--read-data-subset={subset}"));
        }
        let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
        let mut summary: Option<CheckSummary> = None;
        let done = process::run(self, repo, &args, false, Some(cancel), &mut |line| {
            if let Line::Summary(v) = line {
                summary = serde_json::from_value(v.clone()).ok();
            }
        })?;
        match summary {
            Some(s) if done.code == exit::OK || s.num_errors > 0 => Ok(s),
            _ => {
                self.check_exit(repo, &done)?;
                Ok(CheckSummary::default())
            }
        }
    }

    /// Removes locks that restic judges stale. A lock held by a running restic stays.
    pub fn unlock_stale(&self, repo: &Repository) -> Result<(), ResticError> {
        let done = self.run(repo, &["unlock"], false, None, &mut |_| {})?;
        self.check_exit(repo, &done)
    }

    fn run(
        &self,
        repo: &Repository,
        args: &[&str],
        collect: bool,
        cancel: Option<&CancelToken>,
        on_line: &mut dyn FnMut(&Line),
    ) -> Result<process::Finished, ResticError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        process::run(self, repo, &args, collect, cancel, on_line)
    }

    fn run_owned(
        &self,
        repo: &Repository,
        args: &[String],
        collect: bool,
    ) -> Result<process::Finished, ResticError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        process::run(self, repo, &args, collect, None, &mut |_| {})
    }

    fn check_exit(&self, repo: &Repository, done: &process::Finished) -> Result<(), ResticError> {
        if done.code == exit::OK {
            Ok(())
        } else {
            Err(self.error(repo, done))
        }
    }

    fn error(&self, repo: &Repository, done: &process::Finished) -> ResticError {
        let location = repo.location.clone();
        match done.code {
            exit::NO_REPOSITORY => ResticError::RepoNotFound { location },
            exit::WRONG_PASSWORD => ResticError::WrongPassword { location },
            exit::LOCKED => ResticError::Locked {
                location,
                detail: done.message(),
            },
            code => ResticError::Failed {
                code,
                detail: done.message(),
            },
        }
    }
}

fn forget_args(job: &str, retention: &Retention) -> Vec<String> {
    // A pinned snapshot is always kept.
    let mut args: Vec<String> = [
        "forget",
        "--json",
        "--tag",
        job,
        "--group-by",
        "host,tags",
        "--keep-tag",
        PINNED_TAG,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (flag, n) in [
        ("--keep-daily", retention.daily),
        ("--keep-weekly", retention.weekly),
        ("--keep-monthly", retention.monthly),
    ] {
        if n > 0 {
            args.push(flag.to_string());
            args.push(n.to_string());
        }
    }
    args
}

fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, ResticError> {
    let start = text.find(['[', '{']).unwrap_or(0);
    serde_json::from_str(text[start..].trim()).map_err(|e| ResticError::BadOutput {
        detail: e.to_string(),
    })
}

/// A restore must go to a new or empty folder, and not over a source unless allowed.
///
/// Both checks apply to where the restore really writes ([`paths::real_path`]): `..`, relative paths,
/// links and junctions can't take it into a source. Returns that folder, which is what restic is
/// given. A path that can't be resolved fails the restore.
fn check_restore_target(request: &RestoreRequest<'_>) -> Result<PathBuf, ResticError> {
    let shown = request.target.to_path_buf();
    let real = |path: &Path| {
        paths::real_path(path).map_err(|e| ResticError::RestoreTargetInvalid {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })
    };
    let target = real(request.target)?;
    if request.allow_over_source {
        // Restoring over the original (`--overwrite`): the folder may have files in it.
        return Ok(target);
    }
    if let Ok(mut entries) = std::fs::read_dir(&target)
        && entries.next().is_some()
    {
        return Err(ResticError::RestoreTargetNotEmpty { path: shown });
    }
    if target.exists() && !target.is_dir() {
        return Err(ResticError::RestoreTargetNotEmpty { path: shown });
    }
    if !request.allow_over_source {
        for source in request.sources {
            if paths::overlap(&target, &real(source)?) {
                return Err(ResticError::RestoreOverSource { path: shown });
            }
        }
    }
    Ok(target)
}

/// Removes the locks a killed restic left: those whose PID is `pid` and, where the host name is
/// known, whose host is this one. Only for repositories that are folders; best effort.
pub(crate) fn remove_locks_of(restic: &Restic, repo: &Repository, pid: u32) {
    let locks_dir = Path::new(&repo.location).join("locks");
    if !locks_dir.is_dir() {
        return;
    }
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok();
    let Ok(listed) = restic.run_owned(
        repo,
        &["list".into(), "locks".into(), "--no-lock".into()],
        true,
    ) else {
        return;
    };
    for id in listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let Ok(cat) = restic.run_owned(
            repo,
            &[
                "cat".into(),
                "lock".into(),
                id.into(),
                "--no-lock".into(),
                "--json".into(),
            ],
            true,
        ) else {
            continue;
        };
        let Ok(info) = parse_json::<LockInfo>(&cat.stdout) else {
            continue;
        };
        let same_host = host
            .as_deref()
            .is_none_or(|h| h.eq_ignore_ascii_case(&info.hostname));
        if info.pid == pid && same_host {
            let file = locks_dir.join(id);
            match std::fs::remove_file(&file) {
                Ok(()) => {
                    tracing::info!(target: "replica::restic", "removed the cancelled restic's lock {id}")
                }
                Err(e) => {
                    tracing::warn!(target: "replica::restic", "couldn't remove lock {id}: {e}")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::VolumeRef;

    #[test]
    fn the_pinned_version_comes_from_the_version_file() {
        assert_eq!(pinned_version(), "0.19.1");
    }

    #[test]
    fn the_search_order_never_includes_path() {
        let dir = tempfile::tempdir().unwrap();
        let (exe_dir, dev_dir) = (dir.path().join("exe"), dir.path().join("dev"));
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&dev_dir).unwrap();
        let mut search = ResticSearch {
            override_path: None,
            exe_dir: Some(exe_dir.clone()),
            dev_dir: Some(dev_dir.clone()),
        };
        assert_eq!(
            search.find(),
            Err(ResticError::NotFound {
                path: exe_dir.join(EXE_NAME)
            })
        );
        std::fs::write(dev_dir.join(EXE_NAME), "").unwrap();
        assert_eq!(search.find(), Ok(dev_dir.join(EXE_NAME)));
        std::fs::write(exe_dir.join(EXE_NAME), "").unwrap();
        assert_eq!(
            search.find(),
            Ok(exe_dir.join(EXE_NAME)),
            "the bundled copy comes first"
        );
        let custom = dir.path().join("custom.exe");
        search.override_path = Some(custom.clone());
        assert_eq!(
            search.find(),
            Err(ResticError::NotFound {
                path: custom.clone()
            }),
            "a missing override is an error, not a fallback"
        );
        std::fs::write(&custom, "").unwrap();
        assert_eq!(search.find(), Ok(custom));
    }

    #[test]
    fn password_names_follow_the_target_not_its_letter() {
        let drive = |id: Option<&str>, label: Option<&str>, path: &str| Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: id.map(Into::into),
                label: label.map(Into::into),
            },
            path: path.into(),
        };
        assert_eq!(
            password_secret_name(&drive(Some(r"\\?\Volume{AB}\"), Some("X"), "Replica/Hive")),
            r"replica:repo:drive:id=\\?\volume{ab}\:replica\hive"
        );
        assert_eq!(
            password_secret_name(&drive(None, Some("BACKUP"), r"Replica\Hive")),
            r"replica:repo:drive:label=backup:replica\hive"
        );
        let unc = Target::Unc {
            name: None,
            path: r"\\NAS\Backup\Hive".into(),
            credential: None,
        };
        assert_eq!(
            password_secret_name(&unc),
            r"replica:repo:unc:\\nas\backup\hive"
        );
    }

    #[test]
    fn repository_debug_hides_the_password() {
        let repo = Repository::new("D:/repo", Secret::new("hunter2"));
        assert!(!format!("{repo:?}").contains("hunter2"));
    }

    /// Round 1's finding: `replica-pinned` was a valid job name, so that job's every snapshot
    /// looked pinned and unpinning stripped its job tag. No job can be named after the pin's tag.
    #[test]
    fn no_job_can_be_named_like_the_pin() {
        assert!(!crate::job::is_valid_name(PINNED_TAG));
        let snapshot = |tags: &[&str]| Snapshot {
            id: "x".into(),
            short_id: "x".into(),
            time: chrono::Utc::now().fixed_offset(),
            paths: Vec::new(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            hostname: String::new(),
            summary: None,
        };
        assert!(
            !snapshot(&["replica-pinned"]).is_pinned(),
            "a job's own tag"
        );
        assert!(!snapshot(&["replica"]).is_pinned());
        assert!(snapshot(&["hive", PINNED_TAG]).is_pinned());
    }

    #[test]
    fn forget_arguments() {
        let r = Retention {
            daily: 7,
            weekly: 0,
            monthly: 6,
            keep_forever: false,
        };
        assert_eq!(
            forget_args("hive", &r),
            [
                "forget",
                "--json",
                "--tag",
                "hive",
                "--group-by",
                "host,tags",
                "--keep-tag",
                "replica:pinned",
                "--keep-daily",
                "7",
                "--keep-monthly",
                "6"
            ]
        );
    }

    #[test]
    fn restore_targets_must_be_new_or_empty_and_away_from_sources() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(source.join("sub")).unwrap();
        let sources = vec![source.clone()];
        let check = |target: &Path, allow| {
            check_restore_target(&RestoreRequest {
                job: "job",
                snapshot: "latest",
                item: None,
                target,
                sources: &sources,
                allow_over_source: allow,
            })
            .map(|_| ())
        };

        assert_eq!(check(&dir.path().join("new"), false), Ok(()));
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(check(&empty, false), Ok(()));
        assert!(matches!(
            check(dir.path(), false),
            Err(ResticError::RestoreTargetNotEmpty { .. })
        ));
        // The source itself, inside it, or around it.
        let inside = source.join("sub").join("restored");
        assert_eq!(
            check(&inside, false),
            Err(ResticError::RestoreOverSource {
                path: inside.clone()
            })
        );
        let upper = PathBuf::from(source.to_string_lossy().to_uppercase()).join("x");
        assert!(matches!(
            check(&upper, false),
            Err(ResticError::RestoreOverSource { .. })
        ));
        assert_eq!(
            check(&inside, true),
            Ok(()),
            "allowed when the request says so"
        );
    }

    /// The guard applies to where the restore really writes, not to how the path is spelled.
    #[test]
    fn restore_targets_cant_reach_a_source_by_an_alias() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        let sources = vec![source.clone()];
        let check = |target: &Path| {
            check_restore_target(&RestoreRequest {
                job: "job",
                snapshot: "latest",
                item: None,
                target,
                sources: &sources,
                allow_over_source: false,
            })
        };
        let over = |r: Result<PathBuf, ResticError>| {
            matches!(r, Err(ResticError::RestoreOverSource { .. }))
        };

        // `..` through a folder that doesn't exist, and through one that does.
        assert!(over(check(
            &dir.path()
                .join("alias")
                .join("..")
                .join("src")
                .join("restored")
        )));
        std::fs::create_dir(dir.path().join("real")).unwrap();
        assert!(over(check(&dir.path().join("real").join("..").join("src"))));
        assert!(over(check(
            &dir.path().join(".").join("src").join(".").join("x")
        )));

        // A link or junction to the source.
        let link = dir.path().join("link");
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&source)
            .output()
            .is_ok_and(|o| o.status.success());
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&source, &link).is_ok();
        assert!(made, "couldn't make a junction or link for the test");
        assert!(over(check(&link.join("restored"))));
        assert!(over(check(&link)));

        // A real new folder beside the source is fine, and is what restic is given.
        let fine = check(&dir.path().join("elsewhere").join("..").join("restore-here")).unwrap();
        assert_eq!(fine.file_name().unwrap(), "restore-here");
        assert!(fine.is_absolute() && !fine.to_string_lossy().contains(".."));
    }

    #[test]
    fn the_restore_guide_names_the_repository_and_no_secret() {
        let guide = restore_guide(Path::new(r"\\?\E:\Replica\hive"));
        assert!(
            guide.contains(r#"restic -r "E:\Replica\hive" snapshots"#),
            "{guide}"
        );
        assert!(guide.contains("restore latest --target"), "{guide}");
        assert!(guide.contains("\r\n"), "Windows line ends");
        assert!(
            !guide.to_lowercase().contains("password:"),
            "no password in it"
        );
    }

    #[test]
    fn the_restore_guide_is_written_once_and_never_over_an_edited_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_restore_guide(dir.path()).unwrap(), "written");
        assert!(
            !write_restore_guide(dir.path()).unwrap(),
            "the same text isn't rewritten"
        );
        let path = dir.path().join(RESTORE_GUIDE);
        std::fs::write(&path, "my own notes").unwrap();
        assert!(!write_restore_guide(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "my own notes",
            "an edited file is kept"
        );
    }

    #[test]
    fn snapshot_paths() {
        assert_eq!(
            snapshot_path(Path::new(r"C:\Users\x\a.txt")),
            "/C/Users/x/a.txt"
        );
        assert_eq!(snapshot_path(Path::new(r"\\?\D:\w")), "/D/w");
        assert_eq!(snapshot_path(Path::new("/home/x")), "/home/x");
    }

    #[test]
    fn local_paths_reverse_snapshot_paths() {
        for local in [r"C:\Users\x\a.txt", r"D:\", r"\\nas\share\x"] {
            assert_eq!(
                local_path(&snapshot_path(Path::new(local))),
                PathBuf::from(local),
                "{local}"
            );
        }
        assert_eq!(local_path("/C"), PathBuf::from(r"C:\"));
        // Not a drive letter on its own: another OS's path, as it is.
        assert_eq!(local_path("/home/x"), PathBuf::from("/home/x"));
        assert_eq!(local_path("/Cx/y"), PathBuf::from("/Cx/y"));
    }

    #[test]
    fn errors_read_well() {
        let e = ResticError::RepoNotFound {
            location: "D:/repo".into(),
        };
        assert!(e.to_string().contains("D:/repo"), "{e}");
        let f = ResticError::Failed {
            code: 1,
            detail: "boom".into(),
        };
        assert!(
            f.to_string().contains("boom") && f.to_string().contains("exit code 1"),
            "{f}"
        );
    }
}
