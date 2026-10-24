//! The per-repository lock: jobs that share a restic repository take turns.
//!
//! A job's run lock ([`crate::run::JobLock`]) keeps two runs of one job apart, but two
//! *different* jobs can use the same repository (several jobs backing up to one drive is a real
//! setup). Run together, restic's own lock fails one of them ("repository locked") as soon as one
//! reaches `forget`/`prune` while the other backs up, and on fixed schedules the same job can lose
//! every time. So a run also takes a lock for each repository it uses:
//!
//! - **Keyed by the repository's identity** ([`lock_key`]): the volume ID or label and
//!   the path, or the UNC path, with the path read as Windows opens it
//!   ([`crate::paths::typed_components`]: `./m`, `m\` and `M.` are `m`). Two jobs on one
//!   repository share it, whatever their names or the drive's letter today.
//! - **Every Replica process honours it:** [`RepositoryLocks`] is the platform's. On Windows it's
//!   a named mutex in `Global\`, named by the user's SID (`replica_win::repo_locks`), so the
//!   CLI, the tray, a Task Scheduler run and another logon session all see it, and a holder
//!   that dies lets it go. [`FileLocks`] is a portable one (an OS file lock), for tests and
//!   other platforms.
//! - **Held for the whole run**, before steps to verify, so backup, `forget`, `prune` and `check`
//!   never meet another Replica run on that repository. A job's keys are taken in sorted order,
//!   all before it starts, so two jobs that share two repositories can't each hold one and wait
//!   for the other.
//! - **A busy repository is waited for, not failed** ([`acquire`]): up to a bound (the app's
//!   `repository_wait_minutes`, 2 hours by default), checking for a cancel all the while, with a
//!   [`Wait::Turn`] naming the job that holds it. Each holder writes its job's name beside the
//!   lock (`repo-<digest>.holder` in the locks folder) for that.
//! - **A queue never stalls behind one** (Run all, the tray's queue): a queue starts the first
//!   job whose repositories are free ([`is_free`]), and a run that's waiting gives way
//!   ([`AcquireError::GaveWay`]) as soon as a job behind it could run, to come back to later.
//!
//! Only versioned targets are locked: mirrors that overlap, or that overlap a repository, are
//! refused by validation ([`crate::job::validate_jobs`]), so no two jobs share a mirror's folder.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cancel::CancelToken;
use crate::i18n::tr;
use crate::job::{Job, Mode, Target};
use crate::paths::typed_components;
use crate::platform::PlatformError;
use crate::restic::{Repository, Restic, ResticError};
use crate::tr;

/// How long a run waits for a repository another run holds, unless the settings say otherwise.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(2 * 60 * 60);

/// How often a waiting run tries the lock again.
const POLL: Duration = Duration::from_millis(250);

/// How long a quick listing (`snapshots`, `ls`) retries restic's own lock ([`retry_locked`]).
pub const LISTING_WAIT: Duration = Duration::from_secs(30);
/// The longest pause between two tries of restic's lock.
const RETRY_MAX_PAUSE: Duration = Duration::from_secs(5);

/// The platform's per-repository locks: every Replica process of this user sees the same lock
/// for the same key.
///
/// # For implementers
///
/// - A lock held by a process that dies is let go (or taken over by the next to try).
/// - [`RepositoryLocks::try_lock`] never waits: [`acquire`] does the waiting, so it can see a
///   cancel and report who it's waiting for.
/// - The [`HeldLock`] may be tied to the thread that took it (a Windows mutex is): a run takes
///   and drops its locks on its own thread.
pub trait RepositoryLocks: Send + Sync {
    /// Takes the lock for `key` if it's free: `Ok(None)` if another run holds it.
    fn try_lock(&self, key: &str) -> Result<Option<Box<dyn HeldLock>>, PlatformError>;
}

/// A lock taken by [`RepositoryLocks::try_lock`]: let go when dropped.
pub trait HeldLock {}

