//! The commands. Each returns the outcome its exit code comes from, or a usage error (4).

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Local, Utc};
use replica_core::cancel::CancelToken;
use replica_core::i18n::tr;
use replica_core::job::{AfterStep, DEFAULT_READ_DATA_SUBSET, Job, Mode, Target};
use replica_core::platform::{Credential, Secret, SecretStore, TargetLocation, resolve_target};
use replica_core::repo_lock::{self, Waited};
use replica_core::restic::{Repository, Restic, ResticError, RestoreRequest, snapshot_path};
use replica_core::run::{JobLock, Outcome, RunContext, RunEvent, RunReport, Waits, run_job};
use replica_core::schedule::DEFAULT_GRACE;
use replica_core::tr;
use replica_win::console::read_secret_line;
use replica_win::jobs::JobObjectTree;
use replica_win::keys::{self, Entered, NotSaved};
use replica_win::repo_locks::NamedMutexLocks;

use super::{Env, Output, UsageError, duration_text, json, worst};

type Result<T> = std::result::Result<T, UsageError>;

// --- replica run -------------------------------------------------------------------------------

pub fn run(env: &mut Env, job: Option<&str>, all: bool, out: Output) -> Result<Outcome> {
    let jobs: Vec<Job> = match (job, all) {
        (Some(name), false) => vec![env.job(name)?.clone()],
        (None, true) => env
            .settings
            .jobs
            .iter()
            .filter(|j| j.enabled)
            .cloned()
            .collect(),
        _ => return Err(UsageError(tr("cli-run-job-or-all"))),
    };
    if jobs.is_empty() {
        out.say(&tr("cli-run-no-jobs"));
        if out.json {
            json::print(&json::Run {
                schema: json::SCHEMA,
                outcome: Outcome::Ok.as_str(),
                exit_code: 0,
                jobs: Vec::new(),
            });
        }
        return Ok(Outcome::Ok);
    }
    let (logs, locks, staging) = (
        env.paths.run_logs_dir(),
        env.paths.file("locks"),
        env.paths.file("staging"),
    );
    let cancel = CancelToken::new();
    let repo_locks = NamedMutexLocks::new();
    let bound = Waits::from_settings(&env.settings.app);
    // Run all is a queue: the first job whose repositories are free goes first, and one
    // waiting its turn gives way whenever a job behind it could run, so a busy repository never
    // holds up the jobs on others. Each job's wait counts from its first try, across tries.
    let mut pending: VecDeque<usize> = (0..jobs.len()).collect();
    let mut first_tried: HashMap<usize, (Instant, Vec<Waited>)> = HashMap::new();
    let mut reports: Vec<Option<RunReport>> = vec![None; jobs.len()];
    while let Some(at) = repo_lock::pick_next(pending.iter().map(|&i| &jobs[i]), &repo_locks) {
        let i = pending.remove(at).expect("picked from the queue");
        let job = &jobs[i];
        let behind: Vec<&Job> = pending.iter().map(|&i| &jobs[i]).collect();
        let give_way = || behind.iter().any(|j| repo_lock::is_free(j, &repo_locks));
        let (since, _) = first_tried.entry(i).or_insert_with(|| {
            out.say(&tr!("cli-run-started", job = job.name.as_str()));
            (Instant::now(), Vec::new())
        });
        let waits = Waits {
            turn: bound.turn.saturating_sub(since.elapsed()),
            ..bound
        };
        let ctx = RunContext {
            restic: env.restic.as_ref().ok(),
            resolver: &env.targets,
            secrets: &env.secrets,
            logs_dir: &logs,
            locks_dir: &locks,
            repo_locks: &repo_locks,
            waits,
            give_way: (!behind.is_empty()).then_some(&give_way as &dyn Fn() -> bool),
            staging_dir: &staging,
            processes: &JobObjectTree,
            cancel: &cancel,
        };
        let mut target = String::new();
        let mut report = run_job(job, &ctx, &mut env.history, &mut |event| match event {
            RunEvent::TargetStarted { target: t } => target = t.clone(),
            RunEvent::Backup(p) => out.progress(&format!(
                "{} → {target}: {:.0}%",
                job.name,
                p.percent_done * 100.0
            )),
            RunEvent::Mirror(p) if p.bytes_total > 0 => out.progress(&format!(
                "{} → {target}: {:.0}%",
                job.name,
                p.bytes_done as f64 * 100.0 / p.bytes_total as f64
            )),
            RunEvent::TargetFinished { .. } => out.end_progress(),
            // On stdout with the run's other lines; with --json, on stderr beside the document.
            RunEvent::Waiting(wait) => {
                out.end_progress();
                if out.json {
                    out.warn(&wait.text());
                } else {
                    out.say(&wait.text());
                }
            }
            _ => {}
        });
        out.end_progress();
        let (_, earlier) = first_tried.get_mut(&i).expect("entered above");
        if report.gave_way {
            // Back to the front: it's tried again once the job that could run has started.
            earlier.append(&mut report.waits);
            pending.push_front(i);
            continue;
        }
        report.waits.splice(0..0, earlier.drain(..));
        print_report(&report, out);
        reports[i] = Some(report);
    }
    // In the jobs' order, whatever order they ran in.
    let reports: Vec<RunReport> = reports.into_iter().flatten().collect();
    let outcome = worst(reports.iter().map(|r| r.outcome));
    if jobs.len() > 1 {
        let count = |o: Outcome| reports.iter().filter(|r| r.outcome == o).count();
        out.say(&tr!(
            "cli-run-all-summary",
            ok = count(Outcome::Ok),
            failed = count(Outcome::Failed),
            partly = count(Outcome::Partly),
            skipped = count(Outcome::Skipped)
        ));
    }
    if out.json {
        json::print(&json::Run {
            schema: json::SCHEMA,
            outcome: outcome.as_str(),
            exit_code: outcome.exit_code(),
            jobs: reports.iter().map(job_json).collect(),
        });
    }
    Ok(outcome)
}

