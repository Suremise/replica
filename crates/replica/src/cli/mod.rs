//! `replica.exe`: the command line.
//!
//! ```text
//! replica run <job>|--all [--quiet] [--json]
//! replica list [--json]
//! replica status [--json]
//! replica snapshots <job> [--json]
//! replica restore <job> <snapshot> <path> --to <folder> [--target <name>] [--overwrite]
//! replica verify <job>
//! replica init <job>
//! replica key show <job> [--target <name>]
//! replica key set <job> [--target <name>] [--force]
//! replica login set <name> [--user <user>]
//! replica export [<job>…] [--all] -o <file> [--force]
//! replica import <file> [--only <job>…] [--on-conflict skip|copy|replace]
//! ```
//!
//! **Exit codes:** 0 ok, 1 failed, 2 partly done, 3 skipped (a target isn't available), 4 a
//! usage or settings error. `--json` output is described in `docs/cli-json.md`; its keys are
//! never translated. Every other word comes from Fluent (`locales/en/cli.ftl`).

mod commands;
mod jobs_file;
mod json;
mod tasks;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use replica_core::history::History;
use replica_core::i18n::{self, tr};
use replica_core::restic::{Restic, ResticSearch};
use replica_core::run::{EXIT_USAGE, Outcome};
use replica_core::settings::Settings;
use replica_core::tr;
use replica_win::credentials::CredentialManager;
use replica_win::jobs::JobObjectTree;
use replica_win::paths::{AppPaths, PathsError};
use replica_win::targets::WindowsTargets;

/// For tests: a Credential Manager name prefix of their own, so they never touch Replica's
/// real entries (`Replica/…`).
pub const CREDENTIAL_PREFIX_VAR: &str = "REPLICA_CREDENTIAL_PREFIX";