/// The lock key of a versioned target's repository: its identity (as in
/// [`crate::restic::password_secret_name`]: the volume ID, else its label, and the path; or the
/// UNC path), with the path's components as Windows opens them, so aliases of one folder share
/// a key.
pub fn lock_key(target: &Target) -> String {
    match target {
        Target::Drive { volume, path, .. } => {
            let vol = volume
                .volume_id
                .as_deref()
                .map(|id| format!("id={}", id.trim()))
                .or_else(|| {
                    volume
                        .label
                        .as_deref()
                        .map(|l| format!("label={}", l.trim()))
                })
                .unwrap_or_default();
            let path = typed_components(&path.to_string_lossy(), 0).join("\\");
            format!("replica:repo:drive:{}:{path}", vol.to_lowercase())
        }
        Target::Unc { path, .. } => format!(
            "replica:repo:unc:\\\\{}",
            typed_components(path, 2).join("\\")
        ),
    }
}

/// A short, stable name for a key, safe in a file or mutex name: FNV-1a, 64 bits, in hex. Two
/// keys that happened to share one would only take turns when they needn't.
pub fn key_digest(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What a run is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// Another Replica run holds the repository of `target`: the job `job`, if its name could be
    /// read.
    Turn { target: String, job: Option<String> },
    /// restic reports `target`'s repository locked by a restic outside Replica (or a lock left
    /// behind): retried with backoff for a while ([`crate::run::Waits::restic_lock`]).
    ResticLock { target: String },
}

impl Wait {
    pub fn target(&self) -> &str {
        match self {
            Wait::Turn { target, .. } | Wait::ResticLock { target } => target,
        }
    }

    /// `turn` or `restic-lock`: the name in `--json` (never translated).
    pub fn kind(&self) -> &'static str {
        match self {
            Wait::Turn { .. } => "turn",
            Wait::ResticLock { .. } => "restic-lock",
        }
    }

    /// The job it waits for, if it's another Replica run's turn and its name is known.
    pub fn job(&self) -> Option<&str> {
        match self {
            Wait::Turn { job, .. } => job.as_deref(),
            Wait::ResticLock { .. } => None,
        }
    }

    /// "Waiting for hive to finish with External HDD", for the tray, the window and the CLI.
    pub fn text(&self) -> String {
        let target = self.target();
        match self {
            Wait::Turn { job: Some(job), .. } => {
                tr!("wait-turn", job = job.as_str(), target = target)
            }
            Wait::Turn { job: None, .. } => tr!("wait-turn-unknown", target = target),
            Wait::ResticLock { .. } => tr!("wait-restic-lock", target = target),
        }
    }
}

/// A wait that's over, and how long it lasted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waited {
    pub wait: Wait,
    pub duration: Duration,
    /// How it ended: `ok` (it went on), or `timed-out`, `cancelled` or `gave-way` (the names in
    /// `--json`, never translated).
    pub ended: &'static str,
}

/// Why [`acquire`] didn't get every lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireError {
    /// It waited as long as it may.
    TimedOut {
        wait: Wait,
        waited: Duration,
    },
    Cancelled,
    /// A job queued behind it could run now, so it let go and stepped aside, to run later
    /// ([`acquire`]'s `give_way`). Nothing was done.
    GaveWay,
    Platform(PlatformError),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            AcquireError::TimedOut { wait, waited } => {
                let minutes = waited.as_secs().div_ceil(60);
                match wait.job() {
                    Some(job) => tr!(
                        "error-wait-timed-out",
                        job = job,
                        target = wait.target(),
                        minutes = minutes
                    ),
                    None => tr!(
                        "error-wait-timed-out-unknown",
                        target = wait.target(),
                        minutes = minutes
                    ),
                }
            }
            AcquireError::Cancelled => tr("error-cancelled"),
            AcquireError::GaveWay => tr("wait-gave-way"),
            AcquireError::Platform(e) => tr!("error-repo-lock", detail = e.to_string()),
        })
    }
}

/// The repository locks a run holds. Dropping it lets them all go.
pub struct Held {
    locks: Vec<HeldRepository>,
}