fn print_report(report: &RunReport, out: Output) {
    let job = report.job.as_str();
    for t in &report.targets {
        let reason = t.reason.as_deref().unwrap_or("");
        match t.outcome {
            Outcome::Ok => out.say(&tr!(
                "cli-run-target-ok",
                job = job,
                target = t.target.as_str(),
                duration = duration_text(report.duration)
            )),
            Outcome::Partly => out.warn(&tr!(
                "cli-run-target-partly",
                job = job,
                target = t.target.as_str(),
                reason = reason
            )),
            Outcome::Skipped => out.warn(&tr!(
                "cli-run-target-skipped",
                job = job,
                target = t.target.as_str(),
                reason = reason
            )),
            Outcome::Failed => out.warn(&tr!(
                "cli-run-target-failed",
                job = job,
                target = t.target.as_str(),
                reason = reason
            )),
        }
    }
    let total = report.targets.len();
    let ok = report
        .targets
        .iter()
        .filter(|t| t.outcome == Outcome::Ok)
        .count();
    let error = report.error.as_deref().unwrap_or("");
    match report.outcome {
        Outcome::Ok => out.say(&tr!("cli-run-job-ok", job = job)),
        Outcome::Partly => out.warn(&tr!(
            "cli-run-job-partly",
            job = job,
            ok = ok,
            total = total
        )),
        Outcome::Skipped => out.warn(&tr!("cli-run-job-skipped", job = job, reason = error)),
        Outcome::Failed => {
            if !error.is_empty() {
                out.warn(error);
            }
            out.warn(&tr!("cli-run-job-failed", job = job));
        }
    }
}

fn job_json(r: &RunReport) -> json::JobRun {
    json::JobRun {
        job: r.job.clone(),
        run_id: r.id,
        outcome: r.outcome.as_str(),
        exit_code: r.outcome.exit_code(),
        started_at: r.started_at.to_rfc3339(),
        duration_ms: r.duration.as_millis() as u64,
        files: r.files(),
        bytes: r.bytes(),
        error: r.error.clone(),
        log: r.log_path.as_ref().map(|p| p.display().to_string()),
        waits: r
            .waits
            .iter()
            .map(|w| json::Wait {
                kind: w.wait.kind(),
                target: w.wait.target().to_string(),
                job: w.wait.job().map(str::to_owned),
                duration_ms: w.duration.as_millis() as u64,
                ended: w.ended,
            })
            .collect(),
        targets: r
            .targets
            .iter()
            .map(|t| json::TargetRun {
                target: t.target.clone(),
                mode: t.mode,
                outcome: t.outcome.as_str(),
                reason: t.reason.clone(),
                files: t.files,
                bytes: t.bytes,
                snapshot_id: t.snapshot_id.clone(),
            })
            .collect(),
    }
}

