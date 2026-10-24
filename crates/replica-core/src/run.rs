//! The run orchestrator: one run of a job, start to finish.
//!
//! # For the CLI (`replica run`) and the tray
//!
//! ```text
//! let history = History::open(&data_dir.join("history.db"))?;
//! let restic = ResticSearch::for_this_exe(settings.app.restic_path.clone()).find()
//!     .and_then(Restic::new)            // an Err here only fails versioned targets
//!     .map(|r| r.with_tree(Arc::new(replica_win::jobs::JobObjectTree)));
//! let ctx = RunContext { restic: restic.as_ref().ok(), resolver, secrets,
//!                        logs_dir, locks_dir, staging_dir,
//!                        processes: &replica_win::jobs::JobObjectTree, cancel: &cancel };
//! let report = run_job(&job, &ctx, &mut history, &mut |event| { /* progress */ });
//! std::process::exit(report.outcome.exit_code());
//! ```
//!
//! 1. **The lock:** [`JobLock`] in `locks_dir` (one file per job, held with the OS's file lock,
//!    so it works across processes and is let go if a process dies). If another run of the job
//!    holds it, the run is [`Outcome::Skipped`] at once, with nothing done or recorded.
//!    Then **the repositories' locks** ([`crate::repo_lock`]): a job whose repository
//!    another run (of another job, in any Replica process) is using waits its turn, with a
//!    [`RunEvent::Waiting`], up to [`Waits::turn`]; past that the run fails, and is recorded and
//!    notified as failures are. A cancel stops the wait.
//! 2. **The log:** one file per run in `logs_dir`, `<job>-<UTC time to the ms>-<pid>[-n].log`,
//!    created new. It never holds a secret; restic's password only ever goes in its environment.
//! 3. **Before steps**, in order, after [`crate::snapshot::plan`] has resolved and checked every
//!    path (a problem fails the run before anything runs): `sqlite-snapshot` copies a live
//!    database consistently into Replica's own staging folder (`staging_dir`), and the live
//!    file and its `-wal`, `-shm` and `-journal` are left out of the backup while the copy is
//!    backed up; `command` runs in a [`ProcessTree`] (a Windows job object) with its timeout,
//!    and its whole tree is stopped at it. **A failed before step fails the run**: no engine, no
//!    after steps.
//! 4. **Each target in turn:** resolved through [`TargetResolver`] (not available is
//!    [`Outcome::Skipped`], "skipped: target not available"), then restic (backup, then
//!    `forget` with the job's retention, then `prune`; neither if it keeps everything, forever) or
//!    the mirror engine. Each target has
//!    its own [`TargetReport`]. **restic's own lock is the backstop:** if restic still reports
//!    the repository locked (a restic started outside Replica, or a lock left behind), the
//!    command is retried, after `restic unlock` has removed any lock restic itself judges stale
//!    (never a live one), with backoff for up to [`Waits::restic_lock`], then fails clearly.
//! 5. **After steps:** `command`s, then each `verify` for each versioned target that backed up,
//!    when its schedule says it's due (no schedule: every run).
//! 6. **The result:** the worst of the targets' and steps' outcomes ([`Outcome::worst`]),
//!    recorded in the [`History`] with every target's own result. [`Outcome::exit_code`] is the
//!    CLI's exit code: 0 ok, 1 failed, 2 partly, 3 skipped. 4 (a usage or settings error) is
//!    the CLI's own ([`EXIT_USAGE`]).
//!
//! [`RunEvent`]s report progress as the run goes, for the tray.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};

use crate::cancel::CancelToken;
use crate::exclude::ExcludeRules;
use crate::history::History;
use crate::i18n::tr;
use crate::job::{AfterStep, BeforeStep, Job, Mode, Target};
use crate::mirror::{self, Compare, MirrorProgress, MirrorRequest};
use crate::platform::{ProcessTree, SecretStore, TargetLocation, TargetResolver, resolve_target};
use crate::repo_lock::{self, RepositoryLocks, Wait, Waited};
use crate::restic::{BackupProgress, BackupRequest, Repository, Restic, ResticError};
use crate::settings::AppSettings;
use crate::steps::{self, StepError};
use crate::{snapshot, tr};

