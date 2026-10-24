//! `replica export` and `replica import`: jobs to and from a jobs file. The file, and
//! what importing does, are core's ([`replica_core::jobs_file`]); the window's Export… and
//! Import… use the same.
//!
//! ```text
//! replica export [<job>…] [--all] -o <file> [--force]
//! replica import <file> [--only <job>…] [--on-conflict skip|copy|replace]
//! ```

use std::path::Path;

use clap::ValueEnum;
use replica_core::i18n::tr;
use replica_core::job::Job;
use replica_core::jobs_file::{self, Action, Choice, FileError, OnConflict, Status};
use replica_core::run::Outcome;
use replica_core::tr;
use replica_win::files::rename_no_clobber;

use super::{Env, Output, UsageError};

type Result<T> = std::result::Result<T, UsageError>;

/// `--on-conflict`: what to do with a job whose name is a job's here already.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictArg {
    #[default]
    Skip,
    Copy,
    Replace,
}

impl From<ConflictArg> for OnConflict {
    fn from(arg: ConflictArg) -> Self {
        match arg {
            ConflictArg::Skip => OnConflict::Skip,
            ConflictArg::Copy => OnConflict::Copy,
            ConflictArg::Replace => OnConflict::Replace,
        }
    }
}

// --- replica export ----------------------------------------------------------------------------

pub fn export(
    env: &Env,
    names: &[String],
    all: bool,
    file: &Path,
    force: bool,
    out: Output,
) -> Result<Outcome> {
    let jobs: Vec<Job> = match (names.is_empty(), all) {
        (true, false) | (false, true) => return Err(UsageError(tr("cli-export-names-or-all"))),
        (true, true) => env.settings.jobs.clone(),
        (false, false) => {
            // In the settings' order, each once, however often it's named.
            for name in names {
                env.job(name)?;
            }
            env.settings
                .jobs
                .iter()
                .filter(|j| names.iter().any(|n| n.eq_ignore_ascii_case(&j.name)))
                .cloned()
                .collect()
        }
    };
    if jobs.is_empty() {
        return Err(UsageError(tr("cli-export-no-jobs")));
    }
    match jobs_file::write(file, &jobs, force, rename_no_clobber) {
        Ok(()) => {}
        Err(e @ FileError::Exists(_)) => return Err(UsageError(e.to_string())),
        Err(e) => {
            out.warn(&e.to_string());
            return Ok(Outcome::Failed);
        }
    }
    tracing::info!(file = %file.display(), jobs = jobs.len(), "exported jobs");
    out.say(&tr!(
        "cli-export-done",
        count = jobs.len(),
        path = file.display().to_string()
    ));
    Ok(Outcome::Ok)
}

// --- replica import ----------------------------------------------------------------------------

pub fn import(
    env: &Env,
    file: &Path,
    only: &[String],
    on_conflict: ConflictArg,
    out: Output,
) -> Result<Outcome> {
    let read = jobs_file::read(file).map_err(|e| UsageError(e.to_string()))?;
    for name in only {
        if !read
            .entries
            .iter()
            .any(|e| e.label.eq_ignore_ascii_case(name))
        {
            return Err(UsageError(tr!(
                "cli-import-not-in-file",
                job = name.as_str(),
                path = file.display().to_string()
            )));
        }
    }
    let choices: Vec<Choice> = read
        .entries
        .iter()
        .map(|e| Choice {
            import: only.is_empty() || only.iter().any(|n| n.eq_ignore_ascii_case(&e.label)),
            on_conflict: on_conflict.into(),
        })
        .collect();
    let plan = jobs_file::plan(&read, &env.settings, &choices, &env.secrets, &env.targets);
    for warning in &plan.warnings {
        out.warn(warning);
    }
    out.say(&tr!(
        "cli-import-from",
        path = file.display().to_string(),
        count = read.entries.len()
    ));
    let (mut imported, mut skipped, mut refused) = (0, 0, 0);
    for p in &plan.jobs {
        let job = p.label.as_str();
        let existing = match &p.status {
            Status::NameExists(name) => name.as_str(),
            _ => "",
        };
        match &p.action {
            Action::NotChosen => continue,
            Action::Add(name) if name.eq_ignore_ascii_case(job) => {
                out.say(&tr!("cli-import-new", job = job));
            }
            Action::Add(name) => out.say(&tr!(
                "cli-import-copy",
                job = job,
                name = name.as_str(),
                existing = existing
            )),
            Action::Replace(name) => {
                out.say(&tr!("cli-import-replace", job = job, name = name.as_str()))
            }
            Action::Skip => {
                skipped += 1;
                out.say(&tr!("cli-import-skip", job = job, existing = existing));
                continue;
            }
            Action::Refused => {
                refused += 1;
                out.warn(&tr!("cli-import-refused", job = job));
                if let Status::Invalid(reasons) = &p.status {
                    for reason in reasons {
                        out.warn(&format!("  - {reason}"));
                    }
                }
                continue;
            }
        }
        imported += 1;
        let name = match &p.action {
            Action::Add(n) | Action::Replace(n) => n.as_str(),
            _ => job,
        };
        for target in &p.needs_password {
            out.say(&tr!(
                "cli-import-needs-password",
                job = name,
                target = target.as_str()
            ));
        }
        for target in &p.drive_not_found {
            out.say(&tr!("cli-import-drive-not-found", target = target.as_str()));
        }
        for cmd in &p.commands {
            out.say(&tr!("cli-import-runs", cmd = cmd.as_str()));
        }
        for warning in &p.warnings {
            out.warn(&format!("  {warning}"));
        }
    }
    if plan.changes_anything() {
        let path = env.paths.settings_file();
        if let Err(e) = plan.settings.save(&path) {
            out.warn(&e.to_string());
            out.warn(&tr("cli-import-not-saved"));
            return Ok(Outcome::Failed);
        }
        tracing::info!(file = %file.display(), imported, skipped, refused, "imported jobs");
    }
    out.say(&tr!(
        "cli-import-summary",
        imported = imported,
        skipped = skipped,
        refused = refused
    ));
    if imported > 0 {
        out.say(&tr("cli-import-turned-off"));
    }
    Ok(match (imported, refused) {
        (_, 0) => Outcome::Ok,
        (0, _) => Outcome::Failed,
        _ => Outcome::Partly,
    })
}