// --- replica list and status -------------------------------------------------------------------

pub fn list(env: &Env, status: bool, out: Output) -> Result<Outcome> {
    let now = Local::now();
    let locks = env.paths.file("locks");
    let mut infos = Vec::new();
    for job in &env.settings.jobs {
        let last = env
            .history
            .recent(Some(&job.name), 1)
            .map_err(|e| UsageError(e.to_string()))?
            .into_iter()
            .next();
        let last_success = env
            .history
            .last_success(&job.name)
            .map_err(|e| UsageError(e.to_string()))?;
        let scheduled = job.enabled && !env.settings.app.pause_schedules;
        let next_run = job.schedule.as_ref().filter(|_| scheduled).map(|s| {
            let from = last
                .as_ref()
                .map(|r| r.started_at.with_timezone(&Local))
                .unwrap_or(now);
            s.next_after(&from).max(now)
        });
        let overdue = scheduled
            && match (&job.schedule, last_success) {
                (Some(s), Some(at)) => s.is_overdue(&at.with_timezone(&Local), &now, DEFAULT_GRACE),
                _ => false,
            };
        let running = status.then(|| {
            // Taken for a moment and let go: held means a run is going on.
            JobLock::try_acquire(&locks, &job.name).is_ok_and(|lock| lock.is_none())
        });
        infos.push(json::JobInfo {
            name: job.name.clone(),
            enabled: job.enabled,
            mode: mode_name(job.mode),
            schedule: job.schedule.as_ref().map(|s| s.to_string()),
            targets: job.targets.iter().map(Target::display_name).collect(),
            last_run: last.map(|r| json::LastRun {
                run_id: r.id,
                started_at: r.started_at.to_rfc3339(),
                outcome: r.outcome.as_str(),
                exit_code: r.exit_code,
            }),
            last_success: last_success.map(|t| t.to_rfc3339()),
            next_run: next_run.map(|t| t.to_rfc3339()),
            overdue,
            keep_forever: job.mode == Mode::Versioned && job.retention.keep_forever,
            running,
        });
    }
    if out.json {
        json::print(&json::Jobs {
            schema: json::SCHEMA,
            jobs: infos,
        });
        return Ok(Outcome::Ok);
    }
    let rows: Vec<[String; 4]> = infos
        .iter()
        .map(|i| {
            let last_result = i.last_run.as_ref().map_or_else(
                || tr("result-never-run"),
                |r| Outcome::parse(r.outcome).to_string(),
            );
            let last_run = i
                .last_run
                .as_ref()
                .map_or_else(String::new, |r| local_time(&r.started_at));
            let mut next = if !i.enabled {
                tr("cli-list-disabled")
            } else {
                i.next_run
                    .as_deref()
                    .map_or_else(|| tr("cli-list-not-scheduled"), local_time)
            };
            if i.overdue {
                next = format!("{next} ({})", tr("cli-list-overdue"));
            }
            if i.running == Some(true) {
                next = format!("{next} · {}", tr("result-running"));
            }
            let name = if i.keep_forever {
                format!("{} · {}", i.name, tr("cli-list-forever"))
            } else {
                i.name.clone()
            };
            [name, last_result, last_run, next]
        })
        .collect();
    let header = [
        tr("cli-list-job"),
        tr("cli-list-last-result"),
        tr("cli-list-last-run"),
        tr("cli-list-next-run"),
    ];
    print_table(&header, &rows);
    if status {
        let running: Vec<&str> = infos
            .iter()
            .filter(|i| i.running == Some(true))
            .map(|i| i.name.as_str())
            .collect();
        if running.is_empty() {
            out.say(&tr("cli-status-idle"));
        }
        for job in running {
            out.say(&tr!("cli-status-running-job", job = job));
        }
    }
    Ok(Outcome::Ok)
}