impl Held {
    /// How many repositories it holds.
    pub fn len(&self) -> usize {
        self.locks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

struct HeldRepository {
    holder_file: PathBuf,
    _lock: Box<dyn HeldLock>,
}

impl Drop for HeldRepository {
    fn drop(&mut self) {
        // Before the lock goes (fields drop after this), so it never removes the next holder's.
        let _ = fs::remove_file(&self.holder_file);
    }
}

/// The repositories `job` uses, as (key, the first target's name), sorted by key, each once.
pub fn repositories(job: &Job) -> Vec<(String, String)> {
    if job.mode != Mode::Versioned {
        return Vec::new();
    }
    let mut keys: Vec<(String, String)> = job
        .targets
        .iter()
        .map(|t| (lock_key(t), t.display_name()))
        .collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    keys.dedup_by(|a, b| a.0 == b.0);
    keys
}

/// Whether every repository `job` uses is free now: no other run (in this process on another
/// thread, or in any other process) holds one. A queue starts such a job first. A lock that
/// can't be checked counts as free, so the run itself meets the problem and reports it.
pub fn is_free(job: &Job, locks: &dyn RepositoryLocks) -> bool {
    // Taken and let go at once, one at a time.
    repositories(job)
        .iter()
        .all(|(key, _)| !matches!(locks.try_lock(key), Ok(None)))
}

/// Which job of a queue to start next: the first whose repositories are all free ([`is_free`]),
/// else the first, which then waits its turn. `None` for an empty queue.
pub fn pick_next<'j>(
    queue: impl IntoIterator<Item = &'j Job>,
    locks: &dyn RepositoryLocks,
) -> Option<usize> {
    let mut any = false;
    for (i, job) in queue.into_iter().enumerate() {
        any = true;
        if is_free(job, locks) {
            return Some(i);
        }
    }
    any.then_some(0)
}

/// How long a run may wait its turn, and when it should step aside for the jobs queued behind it.
#[derive(Clone, Copy)]
pub struct Turn<'a> {
    /// The longest it waits in all.
    pub bound: Duration,
    /// Asked while it waits: whether a job queued behind it could run now.
    pub give_way: &'a dyn Fn() -> bool,
}

impl Turn<'_> {
    /// Up to `bound`, with no queue behind it.
    pub fn alone(bound: Duration) -> Turn<'static> {
        Turn {
            bound,
            give_way: &|| false,
        }
    }
}

/// Takes the lock of every repository `job` uses (see the module docs), waiting up to
/// `turn.bound` in all for ones other runs hold.
///
/// - `on_wait` is told `Some` when a wait starts (and again if the job it waits for changes),
///   and `None` once it's over, whatever ended it.
/// - Each wait, once over, is added to `waits`: the ones that let it go on, and the one that
///   ended it (timed out, cancelled, or gave way).
/// - `turn.give_way` is asked while it waits: once it says a job queued behind this one could run
///   now, the locks taken so far are let go and it's [`AcquireError::GaveWay`].
pub fn acquire(
    job: &Job,
    locks: &dyn RepositoryLocks,
    locks_dir: &Path,
    turn: Turn<'_>,
    cancel: &CancelToken,
    waits: &mut Vec<Waited>,
    on_wait: &mut dyn FnMut(Option<&Wait>),
) -> Result<Held, AcquireError> {
    let request = Request {
        keys: repositories(job),
        holder: &job.name,
    };
    take(request, locks, locks_dir, turn, cancel, waits, on_wait)
}

/// Takes the lock of one versioned target's repository for restic work outside a run:
/// a verify, a restore or test restore, setting a repository up. It waits its turn as a run
/// does, up to `turn.bound`, with `on_wait` told as for [`acquire`]. `holder` is the name other
/// runs see while it holds it (the job's, where there is one).
pub fn acquire_target(
    target: &Target,
    holder: &str,
    locks: &dyn RepositoryLocks,
    locks_dir: &Path,
    turn: Turn<'_>,
    cancel: &CancelToken,
    on_wait: &mut dyn FnMut(Option<&Wait>),
) -> Result<Held, AcquireError> {
    let request = Request {
        keys: vec![(lock_key(target), target.display_name())],
        holder,
    };
    take(
        request,
        locks,
        locks_dir,
        turn,
        cancel,
        &mut Vec::new(),
        on_wait,
    )
}

/// The repositories to take, as (key, the target's name) in the order to take them, and the
/// name their holder writes.
struct Request<'a> {
    keys: Vec<(String, String)>,
    holder: &'a str,
}