/// The CLI's exit code for a usage or settings error.
pub const EXIT_USAGE: i32 = 4;

/// How a run, or one target of it, ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Ok,
    /// Done, but some files couldn't be backed up or copied, or a later step failed.
    Partly,
    /// Not done, and not an error: the target isn't available, or the job is already running.
    Skipped,
    Failed,
}

impl Outcome {
    /// The CLI's exit code: 0 ok, 1 failed, 2 partly, 3 skipped.
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Ok => 0,
            Outcome::Failed => 1,
            Outcome::Partly => 2,
            Outcome::Skipped => 3,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Outcome::Ok => 0,
            Outcome::Skipped => 1,
            Outcome::Partly => 2,
            Outcome::Failed => 3,
        }
    }

    /// The worse of two outcomes: failed, then partly, then skipped, then ok.
    pub fn worst(self, other: Outcome) -> Outcome {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }

    /// The name stored in the history and shown in `--json` (never translated).
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Partly => "partly",
            Outcome::Skipped => "skipped",
            Outcome::Failed => "failed",
        }
    }

    /// The inverse of [`Outcome::as_str`]; anything unknown reads as failed.
    pub fn parse(text: &str) -> Outcome {
        match text {
            "ok" => Outcome::Ok,
            "partly" => Outcome::Partly,
            "skipped" => Outcome::Skipped,
            _ => Outcome::Failed,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            Outcome::Ok => tr("result-ok"),
            Outcome::Partly => tr("result-partly"),
            Outcome::Skipped => tr("result-skipped"),
            Outcome::Failed => tr("result-failed"),
        })
    }
}

/// How long a run waits for a repository that's in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    /// For another Replica run to finish with it (the app's `repository_wait_minutes`).
    pub turn: Duration,
    /// For restic's own lock, held by a restic outside Replica, retried with backoff.
    pub restic_lock: Duration,
}

/// How long restic's own lock is retried for.
pub const RESTIC_LOCK_WAIT: Duration = Duration::from_secs(10 * 60);
/// The longest pause between two tries of restic's lock.
const RESTIC_LOCK_MAX_PAUSE: Duration = Duration::from_secs(30);

impl Default for Waits {
    fn default() -> Self {
        Waits {
            turn: repo_lock::DEFAULT_WAIT,
            restic_lock: RESTIC_LOCK_WAIT,
        }
    }
}

impl Waits {
    /// The waits the app's settings ask for.
    pub fn from_settings(app: &AppSettings) -> Self {
        Waits {
            turn: Duration::from_secs(u64::from(app.repository_wait_minutes) * 60),
            ..Waits::default()
        }
    }
}

/// What a run needs from its caller.
pub struct RunContext<'a> {
    /// The restic to use; `None` fails versioned targets (the CLI found none).
    pub restic: Option<&'a Restic>,
    pub resolver: &'a dyn TargetResolver,
    pub secrets: &'a dyn SecretStore,
    /// Where each run's log goes (the platform's logs folder).
    pub logs_dir: &'a Path,
    /// Where the per-job lock files go (and the repository locks' holder names).
    pub locks_dir: &'a Path,
    /// The per-repository locks: `replica-win`'s, shared by every Replica process.
    pub repo_locks: &'a dyn RepositoryLocks,
    pub waits: Waits,
    /// For a run from a queue (Run all, the tray's): asked while it waits for a repository,
    /// whether a job queued behind it could run now. Then it gives way
    /// ([`RunReport::gave_way`]), to be run again later, so one busy repository never holds up
    /// the jobs on others.
    pub give_way: Option<&'a dyn Fn() -> bool>,
    /// Replica's own staging folder for SQLite snapshots (in its data folder); each job's copies
    /// go in `<staging_dir>\<job>\<step n>\`.
    pub staging_dir: &'a Path,
    /// How command steps run so their whole tree can be stopped: `replica-win`'s job object.
    pub processes: &'a dyn ProcessTree,
    pub cancel: &'a CancelToken,
}