fn print_table(header: &[String; 4], rows: &[[String; 4]]) {
    let mut widths = header.clone().map(|h| h.chars().count());
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &[String; 4]| {
        let padded: Vec<String> = cells
            .iter()
            .zip(widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        println!("{}", padded.join("  ").trim_end());
    };
    line(header);
    for row in rows {
        line(row);
    }
}

fn local_time(rfc3339: &str) -> String {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Versioned => "versioned",
        Mode::Mirror => "mirror",
    }
}

// --- Versioned targets: snapshots, restore, verify, init, key show and set ----------------------

/// The job's targets, or the one `--target` names (by its name, ignoring case).
fn chosen<'a>(job: &'a Job, target: Option<&str>) -> Result<Vec<&'a Target>> {
    match target {
        None => Ok(job.targets.iter().collect()),
        Some(name) => job
            .targets
            .iter()
            .find(|t| t.display_name().eq_ignore_ascii_case(name))
            .map(|t| vec![t])
            .ok_or_else(|| {
                UsageError(tr!(
                    "error-target-not-found",
                    job = job.name.as_str(),
                    target = name
                ))
            }),
    }
}

fn versioned(job: &Job) -> Result<()> {
    if job.mode == Mode::Versioned {
        Ok(())
    } else {
        Err(UsageError(tr!(
            "cli-not-versioned",
            job = job.name.as_str()
        )))
    }
}

/// A target's folder now, or how it ended.
fn locate(env: &Env, target: &Target) -> std::result::Result<PathBuf, (Outcome, String)> {
    match resolve_target(target, &env.targets, &env.secrets) {
        Ok(TargetLocation::Available(path)) => Ok(path),
        Ok(TargetLocation::NotAvailable(why)) => Err((
            Outcome::Skipped,
            format!("{}: {why}", tr("skip-target-not-available")),
        )),
        Err(e) => Err((Outcome::Failed, e.to_string())),
    }
}

/// A versioned target's repository, ready to use, or how it ended.
fn open<'e>(
    env: &'e Env,
    target: &Target,
) -> std::result::Result<(&'e Restic, Repository), (Outcome, String)> {
    let restic = env
        .restic
        .as_ref()
        .map_err(|e| (Outcome::Failed, e.clone()))?;
    let location = locate(env, target)?;
    let repo = Repository::from_store(&location, target, &env.secrets)
        .map_err(|e| (Outcome::Failed, e.to_string()))?;
    Ok((restic, repo))
}

/// Takes `target`'s repository lock for restic work outside a run, waiting its turn as a
/// run does, up to the app's `repository_wait_minutes`, with the wait printed (on stderr with
/// `--json`). Held until dropped.
fn take_turn(
    env: &Env,
    target: &Target,
    holder: &str,
    out: Output,
) -> std::result::Result<repo_lock::Held, (Outcome, String)> {
    let bound = Waits::from_settings(&env.settings.app).turn;
    repo_lock::acquire_target(
        target,
        holder,
        &NamedMutexLocks::new(),
        &env.paths.file("locks"),
        repo_lock::Turn::alone(bound),
        &CancelToken::new(),
        &mut |wait| {
            if let Some(wait) = wait {
                if out.json {
                    out.warn(&wait.text());
                } else {
                    out.say(&wait.text());
                }
            }
        },
    )
    .map_err(|e| (Outcome::Failed, e.to_string()))
}