fn take(
    request: Request<'_>,
    locks: &dyn RepositoryLocks,
    locks_dir: &Path,
    turn: Turn<'_>,
    cancel: &CancelToken,
    waits: &mut Vec<Waited>,
    on_wait: &mut dyn FnMut(Option<&Wait>),
) -> Result<Held, AcquireError> {
    let start = Instant::now();
    let mut held = Held { locks: Vec::new() };
    for (key, target) in request.keys {
        let holder_file = locks_dir.join(format!("repo-{}.holder", key_digest(&key)));
        let mut shown: Option<Wait> = None;
        let began = Instant::now();
        let mut end = |shown: Option<Wait>, ended, on_wait: &mut dyn FnMut(Option<&Wait>)| {
            if let Some(wait) = shown {
                on_wait(None);
                waits.push(Waited {
                    wait,
                    duration: began.elapsed(),
                    ended,
                });
            }
        };
        loop {
            match locks.try_lock(&key) {
                Ok(Some(lock)) => {
                    write_holder(locks_dir, &holder_file, request.holder);
                    held.locks.push(HeldRepository {
                        holder_file,
                        _lock: lock,
                    });
                    end(shown, "ok", on_wait);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    end(shown, "failed", on_wait);
                    return Err(AcquireError::Platform(e));
                }
            }
            let wait = Wait::Turn {
                target: target.clone(),
                job: read_holder(&holder_file),
            };
            if shown.as_ref() != Some(&wait) {
                on_wait(Some(&wait));
                shown = Some(wait);
            }
            let left = turn.bound.saturating_sub(start.elapsed());
            if left.is_zero() {
                let wait = shown.clone().expect("set above");
                end(shown, "timed-out", on_wait);
                return Err(AcquireError::TimedOut {
                    wait,
                    waited: start.elapsed(),
                });
            }
            if (turn.give_way)() {
                end(shown, "gave-way", on_wait);
                return Err(AcquireError::GaveWay);
            }
            if cancel.wait_timeout(POLL.min(left)) {
                end(shown, "cancelled", on_wait);
                return Err(AcquireError::Cancelled);
            }
        }
    }
    Ok(held)
}

/// restic's own lock as the backstop for a quick look at a repository outside a run
/// (`snapshots`, `ls`), which doesn't take the repository's lock: a `call` that finds the
/// repository locked (a run pruning it, say) is retried, once at once after `restic unlock`
/// (stale locks only), then with backoff, for up to `bound` in all ([`LISTING_WAIT`]). Then
/// the last error comes back as it is. A cancel ends it with [`ResticError::Cancelled`].
pub fn retry_locked<T>(
    restic: &Restic,
    repo: &Repository,
    bound: Duration,
    cancel: &CancelToken,
    mut call: impl FnMut() -> Result<T, ResticError>,
) -> Result<T, ResticError> {
    let start = Instant::now();
    let mut unlocked = false;
    let mut pause = Duration::from_millis(500);
    loop {
        match call() {
            Err(locked @ ResticError::Locked { .. }) => {
                if !unlocked {
                    unlocked = true;
                    let _ = restic.unlock_stale(repo);
                    continue;
                }
                let left = bound.saturating_sub(start.elapsed());
                if left.is_zero() {
                    return Err(locked);
                }
                if cancel.wait_timeout(pause.min(left)) {
                    return Err(ResticError::Cancelled);
                }
                pause = (pause * 2).min(RETRY_MAX_PAUSE);
            }
            other => return other,
        }
    }
}

fn write_holder(locks_dir: &Path, holder_file: &Path, job: &str) {
    if let Err(e) = fs::create_dir_all(locks_dir).and_then(|()| fs::write(holder_file, job)) {
        // Only the waiting message needs it: it says "another job" without it.
        tracing::warn!("couldn't write {}: {e}", holder_file.display());
    }
}