/// Progress, as the run goes.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Started {
        job: String,
    },
    /// It's waiting for a repository that's in use (again, if what it waits for changed).
    Waiting(Wait),
    /// The wait is over: the run goes on, or ends.
    Waited,
    Step {
        name: String,
    },
    TargetStarted {
        target: String,
    },
    Backup(BackupProgress),
    Mirror(MirrorProgress),
    TargetFinished {
        target: String,
        outcome: Outcome,
    },
    Finished {
        outcome: Outcome,
    },
}

/// One target's part of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetReport {
    pub target: String,
    /// `versioned` or `mirror`.
    pub mode: &'static str,
    pub outcome: Outcome,
    /// Why it was skipped or failed, or what went partly wrong.
    pub reason: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub snapshot_id: Option<String>,
    /// The files that couldn't be backed up or copied.
    pub problems: Vec<String>,
}

/// A finished run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// Its ID in the history, once recorded.
    pub id: Option<i64>,
    pub job: String,
    pub started_at: DateTime<Utc>,
    pub duration: Duration,
    pub outcome: Outcome,
    /// What failed for the whole run: a before step, the lock, the history.
    pub error: Option<String>,
    pub targets: Vec<TargetReport>,
    pub log_path: Option<PathBuf>,
    /// What it waited for along the way, how long, and how each wait ended.
    pub waits: Vec<Waited>,
    /// It gave way to a job queued behind it while it waited its turn
    /// ([`RunContext::give_way`]): nothing was done, logged or recorded, and its outcome says
    /// skipped. Its queue runs it again later.
    pub gave_way: bool,
}

impl RunReport {
    pub fn files(&self) -> u64 {
        self.targets.iter().map(|t| t.files).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.targets.iter().map(|t| t.bytes).sum()
    }
}

/// A job's run lock: held for the run, let go when dropped (or when the process ends).
pub struct JobLock {
    _file: File,
}