pub fn snapshots(env: &Env, job: &str, out: Output) -> Result<Outcome> {
    let job = env.job(job)?;
    versioned(job)?;
    let mut doc = json::Snapshots {
        schema: json::SCHEMA,
        job: job.name.clone(),
        snapshots: Vec::new(),
        unavailable: Vec::new(),
    };
    let mut outcomes = Vec::new();
    for target in &job.targets {
        let name = target.display_name();
        let listed = open(env, target).and_then(|(restic, repo)| {
            // A quick look: restic's own lock is retried briefly rather than waited for.
            repo_lock::retry_locked(
                restic,
                &repo,
                repo_lock::LISTING_WAIT,
                &CancelToken::new(),
                || restic.snapshots(&repo, Some(&job.name)),
            )
            .map_err(|e| (Outcome::Failed, e.to_string()))
        });
        match listed {
            Ok(snaps) => {
                outcomes.push(Outcome::Ok);
                if snaps.is_empty() {
                    out.say(&tr!("cli-snapshots-none", job = job.name.as_str()));
                }
                for s in snaps {
                    let pinned = s.is_pinned();
                    let pin = if pinned {
                        format!("  {}", tr("cli-snapshots-pinned"))
                    } else {
                        String::new()
                    };
                    out.say(&format!(
                        "{}  {}  {name}{pin}",
                        s.short_id,
                        s.time.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S")
                    ));
                    doc.snapshots.push(json::Snapshot {
                        target: name.clone(),
                        id: s.id,
                        short_id: s.short_id,
                        time: s.time.to_rfc3339(),
                        paths: s.paths,
                        hostname: s.hostname,
                        pinned,
                    });
                }
            }
            Err((outcome, reason)) => {
                outcomes.push(outcome);
                out.warn(&format!("{name}: {reason}"));
                doc.unavailable.push(json::Unavailable {
                    target: name,
                    outcome: outcome.as_str(),
                    reason,
                });
            }
        }
    }
    if out.json {
        json::print(&doc);
    }
    Ok(worst(outcomes))
}

#[allow(clippy::too_many_arguments)]
pub fn restore(
    env: &Env,
    job: &str,
    snapshot: &str,
    path: &Path,
    to: &Path,
    target: Option<&str>,
    overwrite: bool,
    out: Output,
) -> Result<Outcome> {
    let job = env.job(job)?;
    versioned(job)?;
    // A path as it was backed up (`C:\Work\a.txt`), or as a snapshot shows it (`/C/Work/a.txt`).
    let item = if path.to_string_lossy().starts_with('/') {
        path.to_string_lossy().into_owned()
    } else {
        let absolute = std::path::absolute(path).map_err(|e| UsageError(e.to_string()))?;
        snapshot_path(&absolute)
    };
    let sources: Vec<PathBuf> = job.sources.iter().map(|s| s.path.clone()).collect();
    let mut last = (Outcome::Skipped, String::new());
    for t in chosen(job, target)? {
        let (restic, repo) = match open(env, t) {
            Ok(opened) => opened,
            Err(ended) => {
                out.warn(&format!("{}: {}", t.display_name(), ended.1));
                last = ended;
                continue;
            }
        };
        let request = RestoreRequest {
            job: &job.name,
            snapshot,
            item: Some(&item),
            target: to,
            sources: &sources,
            allow_over_source: overwrite,
        };
        let _turn = match take_turn(env, t, &job.name, out) {
            Ok(turn) => turn,
            Err((outcome, reason)) => {
                out.warn(&format!("{}: {reason}", t.display_name()));
                return Ok(outcome);
            }
        };
        match restic.restore(&repo, &request, &CancelToken::new()) {
            Ok(_) => {
                out.say(&tr!(
                    "cli-restore-done",
                    path = path.display().to_string(),
                    snapshot = snapshot,
                    to = to.display().to_string()
                ));
                return Ok(Outcome::Ok);
            }
            // Not this target's: another of the job's targets may have it.
            Err(e @ replica_core::restic::ResticError::SnapshotNotFound { .. }) => {
                last = (Outcome::Failed, e.to_string());
            }
            Err(e) => {
                out.warn(&e.to_string());
                return Ok(Outcome::Failed);
            }
        }
    }
    if last.1.is_empty() {
        out.warn(&tr!("cli-no-target", job = job.name.as_str()));
    } else if last.0 == Outcome::Failed {
        out.warn(&last.1);
    }
    Ok(last.0)
}