fn read_holder(holder_file: &Path) -> Option<String> {
    fs::read_to_string(holder_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Repository locks as OS file locks in a folder (`repo-<digest>.lock`): across processes, let
/// go when the process ends. Portable; Windows uses `replica_win::repo_locks` instead, which
/// doesn't depend on every process using the same folder.
#[derive(Debug, Clone)]
pub struct FileLocks {
    dir: PathBuf,
}

impl FileLocks {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileLocks { dir: dir.into() }
    }
}

struct FileLock(#[allow(dead_code)] File);

impl HeldLock for FileLock {}

impl RepositoryLocks for FileLocks {
    fn try_lock(&self, key: &str) -> Result<Option<Box<dyn HeldLock>>, PlatformError> {
        let platform = |e: std::io::Error| PlatformError(e.to_string());
        fs::create_dir_all(&self.dir).map_err(platform)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(format!("repo-{}.lock", key_digest(key))))
            .map_err(platform)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Box::new(FileLock(file)))),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(platform(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Source, VolumeRef};

    fn drive(label: &str, path: &str) -> Target {
        Target::Drive {
            name: Some(format!("{label} {path}")),
            volume: VolumeRef {
                volume_id: None,
                label: Some(label.into()),
            },
            path: path.into(),
        }
    }

    fn job(name: &str, mode: Mode, targets: Vec<Target>) -> Job {
        Job {
            name: name.into(),
            mode,
            sources: vec![Source {
                path: "C:/src".into(),
                exclude: Vec::new(),
            }],
            targets,
            ..Job::default()
        }
    }

    /// `acquire` with no queue behind it, its waits thrown away.
    fn take(
        job: &Job,
        locks: &FileLocks,
        dir: &Path,
        bound: Duration,
        cancel: &CancelToken,
        on_wait: &mut dyn FnMut(Option<&Wait>),
    ) -> Result<Held, AcquireError> {
        acquire(
            job,
            locks,
            dir,
            Turn::alone(bound),
            cancel,
            &mut Vec::new(),
            on_wait,
        )
    }

    #[test]
    fn the_key_is_the_repository_not_the_job_or_the_letter() {
        let key = |p| lock_key(&drive("BACKUP", p));
        assert_eq!(
            key(r"Replica\Hive"),
            lock_key(&drive("backup", "replica/hive")),
            "case and separators don't matter"
        );
        for alias in [r".\Replica\Hive\", "replica/./hive.", "x/../replica/hive"] {
            assert_eq!(key(alias), key(r"Replica\Hive"), "{alias}");
        }
        assert_ne!(key("a"), key("b"));
        let unc = |p: &str| {
            lock_key(&Target::Unc {
                name: None,
                path: p.into(),
                credential: None,
            })
        };
        assert_eq!(unc(r"\\NAS\share\.\repo\"), unc(r"\\nas\share\repo"));
        assert_ne!(unc(r"\\nas\share\a"), key("a"), "a share is never a drive");
        assert_eq!(key_digest("x"), key_digest("x"));
        assert_ne!(key_digest("x"), key_digest("y"));
        assert_eq!(key_digest("x").len(), 16);
    }

    #[test]
    fn a_job_takes_each_repository_once_in_a_fixed_order_and_mirrors_none() {
        let a = drive("BACKUP", "a");
        let b = drive("BACKUP", "b");
        let one = job(
            "one",
            Mode::Versioned,
            vec![b.clone(), a.clone(), b.clone()],
        );
        let two = job("two", Mode::Versioned, vec![a.clone(), b.clone()]);
        let keys = |j: &Job| -> Vec<String> { repositories(j).into_iter().map(|k| k.0).collect() };
        assert_eq!(
            keys(&one),
            keys(&two),
            "the same order whatever the targets' order"
        );
        assert_eq!(keys(&one).len(), 2);
        assert!(repositories(&job("m", Mode::Mirror, vec![a])).is_empty());
    }

    #[test]
    fn a_held_repository_is_waited_for_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let locks = FileLocks::new(dir.path());
        let target = drive("BACKUP", "a");
        let first = job("first", Mode::Versioned, vec![target.clone()]);
        let second = job("second", Mode::Versioned, vec![target]);
        let cancel = CancelToken::new();
        let held = take(
            &first,
            &locks,
            dir.path(),
            DEFAULT_WAIT,
            &cancel,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(held.len(), 1);
        assert!(!is_free(&second, &locks), "held");

        // The second waits, names the first, and gets it once the first lets go: the first is
        // dropped from the wait's own callback, so nothing here depends on timing.
        let mut first_held = Some(held);
        let mut seen = Vec::new();
        let mut waits = Vec::new();
        let got = acquire(
            &second,
            &locks,
            dir.path(),
            Turn {
                bound: DEFAULT_WAIT,
                give_way: &|| false,
            },
            &cancel,
            &mut waits,
            &mut |w| {
                seen.push(w.cloned());
                first_held = None;
            },
        )
        .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            seen,
            [
                Some(Wait::Turn {
                    target: "BACKUP a".into(),
                    job: Some("first".into())
                }),
                None
            ]
        );
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0].wait.job(), Some("first"));
        assert_eq!(waits[0].ended, "ok");
        assert!(
            waits[0].wait.text().contains("first"),
            "{}",
            waits[0].wait.text()
        );
    }

    #[test]
    fn a_wait_ends_at_its_bound_a_cancel_or_a_job_behind_it_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let locks = FileLocks::new(dir.path());
        let target = drive("BACKUP", "a");
        let first = job("first", Mode::Versioned, vec![target.clone()]);
        let second = job("second", Mode::Versioned, vec![target]);
        let cancel = CancelToken::new();
        let _held = take(
            &first,
            &locks,
            dir.path(),
            DEFAULT_WAIT,
            &cancel,
            &mut |_| {},
        )
        .unwrap();

        let mut waits = Vec::new();
        let mut ends = Vec::new();
        let timed_out = acquire(
            &second,
            &locks,
            dir.path(),
            Turn {
                bound: Duration::ZERO,
                give_way: &|| false,
            },
            &cancel,
            &mut waits,
            &mut |w| ends.push(w.is_some()),
        );
        let Err(AcquireError::TimedOut { wait, .. }) = &timed_out else {
            panic!("{:?}", timed_out.map(|h| h.len()));
        };
        assert_eq!(wait.job(), Some("first"));
        let text = timed_out.err().unwrap().to_string();
        assert!(
            text.contains("first") && text.contains("BACKUP a"),
            "{text}"
        );
        assert_eq!(ends, [true, false], "the wait is said to be over");

        let gave_way = acquire(
            &second,
            &locks,
            dir.path(),
            Turn {
                bound: DEFAULT_WAIT,
                give_way: &|| true,
            },
            &cancel,
            &mut waits,
            &mut |_| {},
        );
        assert!(matches!(gave_way, Err(AcquireError::GaveWay)));

        let stopper = cancel.clone();
        let cancelled = acquire(
            &second,
            &locks,
            dir.path(),
            Turn {
                bound: DEFAULT_WAIT,
                give_way: &|| false,
            },
            &cancel,
            &mut waits,
            &mut |w| {
                if w.is_some() {
                    stopper.cancel();
                }
            },
        );
        assert!(matches!(cancelled, Err(AcquireError::Cancelled)));
        let ended: Vec<&str> = waits.iter().map(|w| w.ended).collect();
        assert_eq!(ended, ["timed-out", "gave-way", "cancelled"], "{waits:?}");
    }

    #[test]
    fn another_repository_is_never_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let locks = FileLocks::new(dir.path());
        let cancel = CancelToken::new();
        let a = job("a", Mode::Versioned, vec![drive("BACKUP", "a")]);
        let b = job("b", Mode::Versioned, vec![drive("BACKUP", "b")]);
        let _a = take(&a, &locks, dir.path(), DEFAULT_WAIT, &cancel, &mut |_| {}).unwrap();
        assert!(is_free(&b, &locks));
        assert!(is_free(
            &job("m", Mode::Mirror, vec![drive("BACKUP", "a")]),
            &locks
        ));
        let a2 = job("a2", Mode::Versioned, vec![drive("BACKUP", "a")]);
        assert_eq!(pick_next([&a2, &b], &locks), Some(1), "the free one first");
        assert_eq!(pick_next([&a2, &a2], &locks), Some(0), "else the first");
        assert_eq!(pick_next([], &locks), None);
        take(&b, &locks, dir.path(), Duration::ZERO, &cancel, &mut |_| {
            panic!("b's repository is free")
        })
        .unwrap();
    }

    #[test]
    fn letting_go_removes_the_holder_name() {
        let dir = tempfile::tempdir().unwrap();
        let locks = FileLocks::new(dir.path());
        let cancel = CancelToken::new();
        let a = job("a", Mode::Versioned, vec![drive("BACKUP", "a")]);
        let held = take(&a, &locks, dir.path(), DEFAULT_WAIT, &cancel, &mut |_| {}).unwrap();
        let holder = dir.path().join(format!(
            "repo-{}.holder",
            key_digest(&lock_key(&a.targets[0]))
        ));
        assert_eq!(fs::read_to_string(&holder).unwrap(), "a");
        drop(held);
        assert!(!holder.exists());
    }
}