impl JobLock {
    /// Takes the job's lock, or `Ok(None)` if another run holds it.
    pub fn try_acquire(locks_dir: &Path, job: &str) -> io::Result<Option<JobLock>> {
        fs::create_dir_all(locks_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(locks_dir.join(format!("{job}.lock")))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(JobLock { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

/// A run's log file. Writing to it never fails the run.
struct RunLog {
    file: Option<File>,
    path: Option<PathBuf>,
}

impl RunLog {
    /// `<job>-<UTC time to the millisecond>-<pid>[-<n>].log`, created new: a run never writes
    /// into another run's log.
    fn create(dir: &Path, job: &str, started: DateTime<Utc>) -> RunLog {
        let none = RunLog {
            file: None,
            path: None,
        };
        let stem = format!(
            "{job}-{}-{}",
            started.format("%Y%m%d-%H%M%S-%3f"),
            std::process::id()
        );
        if let Err(e) = fs::create_dir_all(dir) {
            tracing::warn!("couldn't create the logs folder {}: {e}", dir.display());
            return none;
        }
        for n in 0..1000 {
            let name = if n == 0 {
                format!("{stem}.log")
            } else {
                format!("{stem}-{n}.log")
            };
            let path = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return RunLog {
                        file: Some(file),
                        path: Some(path),
                    };
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    tracing::warn!("couldn't create the run log {}: {e}", path.display());
                    break;
                }
            }
        }
        none
    }

    fn line(&mut self, text: &str) {
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{} {text}", Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }
}

/// Runs a job: see the module docs. The run is recorded in `history`.
pub fn run_job(
    job: &Job,
    ctx: &RunContext<'_>,
    history: &mut History,
    on_event: &mut dyn FnMut(&RunEvent),
) -> RunReport {
    let started_at = Utc::now();
    let start = Instant::now();
    let mut report = RunReport {
        id: None,
        job: job.name.clone(),
        started_at,
        duration: Duration::ZERO,
        outcome: Outcome::Ok,
        error: None,
        targets: Vec::new(),
        log_path: None,
        waits: Vec::new(),
        gave_way: false,
    };

    let _lock = match JobLock::try_acquire(ctx.locks_dir, &job.name) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            report.outcome = Outcome::Skipped;
            report.error = Some(tr("skip-already-running"));
            return report;
        }
        Err(e) => {
            report.outcome = Outcome::Failed;
            report.error = Some(tr!("error-run-lock", detail = e.to_string()));
            return report;
        }
    };
    // The repositories' locks, held to the end of the run. Taken first, so a run that gives way
    // to a job behind it has done, logged and recorded nothing.
    let repositories = repo_lock::acquire(
        job,
        ctx.repo_locks,
        ctx.locks_dir,
        repo_lock::Turn {
            bound: ctx.waits.turn,
            give_way: ctx.give_way.unwrap_or(&|| false),
        },
        ctx.cancel,
        &mut report.waits,
        &mut |wait| match wait {
            Some(wait) => on_event(&RunEvent::Waiting(wait.clone())),
            None => on_event(&RunEvent::Waited),
        },
    );
    if let Err(repo_lock::AcquireError::GaveWay) = repositories {
        report.outcome = Outcome::Skipped;
        report.gave_way = true;
        report.duration = start.elapsed();
        return report;
    }
    on_event(&RunEvent::Started {
        job: job.name.clone(),
    });
    let mut log = RunLog::create(ctx.logs_dir, &job.name, started_at);
    report.log_path = log.path.clone();
    log.line(&format!(
        "run of {} started (Replica {})",
        job.name,
        crate::VERSION
    ));
    for w in &report.waits {
        log.line(&format!(
            "{} ({}, {:.1?})",
            w.wait.text(),
            w.ended,
            w.duration
        ));
    }
    match &repositories {
        Ok(_) => run_inner(job, ctx, history, &mut log, &mut report, on_event),
        Err(e) => {
            log.line(&format!("not run: {e}"));
            report.outcome = Outcome::Failed;
            report.error = Some(e.to_string());
        }
    }

    report.outcome = report
        .targets
        .iter()
        .fold(report.outcome, |o, t| o.worst(t.outcome));
    if report.targets.is_empty() && report.outcome == Outcome::Ok && report.error.is_none() {
        // Nothing to do isn't success.
        report.outcome = Outcome::Failed;
        report.error = Some(tr("error-job-no-targets"));
    }
    report.duration = start.elapsed();
    for t in &report.targets {
        log.line(&format!(
            "target {}: {} {}",
            t.target,
            t.outcome.as_str(),
            t.reason.as_deref().unwrap_or("")
        ));
    }
    log.line(&format!(
        "run of {} ended: {} (exit code {}) after {:.1?}",
        job.name,
        report.outcome.as_str(),
        report.outcome.exit_code(),
        report.duration
    ));
    match history.record(&report) {
        Ok(id) => report.id = Some(id),
        Err(e) => {
            log.line(&format!("the run couldn't be recorded: {e}"));
            report.error.get_or_insert_with(|| e.to_string());
            report.outcome = report.outcome.worst(Outcome::Partly);
        }
    }
    tracing::info!(
        job = %job.name,
        outcome = report.outcome.as_str(),
        files = report.files(),
        bytes = report.bytes(),
        "run finished in {:.1?}",
        report.duration
    );
    on_event(&RunEvent::Finished {
        outcome: report.outcome,
    });
    report
}

fn run_inner(
    job: &Job,
    ctx: &RunContext<'_>,
    history: &mut History,
    log: &mut RunLog,
    report: &mut RunReport,
    on_event: &mut dyn FnMut(&RunEvent),
) {
    // The snapshot plan first, so a problem (or a path that can't be resolved) fails the run
    // before anything is touched.
    let databases: Vec<PathBuf> = job
        .before
        .iter()
        .filter_map(|s| match s {
            BeforeStep::SqliteSnapshot { db } => Some(db.clone()),
            BeforeStep::Command { .. } => None,
        })
        .collect();
    let job_staging = ctx.staging_dir.join(&job.name);
    let plan = match snapshot::plan(&job.sources, &databases, ctx.staging_dir, &job_staging) {
        Ok(plan) => plan,
        Err(e) => {
            log.line(&format!("before steps refused: {e}"));
            report.outcome = Outcome::Failed;
            report.error = Some(e.to_string());
            return;
        }
    };

    // Copies left by steps since removed aren't backed up as if current.
    if let Err(e) = snapshot::tidy(&job_staging, &plan) {
        log.line(&format!("before steps refused: {e}"));
        report.outcome = Outcome::Failed;
        report.error = Some(tr!("error-snapshot-tidy", detail = e.to_string()));
        return;
    }

    // Before steps: a failure ends the run here.
    let mut copies = plan.copies.iter();
    for step in &job.before {
        let result = match step {
            BeforeStep::SqliteSnapshot { db } => {
                let (_, dir) = copies.next().expect("one planned copy per snapshot step");
                on_event(&RunEvent::Step {
                    name: format!("sqlite-snapshot {}", db.display()),
                });
                log.line(&format!(
                    "sqlite snapshot of {} into {}",
                    db.display(),
                    dir.display()
                ));
                snapshot::sqlite_snapshot(db, dir)
                    .map(|copy| log.line(&format!("snapshot written: {}", copy.display())))
                    .map_err(|e| e.to_string())
            }
            BeforeStep::Command { cmd, timeout_secs } => {
                on_event(&RunEvent::Step { name: cmd.clone() });
                run_step(cmd, *timeout_secs, ctx, log).map_err(|e| e.to_string())
            }
        };
        if let Err(e) = result {
            log.line(&format!("before step failed: {e}"));
            report.outcome = Outcome::Failed;
            report.error = Some(e);
            return;
        }
    }
    // Live database files are left out by exact path, and the plan's sources hold each copy
    // (run_target reads both from the plan).

    let mut verified_ok: Vec<(usize, Repository)> = Vec::new();
    for target in &job.targets {
        if ctx.cancel.is_cancelled() {
            report.targets.push(target_report(
                job,
                target,
                Outcome::Failed,
                Some(tr("error-cancelled")),
            ));
            continue;
        }
        let name = target.display_name();
        on_event(&RunEvent::TargetStarted {
            target: name.clone(),
        });
        log.line(&format!("target {name}"));
        let (t, repo) = run_target(job, target, &plan, ctx, log, &mut report.waits, on_event);
        on_event(&RunEvent::TargetFinished {
            target: name,
            outcome: t.outcome,
        });
        if let Some(repo) = repo {
            verified_ok.push((report.targets.len(), repo));
        }
        report.targets.push(t);
    }

    // After steps.
    for step in &job.after {
        match step {
            AfterStep::Command { cmd, timeout_secs } => {
                on_event(&RunEvent::Step { name: cmd.clone() });
                if let Err(e) = run_step(cmd, *timeout_secs, ctx, log) {
                    log.line(&format!("after step failed: {e}"));
                    report.outcome = report.outcome.worst(Outcome::Partly);
                    report.error.get_or_insert_with(|| e.to_string());
                }
            }
            AfterStep::Verify {
                schedule,
                read_data_subset,
            } => {
                let Some(restic) = ctx.restic else { continue };
                for (index, repo) in &verified_ok {
                    let t = &mut report.targets[*index];
                    let due = match (schedule, history.last_verify(&job.name, &t.target)) {
                        (None, _) | (_, Ok(None)) | (_, Err(_)) => true,
                        (Some(s), Ok(Some(last))) => {
                            Local::now() >= s.next_after(&last.with_timezone(&Local))
                        }
                    };
                    if !due {
                        continue;
                    }
                    on_event(&RunEvent::Step {
                        name: format!("verify {}", t.target),
                    });
                    log.line(&format!("verify {} ({read_data_subset})", t.target));
                    let mut locked = ResticLockRetry::new(restic, repo, &t.target, ctx);
                    let checked = locked.run(log, &mut report.waits, on_event, &mut |_| {
                        restic.check(repo, Some(read_data_subset), ctx.cancel)
                    });
                    match checked {
                        Ok(summary) if summary.num_errors == 0 => {
                            let _ = history.record_verify(&job.name, &t.target, Utc::now());
                        }
                        Ok(summary) => {
                            t.outcome = Outcome::Failed;
                            t.reason = Some(tr!("error-verify-errors", count = summary.num_errors));
                        }
                        Err(e) => {
                            t.outcome = Outcome::Failed;
                            t.reason = Some(e);
                        }
                    }
                }
            }
        }
    }
}

/// A run's event callback, handed to the command [`ResticLockRetry`] retries for its progress.
type OnEvent<'e> = dyn FnMut(&RunEvent) + 'e;

/// restic's own lock as the backstop: a command that finds the repository locked is
/// retried, the first time at once after `restic unlock` (which removes only locks restic judges
/// stale, never a live one), then with backoff, up to [`Waits::restic_lock`] in all for the
/// target. Errors come back as their text, for the target's reason.
struct ResticLockRetry<'r> {
    restic: &'r Restic,
    repo: &'r Repository,
    target: &'r str,
    ctx: &'r RunContext<'r>,
    started: Option<Instant>,
    unlocked: bool,
    pause: Duration,
}

impl<'r> ResticLockRetry<'r> {
    fn new(
        restic: &'r Restic,
        repo: &'r Repository,
        target: &'r str,
        ctx: &'r RunContext<'r>,
    ) -> Self {
        ResticLockRetry {
            restic,
            repo,
            target,
            ctx,
            started: None,
            unlocked: false,
            pause: Duration::from_secs(1),
        }
    }