/// `replica pin` and `replica unpin`: tags the snapshot on the first of the job's targets
/// (or `target`) that has it, holding the repository's lock. restic gives the snapshot a new ID
/// when its tags change: it's printed.
pub fn pin(
    env: &Env,
    job: &str,
    snapshot: &str,
    target: Option<&str>,
    pinned: bool,
    out: Output,
) -> Result<Outcome> {
    let job = env.job(job)?;
    versioned(job)?;
    let mut last = (Outcome::Skipped, String::new());
    for t in chosen(job, target)? {
        let name = t.display_name();
        let changed = open(env, t).and_then(|(restic, repo)| {
            let _turn = take_turn(env, t, &job.name, out)?;
            let failed = |e: ResticError| (Outcome::Failed, e.to_string());
            let id = restic
                .job_snapshot(&repo, &job.name, snapshot)
                .map_err(failed)?;
            repo_lock::retry_locked(
                restic,
                &repo,
                repo_lock::LISTING_WAIT,
                &CancelToken::new(),
                || restic.set_pinned(&repo, &id, pinned),
            )
            .map_err(failed)
        });
        match changed {
            Ok(id) => {
                let short = &id[..id.len().min(8)];
                out.say(&if pinned {
                    tr!(
                        "cli-pin-done",
                        job = job.name.as_str(),
                        target = name.as_str(),
                        snapshot = short
                    )
                } else {
                    tr!(
                        "cli-unpin-done",
                        job = job.name.as_str(),
                        target = name.as_str(),
                        snapshot = short
                    )
                });
                return Ok(Outcome::Ok);
            }
            Err(ended) => last = ended,
        }
    }
    if last.1.is_empty() {
        out.warn(&tr!("cli-no-target", job = job.name.as_str()));
    } else {
        out.warn(&last.1);
    }
    Ok(last.0)
}

pub fn verify(env: &mut Env, job: &str, out: Output) -> Result<Outcome> {
    let job = env.job(job)?.clone();
    versioned(&job)?;
    let subset = job
        .after
        .iter()
        .find_map(|s| match s {
            AfterStep::Verify {
                read_data_subset, ..
            } => Some(read_data_subset.clone()),
            AfterStep::Command { .. } => None,
        })
        .unwrap_or_else(|| DEFAULT_READ_DATA_SUBSET.to_string());
    let mut outcomes = Vec::new();
    for target in &job.targets {
        let name = target.display_name();
        let checked = open(env, target).and_then(|(restic, repo)| {
            let _turn = take_turn(env, target, &job.name, out)?;
            restic
                .check(&repo, Some(&subset), &CancelToken::new())
                .map_err(|e| (Outcome::Failed, e.to_string()))
        });
        let outcome = match checked {
            Ok(summary) if summary.num_errors == 0 => {
                let _ = env.history.record_verify(&job.name, &name, Utc::now());
                out.say(&tr!(
                    "cli-verify-ok",
                    job = job.name.as_str(),
                    target = name.as_str()
                ));
                Outcome::Ok
            }
            Ok(summary) => {
                out.warn(&tr!(
                    "cli-verify-failed",
                    job = job.name.as_str(),
                    target = name.as_str(),
                    reason = tr!("error-verify-errors", count = summary.num_errors)
                ));
                Outcome::Failed
            }
            Err((outcome, reason)) => {
                out.warn(&format!("{name}: {reason}"));
                outcome
            }
        };
        outcomes.push(outcome);
    }
    Ok(worst(outcomes))
}

pub fn init(env: &Env, job: &str, out: Output) -> Result<Outcome> {
    let job = env.job(job)?;
    let mut outcomes = Vec::new();
    for target in &job.targets {
        let name = target.display_name();
        let location = match locate(env, target) {
            Ok(location) => location,
            Err((outcome, reason)) => {
                out.warn(&format!("{name}: {reason}"));
                outcomes.push(outcome);
                continue;
            }
        };
        let outcome = match job.mode {
            Mode::Mirror => match std::fs::create_dir_all(&location) {
                Ok(()) => {
                    out.say(&tr!(
                        "cli-init-folder-done",
                        job = job.name.as_str(),
                        target = name.as_str()
                    ));
                    Outcome::Ok
                }
                Err(e) => {
                    out.warn(&format!("{name}: {e}"));
                    Outcome::Failed
                }
            },
            Mode::Versioned => init_versioned(env, job, target, &location, out),
        };
        outcomes.push(outcome);
    }
    Ok(worst(outcomes))
}