#[derive(Parser, Debug)]
#[command(name = "replica", version = replica_core::VERSION)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Run {
        job: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Snapshots {
        job: String,
        #[arg(long)]
        json: bool,
    },
    Restore {
        job: String,
        snapshot: String,
        path: PathBuf,
        #[arg(long)]
        to: PathBuf,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        overwrite: bool,
    },
    Verify {
        job: String,
    },
    /// Keeps a snapshot whatever the job's retention.
    Pin {
        job: String,
        snapshot: String,
        #[arg(long)]
        target: Option<String>,
    },
    /// Lets the job's retention delete a pinned snapshot again.
    Unpin {
        job: String,
        snapshot: String,
        #[arg(long)]
        target: Option<String>,
    },
    Init {
        job: String,
    },
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    Login {
        #[command(subcommand)]
        action: LoginAction,
    },
    /// Jobs to a jobs file (`cli/jobs_file.rs`).
    Export {
        names: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(short = 'o', long = "output")]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Jobs from a jobs file.
    Import {
        file: PathBuf,
        #[arg(long, num_args = 1..)]
        only: Vec<String>,
        #[arg(long, value_enum, default_value_t)]
        on_conflict: jobs_file::ConflictArg,
    },
    /// Replica's Task Scheduler tasks (`cli/tasks.rs`).
    Tasks {
        #[command(subcommand)]
        action: tasks::TasksAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum KeyAction {
    Show {
        job: String,
        #[arg(long)]
        target: Option<String>,
    },
    Set {
        job: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum LoginAction {
    Set {
        name: String,
        #[arg(long)]
        user: Option<String>,
    },
}

/// The command line with its help in the user's language.
fn localized() -> clap::Command {
    let arg =
        |cmd: clap::Command, name: &'static str, key: &str| cmd.mut_arg(name, |a| a.help(tr(key)));
    Cli::command()
        .about(tr("cli-help-about"))
        .after_help(tr("cli-help-exit-codes"))
        .mut_subcommand("run", |c| {
            let c = c.about(tr("cli-help-run"));
            let c = arg(c, "job", "cli-help-arg-job");
            let c = arg(c, "all", "cli-help-arg-all");
            let c = arg(c, "quiet", "cli-help-arg-quiet");
            arg(c, "json", "cli-help-arg-json")
        })
        .mut_subcommand("list", |c| {
            arg(c.about(tr("cli-help-list")), "json", "cli-help-arg-json")
        })
        .mut_subcommand("status", |c| {
            arg(c.about(tr("cli-help-status")), "json", "cli-help-arg-json")
        })
        .mut_subcommand("snapshots", |c| {
            let c = arg(c.about(tr("cli-help-snapshots")), "job", "cli-help-arg-job");
            arg(c, "json", "cli-help-arg-json")
        })
        .mut_subcommand("restore", |c| {
            let c = c.about(tr("cli-help-restore"));
            let c = arg(c, "job", "cli-help-arg-job");
            let c = arg(c, "snapshot", "cli-help-arg-snapshot");
            let c = arg(c, "path", "cli-help-arg-path");
            let c = arg(c, "to", "cli-help-arg-to");
            let c = arg(c, "target", "cli-help-arg-target");
            arg(c, "overwrite", "cli-help-arg-overwrite")
        })
        .mut_subcommand("verify", |c| {
            arg(c.about(tr("cli-help-verify")), "job", "cli-help-arg-job")
        })
        .mut_subcommand("pin", |c| {
            let c = c.about(tr("cli-help-pin"));
            let c = arg(c, "job", "cli-help-arg-job");
            let c = arg(c, "snapshot", "cli-help-arg-snapshot");
            arg(c, "target", "cli-help-arg-target")
        })
        .mut_subcommand("unpin", |c| {
            let c = c.about(tr("cli-help-unpin"));
            let c = arg(c, "job", "cli-help-arg-job");
            let c = arg(c, "snapshot", "cli-help-arg-snapshot");
            arg(c, "target", "cli-help-arg-target")
        })
        .mut_subcommand("init", |c| {
            arg(c.about(tr("cli-help-init")), "job", "cli-help-arg-job")
        })
        .mut_subcommand("key", |c| {
            c.about(tr("cli-help-key"))
                .mut_subcommand("show", |s| {
                    let s = arg(s.about(tr("cli-help-key-show")), "job", "cli-help-arg-job");
                    arg(s, "target", "cli-help-arg-target")
                })
                .mut_subcommand("set", |s| {
                    let s = arg(s.about(tr("cli-help-key-set")), "job", "cli-help-arg-job");
                    let s = arg(s, "target", "cli-help-arg-target");
                    arg(s, "force", "cli-help-arg-force")
                })
        })
        .mut_subcommand("login", |c| {
            c.about(tr("cli-help-login")).mut_subcommand("set", |s| {
                let s = arg(
                    s.about(tr("cli-help-login-set")),
                    "name",
                    "cli-help-arg-login",
                );
                arg(s, "user", "cli-help-arg-user")
            })
        })
        .mut_subcommand("export", |c| {
            let c = arg(
                c.about(tr("cli-help-export")),
                "names",
                "cli-help-arg-export-jobs",
            );
            let c = arg(c, "all", "cli-help-arg-export-all");
            let c = arg(c, "output", "cli-help-arg-output");
            arg(c, "force", "cli-help-arg-replace-file")
        })
        .mut_subcommand("import", |c| {
            let c = arg(
                c.about(tr("cli-help-import")),
                "file",
                "cli-help-arg-import-file",
            );
            let c = arg(c, "only", "cli-help-arg-only");
            arg(c, "on_conflict", "cli-help-arg-on-conflict")
        })
        .mut_subcommand("tasks", tasks::localized)
}

/// How the command was asked to talk.
#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub quiet: bool,
    pub json: bool,
    /// stdout is a terminal: progress lines are shown.
    pub terminal: bool,
}

impl Output {
    /// A line for the user, unless `--quiet` or `--json`.
    pub fn say(&self, text: &str) {
        if !self.quiet && !self.json {
            println!("{text}");
        }
    }

    /// A problem: always to stderr, `--quiet` or not.
    pub fn warn(&self, text: &str) {
        eprintln!("{text}");
    }

    /// A progress line, overwritten by the next, on a terminal only.
    pub fn progress(&self, text: &str) {
        if self.terminal && !self.quiet && !self.json {
            print!("\r{text:<78.78}");
            let _ = std::io::stdout().flush();
        }
    }

    /// Clears a progress line.
    pub fn end_progress(&self) {
        if self.terminal && !self.quiet && !self.json {
            print!("\r{:78}\r", "");
            let _ = std::io::stdout().flush();
        }
    }
}

/// What every command works with.
pub struct Env {
    pub paths: AppPaths,
    pub settings: Settings,
    pub history: History,
    pub secrets: CredentialManager,
    pub targets: WindowsTargets,
    /// restic, or why it can't be used (only versioned targets need it).
    pub restic: Result<Restic, String>,
}

/// A usage or settings error: exit code 4.
pub struct UsageError(pub String);

/// Why the app's folders couldn't be worked out, in the user's words (`AppPaths` is generic
/// and leaves the wording to the app). The CLI and `replica-tray --run` both show it.
pub fn paths_error(e: &PathsError) -> String {
    match e {
        PathsError::NoExe(e) => tr!("error-paths-no-exe", detail = e.to_string()),
        PathsError::NoAppData => tr("error-paths-no-appdata"),
    }
}

impl Env {
    fn open() -> Result<Env, UsageError> {
        let paths = AppPaths::detect("Replica").map_err(|e| UsageError(paths_error(&e)))?;
        let loaded = Settings::load_or_default(&paths.settings_file())
            .map_err(|e| UsageError(e.to_string()))?;
        for warning in &loaded.warnings {
            eprintln!("{warning}");
        }
        let history =
            History::open(&paths.file("history.db")).map_err(|e| UsageError(e.to_string()))?;
        let secrets = match std::env::var(CREDENTIAL_PREFIX_VAR) {
            Ok(prefix) if !prefix.is_empty() => CredentialManager::with_prefix(&prefix),
            _ => CredentialManager::new(),
        };
        let restic = ResticSearch::for_this_exe(loaded.settings.app.restic_path.clone())
            .find()
            .and_then(Restic::new)
            .map(|r| r.with_tree(Arc::new(JobObjectTree)))
            .map_err(|e| e.to_string());
        Ok(Env {
            paths,
            settings: loaded.settings,
            history,
            secrets,
            targets: WindowsTargets::new(),
            restic,
        })
    }

    /// The job called `name` (ignoring case, as job names are unique that way).
    pub fn job(&self, name: &str) -> Result<&replica_core::job::Job, UsageError> {
        self.settings
            .jobs
            .iter()
            .find(|j| j.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| UsageError(tr!("error-job-not-found", job = name)))
    }
}

/// Runs the command line and returns the exit code.
pub fn main() -> i32 {
    i18n::init(&ui_language());
    let matches = match localized().try_get_matches() {
        Ok(m) => m,
        Err(e) => {
            // --help and --version are answers, not errors.
            let code = if e.use_stderr() { EXIT_USAGE } else { 0 };
            let _ = e.print();
            if code != 0 {
                eprintln!("{}", tr("cli-see-help"));
            }
            return code;
        }
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return EXIT_USAGE;
        }
    };
    let mut env = match Env::open() {
        Ok(env) => env,
        Err(UsageError(text)) => {
            eprintln!("{}", tr!("cli-usage-error", detail = text));
            return EXIT_USAGE;
        }
    };
    let terminal = std::io::stdout().is_terminal();
    let result = match cli.command {
        Command::Run {
            job,
            all,
            quiet,
            json,
        } => {
            let out = Output {
                quiet,
                json,
                terminal,
            };
            commands::run(&mut env, job.as_deref(), all, out)
        }
        Command::List { json } => commands::list(
            &env,
            false,
            Output {
                quiet: false,
                json,
                terminal,
            },
        ),
        Command::Status { json } => commands::list(
            &env,
            true,
            Output {
                quiet: false,
                json,
                terminal,
            },
        ),
        Command::Snapshots { job, json } => commands::snapshots(
            &env,
            &job,
            Output {
                quiet: false,
                json,
                terminal,
            },
        ),
        Command::Restore {
            job,
            snapshot,
            path,
            to,
            target,
            overwrite,
        } => commands::restore(
            &env,
            &job,
            &snapshot,
            &path,
            &to,
            target.as_deref(),
            overwrite,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Verify { job } => commands::verify(
            &mut env,
            &job,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Pin {
            job,
            snapshot,
            target,
        } => commands::pin(
            &env,
            &job,
            &snapshot,
            target.as_deref(),
            true,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Unpin {
            job,
            snapshot,
            target,
        } => commands::pin(
            &env,
            &job,
            &snapshot,
            target.as_deref(),
            false,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Init { job } => commands::init(
            &env,
            &job,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Key {
            action: KeyAction::Show { job, target },
        } => commands::key_show(
            &env,
            &job,
            target.as_deref(),
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Key {
            action: KeyAction::Set { job, target, force },
        } => commands::key_set(
            &env,
            &job,
            target.as_deref(),
            force,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Login {
            action: LoginAction::Set { name, user },
        } => commands::login_set(
            &env,
            &name,
            user.as_deref(),
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Export {
            names,
            all,
            output,
            force,
        } => jobs_file::export(
            &env,
            &names,
            all,
            &output,
            force,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Import {
            file,
            only,
            on_conflict,
        } => jobs_file::import(
            &env,
            &file,
            &only,
            on_conflict,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
        Command::Tasks { action } => tasks::run(
            &env,
            action,
            Output {
                quiet: false,
                json: false,
                terminal,
            },
        ),
    };
    match result {
        Ok(outcome) => outcome.exit_code(),
        Err(UsageError(text)) => {
            eprintln!("{}", tr!("cli-usage-error", detail = text));
            EXIT_USAGE
        }
    }
}

/// The language for messages: `REPLICA_LANG` if set, else the Windows display language.
fn ui_language() -> String {
    std::env::var("REPLICA_LANG").unwrap_or_else(|_| replica_win::language::display_language())
}

/// A duration as people read it: "42 s", "2 min 5 s".
pub fn duration_text(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        tr!(
            "cli-duration-seconds",
            s = format!("{:.1}", d.as_secs_f64())
        )
    } else {
        tr!("cli-duration-minutes", min = secs / 60, s = secs % 60)
    }
}

/// The worse of two outcomes, starting from ok.
pub fn worst(outcomes: impl IntoIterator<Item = Outcome>) -> Outcome {
    outcomes.into_iter().fold(Outcome::Ok, Outcome::worst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_s_folder_errors_are_worded_from_fluent() {
        assert_eq!(
            paths_error(&PathsError::NoAppData),
            tr("error-paths-no-appdata")
        );
        let no_exe = paths_error(&PathsError::NoExe(std::io::Error::other("detail here")));
        assert_eq!(
            no_exe,
            tr!("error-paths-no-exe", detail = "detail here".to_string())
        );
        // Not the generic module's own English.
        assert!(!no_exe.contains("isn't known"));
    }

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
        localized().debug_assert();
    }

    #[test]
    fn help_is_translated_and_lists_the_exit_codes() {
        let help = localized().render_long_help().to_string();
        assert!(help.contains(&tr("cli-help-about")), "{help}");
        assert!(help.contains("4"), "{help}");
    }
}
