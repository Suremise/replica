//! `replica tasks`: Replica's Windows Task Scheduler tasks, in full view ("nothing hidden").
//! The window's Scheduled tasks view shows the same.
//!
//! ```text
//! replica tasks list [--json]
//! replica tasks enable <task>
//! replica tasks disable <task>
//! replica tasks remove <task> | --all
//! ```

use clap::Subcommand;
use replica_core::i18n::tr;
use replica_core::run::Outcome;
use replica_core::tr;
use replica_win::scheduler::{ScheduledTask, Scheduler};
use serde::Serialize;

use super::{Env, Output, UsageError, json};

#[derive(Subcommand, Debug)]
pub enum TasksAction {
    List {
        #[arg(long)]
        json: bool,
    },
    Enable {
        task: String,
    },
    Disable {
        task: String,
    },
    Remove {
        task: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

/// The help for `replica tasks`, in the user's language.
pub fn localized(cmd: clap::Command) -> clap::Command {
    let arg =
        |cmd: clap::Command, name: &'static str, key: &str| cmd.mut_arg(name, |a| a.help(tr(key)));
    cmd.about(tr("cli-help-tasks"))
        .mut_subcommand("list", |c| {
            arg(
                c.about(tr("cli-help-tasks-list")),
                "json",
                "cli-help-arg-json",
            )
        })
        .mut_subcommand("enable", |c| {
            arg(
                c.about(tr("cli-help-tasks-enable")),
                "task",
                "cli-help-arg-task",
            )
        })
        .mut_subcommand("disable", |c| {
            arg(
                c.about(tr("cli-help-tasks-disable")),
                "task",
                "cli-help-arg-task",
            )
        })
        .mut_subcommand("remove", |c| {
            let c = arg(
                c.about(tr("cli-help-tasks-remove")),
                "task",
                "cli-help-arg-task",
            );
            arg(c, "all", "cli-help-arg-all-tasks")
        })
}

/// `replica tasks list --json`: see `docs/cli-json.md`.
#[derive(Debug, Serialize)]
struct TasksJson {
    schema: u32,
    folder: String,
    tasks: Vec<TaskJson>,
}

#[derive(Debug, Serialize)]
struct TaskJson {
    name: String,
    job: String,
    orphan: bool,
    schedule: Option<String>,
    enabled: bool,
    running: bool,
    /// Local time, `YYYY-MM-DDTHH:MM:SS`.
    next_run: Option<String>,
    last_run: Option<String>,
    last_result: Option<i32>,
}

fn local_time(t: Option<chrono::NaiveDateTime>) -> Option<String> {
    t.map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// The last result as people read it: Replica's own exit codes by name, others as a number.
fn result_text(code: Option<i32>) -> String {
    match code {
        None => tr("cli-tasks-never"),
        Some(0) => tr("result-ok"),
        Some(1) => tr("result-failed"),
        Some(2) => tr("result-partly"),
        Some(3) => tr("result-skipped"),
        Some(other) => format!("0x{other:X}"),
    }
}

pub fn run(env: &Env, action: TasksAction, out: Output) -> Result<Outcome, UsageError> {
    let scheduler = Scheduler::new();
    let jobs: Vec<&str> = env.settings.jobs.iter().map(|j| j.name.as_str()).collect();
    let failed = |e: replica_core::platform::PlatformError| UsageError(e.0);
    match action {
        TasksAction::List { json: as_json } => {
            let tasks = scheduler.list(&jobs).map_err(failed)?;
            if as_json {
                json::print(&TasksJson {
                    schema: json::SCHEMA,
                    folder: scheduler.folder().to_owned(),
                    tasks: tasks.iter().map(task_json).collect(),
                });
            } else {
                print_list(&tasks, out);
            }
        }
        TasksAction::Enable { task } => {
            scheduler.set_enabled(&task, true).map_err(failed)?;
            out.say(&tr!("cli-tasks-enabled-one", task = task.as_str()));
        }
        TasksAction::Disable { task } => {
            scheduler.set_enabled(&task, false).map_err(failed)?;
            out.say(&tr!("cli-tasks-disabled-one", task = task.as_str()));
        }
        TasksAction::Remove {
            task: None,
            all: true,
        } => {
            let count = scheduler.delete_all().map_err(failed)?;
            out.say(&tr!("cli-tasks-removed-all", count = count));
        }
        TasksAction::Remove {
            task: Some(task),
            all: false,
        } => {
            scheduler.delete(&task).map_err(failed)?;
            out.say(&tr!("cli-tasks-removed-one", task = task.as_str()));
        }
        TasksAction::Remove { .. } => return Err(UsageError(tr("cli-tasks-name-or-all"))),
    }
    Ok(Outcome::Ok)
}

fn task_json(t: &ScheduledTask) -> TaskJson {
    TaskJson {
        name: t.name.clone(),
        job: t.job.clone(),
        orphan: t.orphan,
        schedule: t.schedule.clone(),
        enabled: t.enabled,
        running: t.running,
        next_run: local_time(t.next_run),
        last_run: local_time(t.last_run),
        last_result: t.last_result,
    }
}

fn print_list(tasks: &[ScheduledTask], out: Output) {
    if tasks.is_empty() {
        out.say(&tr("cli-tasks-none"));
        return;
    }
    let yes_no = |b: bool| tr(if b { "cli-tasks-yes" } else { "cli-tasks-no" });
    let when = |t: Option<chrono::NaiveDateTime>| {
        t.map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| tr("cli-tasks-never"))
    };
    let header = [
        tr("cli-tasks-task"),
        tr("cli-tasks-job"),
        tr("cli-tasks-schedule"),
        tr("cli-tasks-next-run"),
        tr("cli-tasks-last-run"),
        tr("cli-tasks-last-result"),
        tr("cli-tasks-enabled"),
    ];
    let rows: Vec<[String; 7]> = tasks
        .iter()
        .map(|t| {
            let job = if t.orphan {
                tr!("cli-tasks-orphan", job = t.job.as_str())
            } else {
                t.job.clone()
            };
            let last = if t.running {
                tr("cli-tasks-running")
            } else {
                result_text(t.last_result)
            };
            [
                t.name.clone(),
                job,
                t.schedule.clone().unwrap_or_default(),
                when(t.next_run),
                when(t.last_run),
                last,
                yes_no(t.enabled),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..7)
        .map(|c| {
            rows.iter()
                .map(|r| r[c].chars().count())
                .chain([header[c].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String; 7]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    out.say(&line(&header));
    for row in &rows {
        out.say(&line(row));
    }
}