fn init_versioned(env: &Env, job: &Job, target: &Target, location: &Path, out: Output) -> Outcome {
    let name = target.display_name();
    let restic = match &env.restic {
        Ok(restic) => restic,
        Err(e) => {
            out.warn(e);
            return Outcome::Failed;
        }
    };
    let _turn = match take_turn(env, target, &job.name, out) {
        Ok(turn) => turn,
        Err((outcome, reason)) => {
            out.warn(&format!("{name}: {reason}"));
            return outcome;
        }
    };
    if location.join("config").is_file() {
        out.say(&tr!(
            "cli-init-exists",
            job = job.name.as_str(),
            target = name.as_str()
        ));
        return Outcome::Ok;
    }
    // A password saved earlier (an init that stopped before restic) is used as it is.
    let secret = match keys::for_restic(target, &env.secrets) {
        Ok(Some(secret)) => secret,
        Ok(None) => match confirm_new_password(env, target, out) {
            Ok(secret) => secret,
            Err(outcome) => return outcome,
        },
        Err(e) => {
            out.warn(&e.to_string());
            return Outcome::Failed;
        }
    };
    let repo = Repository::new(location.to_string_lossy(), secret);
    match restic.init_target(&repo) {
        Ok(()) => {
            out.say(&tr!(
                "cli-init-done",
                job = job.name.as_str(),
                target = location.display().to_string()
            ));
            Outcome::Ok
        }
        Err(e) => {
            out.warn(&e.to_string());
            Outcome::Failed
        }
    }
}

/// Makes a new password, shows it once and stores it only once the user types the
/// confirmation word.
fn confirm_new_password(
    env: &Env,
    target: &Target,
    out: Output,
) -> std::result::Result<replica_core::platform::Secret, Outcome> {
    let new = keys::generate(target, &env.secrets).map_err(|e| {
        out.warn(&e.to_string());
        Outcome::Failed
    })?;
    let word = tr("cli-init-confirm-word");
    println!();
    println!("{}", tr("password-save-title"));
    println!();
    println!(
        "{}",
        tr!("password-save-body", target = target.display_name())
    );
    println!();
    println!("    {}", new.reveal());
    println!();
    println!("{}", tr!("cli-init-confirm", word = word.as_str()));
    let mut answer = String::new();
    let read = std::io::stdin().lock().read_line(&mut answer);
    if read.is_err() || !answer.trim().eq_ignore_ascii_case(&word) {
        out.warn(&tr("cli-init-not-confirmed"));
        return Err(Outcome::Failed);
    }
    new.confirm_saved(&env.secrets).map_err(|e| {
        out.warn(&e.to_string());
        Outcome::Failed
    })
}

pub fn key_show(env: &Env, job: &str, target: Option<&str>, out: Output) -> Result<Outcome> {
    let job = env.job(job)?;
    versioned(job)?;
    let mut outcomes = Vec::new();
    for t in chosen(job, target)? {
        match keys::show(t, &env.secrets) {
            Ok(secret) => {
                println!(
                    "{}",
                    tr!(
                        "cli-key-show",
                        job = job.name.as_str(),
                        target = t.display_name()
                    )
                );
                println!("    {}", secret.expose());
                outcomes.push(Outcome::Ok);
            }
            Err(e) => {
                out.warn(&e.to_string());
                outcomes.push(Outcome::Failed);
            }
        }
    }
    Ok(worst(outcomes))
}

/// Puts a saved repository password back, for each of the job's targets or the one named:
/// on a new PC, or after its entry was removed from Credential Manager.
pub fn key_set(
    env: &Env,
    job: &str,
    target: Option<&str>,
    force: bool,
    out: Output,
) -> Result<Outcome> {
    let job = env.job(job)?;
    versioned(job)?;
    let mut outcomes = Vec::new();
    for t in chosen(job, target)? {
        outcomes.push(key_set_one(env, job, t, force, out));
    }
    Ok(worst(outcomes))
}