    fn run<T>(
        &mut self,
        log: &mut RunLog,
        waits: &mut Vec<Waited>,
        on_event: &mut dyn FnMut(&RunEvent),
        call: &mut dyn FnMut(&mut OnEvent<'_>) -> Result<T, ResticError>,
    ) -> Result<T, String> {
        let mut waiting: Option<(Wait, Instant)> = None;
        let mut ended = "ok";
        let result = loop {
            let detail = match call(on_event) {
                Err(ResticError::Locked { detail, .. }) => detail,
                other => break other.map_err(|e| e.to_string()),
            };
            let started = *self.started.get_or_insert_with(Instant::now);
            if !self.unlocked {
                self.unlocked = true;
                log.line(&format!("restic reports the repository locked: {detail}"));
                log.line("removing the locks restic judges stale (restic unlock), then retrying");
                if let Err(e) = self.restic.unlock_stale(self.repo) {
                    log.line(&format!("restic unlock failed: {e}"));
                }
                continue;
            }
            let left = self.ctx.waits.restic_lock.saturating_sub(started.elapsed());
            // Counted as a wait even when it gives up at once, so it's reported.
            if waiting.is_none() {
                let wait = Wait::ResticLock {
                    target: self.target.to_string(),
                };
                log.line(&wait.text());
                on_event(&RunEvent::Waiting(wait.clone()));
                waiting = Some((wait, Instant::now()));
            }
            if left.is_zero() {
                ended = "timed-out";
                log.line(&format!("still locked, giving up: {detail}"));
                break Err(tr!(
                    "error-restic-locked-too-long",
                    minutes = started.elapsed().as_secs().div_ceil(60),
                    detail = detail
                ));
            }
            if self.ctx.cancel.wait_timeout(self.pause.min(left)) {
                ended = "cancelled";
                break Err(tr("error-cancelled"));
            }
            self.pause = (self.pause * 2).min(RESTIC_LOCK_MAX_PAUSE);
        };
        if let Some((wait, since)) = waiting {
            on_event(&RunEvent::Waited);
            waits.push(Waited {
                wait,
                duration: since.elapsed(),
                ended,
            });
        }
        result
    }
}

fn run_step(
    cmd: &str,
    timeout_secs: u64,
    ctx: &RunContext<'_>,
    log: &mut RunLog,
) -> Result<(), StepError> {
    log.line(&format!("command: {cmd}"));
    steps::run_command(
        cmd,
        Duration::from_secs(timeout_secs),
        ctx.cancel,
        ctx.processes,
        &mut |line| log.line(&format!("  | {line}")),
    )
}

fn target_report(
    job: &Job,
    target: &Target,
    outcome: Outcome,
    reason: Option<String>,
) -> TargetReport {
    TargetReport {
        target: target.display_name(),
        mode: match job.mode {
            Mode::Versioned => "versioned",
            Mode::Mirror => "mirror",
        },
        outcome,
        reason,
        files: 0,
        bytes: 0,
        snapshot_id: None,
        problems: Vec::new(),
    }
}

/// One target: resolve it, run its engine. For a versioned target that backed up, also returns
/// its repository, for verify.
fn run_target(
    job: &Job,
    target: &Target,
    plan: &snapshot::SnapshotPlan,
    ctx: &RunContext<'_>,
    log: &mut RunLog,
    waits: &mut Vec<Waited>,
    on_event: &mut dyn FnMut(&RunEvent),
) -> (TargetReport, Option<Repository>) {
    let fail = |reason: String| target_report(job, target, Outcome::Failed, Some(reason));
    let name = target.display_name();
    let (sources, live_files) = (&plan.sources[..], &plan.live_files[..]);
    let location = match resolve_target(target, ctx.resolver, ctx.secrets) {
        Ok(TargetLocation::Available(path)) => path,
        Ok(TargetLocation::NotAvailable(why)) => {
            log.line(&format!("skipped: {why}"));
            let reason = format!("{}: {why}", tr("skip-target-not-available"));
            return (
                target_report(job, target, Outcome::Skipped, Some(reason)),
                None,
            );
        }
        Err(e) => return (fail(e.to_string()), None),
    };
    log.line(&format!("at {}", location.display()));
    match job.mode {
        Mode::Versioned => {
            let Some(restic) = ctx.restic else {
                return (fail(tr("error-run-no-restic")), None);
            };
            let repo = match Repository::from_store(&location, target, ctx.secrets) {
                Ok(repo) => repo,
                Err(e) => return (fail(e.to_string()), None),
            };
            let source_paths: Vec<PathBuf> = sources.iter().map(|s| s.path.clone()).collect();
            let excludes = match ExcludeRules::new(
                sources.iter().map(|s| (s.path.as_path(), s.exclude.iter())),
                true,
            )
            .and_then(|r| r.with_exact_paths(live_files.iter().map(PathBuf::as_path)))
            {
                Ok(rules) => rules,
                Err(e) => return (fail(e.to_string()), None),
            };
            let request = BackupRequest {
                job: &job.name,
                sources: &source_paths,
                excludes: &excludes,
            };
            let mut locked = ResticLockRetry::new(restic, &repo, &name, ctx);
            let backup = locked.run(log, waits, on_event, &mut |on_event| {
                restic.backup(
                    &repo,
                    &request,
                    &mut |p| on_event(&RunEvent::Backup(p.clone())),
                    ctx.cancel,
                )
            });
            let mut t = target_report(job, target, Outcome::Ok, None);
            match backup {
                Ok(outcome) => {
                    t.files = outcome.summary.files_new + outcome.summary.files_changed;
                    t.bytes = outcome.summary.data_added;
                    t.snapshot_id =
                        Some(outcome.summary.snapshot_id.clone()).filter(|s| !s.is_empty());
                    if outcome.partial {
                        t.outcome = Outcome::Partly;
                        t.problems = outcome
                            .unreadable
                            .iter()
                            .map(|u| format!("{}: {}", u.item, u.message))
                            .collect();
                        t.reason = Some(tr!("error-run-unreadable", count = t.problems.len()));
                        for p in &t.problems {
                            log.line(&format!("unreadable: {p}"));
                        }
                    }
                }
                Err(e) => return (fail(e), None),
            }
            log.line(&format!(
                "snapshot {}",
                t.snapshot_id.as_deref().unwrap_or("-")
            ));
            // Keep forever: nothing is forgotten or pruned, ever.
            if job.retention.keep_forever {
                log.line("keep forever: no forget or prune");
                return (t, Some(repo));
            }
            // Retention: forget, then prune. A failure here leaves the new snapshot, so it's
            // "partly", not failed.
            let mut locked = ResticLockRetry::new(restic, &repo, &name, ctx);
            let retention = locked
                .run(log, waits, on_event, &mut |_| {
                    restic.forget(&repo, &job.name, &job.retention)
                })
                .and_then(|f| {
                    log.line(&format!("forgot {} snapshot(s)", f.removed.len()));
                    locked.run(log, waits, on_event, &mut |_| {
                        restic.prune(&repo, ctx.cancel)
                    })
                });
            if let Err(e) = retention {
                log.line(&format!("retention failed: {e}"));
                t.outcome = t.outcome.worst(Outcome::Partly);
                t.reason.get_or_insert(e);
            }
            (t, Some(repo))
        }
        Mode::Mirror => {
            let request = MirrorRequest {
                sources,
                target: &location,
                compare: Compare::SizeAndTime,
                delete_limit_percent: job.mirror_delete_limit_percent,
                case_insensitive: true,
                exclude_paths: live_files,
            };
            match mirror::mirror(
                &request,
                &mut |p| on_event(&RunEvent::Mirror(p.clone())),
                ctx.cancel,
            ) {
                Ok(outcome) => {
                    let mut t = target_report(job, target, Outcome::Ok, None);
                    t.files = outcome.copied;
                    t.bytes = outcome.bytes_copied;
                    log.line(&format!(
                        "copied {}, deleted {}, unchanged {}, skipped {}",
                        outcome.copied, outcome.deleted, outcome.unchanged, outcome.skipped
                    ));
                    if outcome.partial() {
                        t.outcome = Outcome::Partly;
                        t.problems = outcome
                            .failed
                            .iter()
                            .map(|f| format!("{}: {}", f.path.display(), f.message))
                            .collect();
                        t.reason = Some(tr!("error-run-unreadable", count = t.problems.len()));
                        for p in &t.problems {
                            log.line(&format!("problem: {p}"));
                        }
                    }
                    (t, None)
                }
                Err(e) => (fail(e.to_string()), None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes() {
        assert_eq!(Outcome::Ok.exit_code(), 0);
        assert_eq!(Outcome::Failed.exit_code(), 1);
        assert_eq!(Outcome::Partly.exit_code(), 2);
        assert_eq!(Outcome::Skipped.exit_code(), 3);
        assert_eq!(EXIT_USAGE, 4);
    }

    #[test]
    fn the_worst_outcome_wins() {
        use Outcome::*;
        assert_eq!(Ok.worst(Skipped), Skipped);
        assert_eq!(Skipped.worst(Partly), Partly);
        assert_eq!(Partly.worst(Failed), Failed);
        assert_eq!(Failed.worst(Ok), Failed);
        assert_eq!(
            [Ok, Skipped, Ok].into_iter().fold(Ok, Outcome::worst),
            Skipped
        );
    }

    #[test]
    fn outcomes_round_trip_as_text() {
        for o in [
            Outcome::Ok,
            Outcome::Partly,
            Outcome::Skipped,
            Outcome::Failed,
        ] {
            assert_eq!(Outcome::parse(o.as_str()), o);
        }
        assert_eq!(Outcome::parse("nonsense"), Outcome::Failed);
    }

    #[test]
    fn the_lock_is_held_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let first = JobLock::try_acquire(dir.path(), "hive").unwrap();
        assert!(first.is_some());
        assert!(
            JobLock::try_acquire(dir.path(), "hive").unwrap().is_none(),
            "held"
        );
        assert!(
            JobLock::try_acquire(dir.path(), "other").unwrap().is_some(),
            "per job"
        );
        drop(first);
        assert!(
            JobLock::try_acquire(dir.path(), "hive").unwrap().is_some(),
            "let go"
        );
    }
}