fn key_set_one(env: &Env, job: &Job, target: &Target, force: bool, out: Output) -> Outcome {
    let name = target.display_name();
    let restic = match &env.restic {
        Ok(restic) => restic,
        Err(e) => {
            out.warn(e);
            return Outcome::Failed;
        }
    };
    let location = match locate(env, target) {
        Ok(location) => location,
        Err((outcome, reason)) => {
            out.warn(&format!("{name}: {reason}"));
            return outcome;
        }
    };
    if !location.join("config").is_file() {
        out.warn(
            &ResticError::RepoNotFound {
                location: location.display().to_string(),
            }
            .to_string(),
        );
        return Outcome::Failed;
    }
    let opens = |secret: &Secret| {
        restic.opens(&Repository::new(location.to_string_lossy(), secret.clone()))
    };
    // A saved password that opens the repository already: there's nothing to type.
    if !force
        && let Ok(Some(saved)) = keys::for_restic(target, &env.secrets)
        && opens(&saved).is_ok()
    {
        out.warn(&NotSaved::SavedWorks { target: name }.to_string());
        return Outcome::Failed;
    }
    let Some(entered) = ask_secret(&tr!("cli-key-set-prompt", target = name.as_str()), out) else {
        return Outcome::Failed;
    };
    match keys::save_entered(target, entered, &env.secrets, force, &opens) {
        Ok(entered) => {
            let key = match entered {
                Entered::Saved => "cli-key-set-saved",
                Entered::Replaced => "cli-key-set-replaced",
                Entered::Unchanged => "cli-key-set-unchanged",
            };
            out.say(&tr!(key, job = job.name.as_str(), target = name.as_str()));
            Outcome::Ok
        }
        Err(e) => {
            out.warn(&e.to_string());
            Outcome::Failed
        }
    }
}

/// Shows `prompt` and reads a line without echoing it; `None`, said why, if it can't be read.
fn ask_secret(prompt: &str, out: Output) -> Option<Secret> {
    print!("{prompt} ");
    let _ = std::io::stdout().flush();
    match read_secret_line() {
        Ok(secret) => Some(secret),
        Err(e) => {
            println!();
            out.warn(&tr!("cli-secret-not-read", detail = e.to_string()));
            None
        }
    }
}

// --- replica login set ---------------------------------------------------------------------------

/// Saves a share login under `name`, the `credential` a share target names in `settings.toml`.
pub fn login_set(env: &Env, name: &str, user: Option<&str>, out: Output) -> Result<Outcome> {
    let name = name.trim();
    if name.is_empty() {
        return Err(UsageError(tr("cli-login-name-empty")));
    }
    // Repository passwords live beside logins: a login can't take one's name.
    if name.to_lowercase().starts_with("replica:repo:") {
        return Err(UsageError(tr!("cli-login-name-reserved", name = name)));
    }
    let user = match user {
        Some(user) => user.trim().to_owned(),
        None => {
            print!("{} ", tr!("cli-login-user-prompt", name = name));
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if let Err(e) = std::io::stdin().lock().read_line(&mut line) {
                out.warn(&tr!("cli-secret-not-read", detail = e.to_string()));
                return Ok(Outcome::Failed);
            }
            line.trim().to_owned()
        }
    };
    let Some(password) = ask_secret(&tr("cli-login-password-prompt"), out) else {
        return Ok(Outcome::Failed);
    };
    let Some(again) = ask_secret(&tr("cli-login-password-again"), out) else {
        return Ok(Outcome::Failed);
    };
    if password.expose().is_empty() {
        out.warn(&tr("cli-login-password-empty"));
        return Ok(Outcome::Failed);
    }
    if password != again {
        out.warn(&tr("cli-login-password-mismatch"));
        return Ok(Outcome::Failed);
    }
    let existed = match env.secrets.get(name) {
        Ok(found) => found.is_some(),
        Err(e) => {
            out.warn(&e.to_string());
            return Ok(Outcome::Failed);
        }
    };
    let credential = Credential {
        username: (!user.is_empty()).then_some(user),
        secret: password,
    };
    if let Err(e) = env.secrets.set(name, &credential) {
        out.warn(&e.to_string());
        return Ok(Outcome::Failed);
    }
    tracing::info!(login = %name, replaced = existed, "stored a share login the user entered");
    out.say(&tr!(
        if existed {
            "cli-login-set-replaced"
        } else {
            "cli-login-set-saved"
        },
        name = name
    ));
    let used = env.settings.jobs.iter().flat_map(|j| &j.targets).any(
        |t| matches!(t, Target::Unc { credential: Some(c), .. } if c.eq_ignore_ascii_case(name)),
    );
    if !used {
        out.warn(&tr!("cli-login-set-unused", name = name));
    }
    Ok(Outcome::Ok)
}
