//! `replica-tray.exe`: the tray app, the hidden `--run <job>` mode Task Scheduler uses, and the
//! two the installer uses.
//!
//! ```text
//! replica-tray                 the tray icon, its menu and the built-in timer (one instance),
//!                              with its window open: started by hand (Start menu, Explorer)
//! replica-tray --show          the same (the installer uses it after an interactive install)
//! replica-tray --background    the tray alone, window closed: what Start with Windows runs
//! replica-tray --run <job>     runs one job, notifies if it failed, and exits with its code
//! replica-tray --quit          asks a running tray to quit, and waits for it (before an
//!                              upgrade or uninstall replaces or removes the exe)
//! replica-tray --uninstall     that, then removes Replica's scheduled tasks and its Start with
//!                              Windows entry; settings, history and backups are never touched
//! ```
//!
//! Started again while a tray runs, it brings that tray's window forward and ends.
//!
//! It's a GUI program (`windows_subsystem = "windows"`), so neither mode ever shows a console.

mod app;
mod icons;
pub mod runner;
pub mod state;

use replica_core::i18n::{self, tr};
use replica_core::platform::{Level, Notification};
use replica_core::run::EXIT_USAGE;
use replica_core::tr;
use replica_win::logging::{self, LogConfig};
use replica_win::paths::AppPaths;

use runner::APP;

/// How long a notification from `--run` stays up before its icon goes.
const ALONE_LINGER: std::time::Duration = std::time::Duration::from_secs(10);
/// For tests: how long, in milliseconds, instead of [`ALONE_LINGER`].
pub const LINGER_VAR: &str = "REPLICA_NOTIFY_LINGER_MS";

fn linger() -> std::time::Duration {
    std::env::var(LINGER_VAR)
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map_or(ALONE_LINGER, std::time::Duration::from_millis)
}

/// What Start with Windows passes, so a login starts the tray without its window.
pub const BACKGROUND: &str = "--background";

/// What `replica-tray.exe` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// The tray, with its window open when `show`.
    Tray {
        show: bool,
    },
    /// `--run <job>`.
    Run(String),
    Quit,
    Uninstall,
}

impl Mode {
    /// The mode `args` (without the exe) ask for, or `None` for a usage error.
    pub fn parse(args: &[String]) -> Option<Mode> {
        Some(match args {
            [] => Mode::Tray { show: true },
            [flag] if flag == "--show" => Mode::Tray { show: true },
            [flag] if flag == BACKGROUND => Mode::Tray { show: false },
            [flag, job] if flag == "--run" => Mode::Run(job.clone()),
            [flag] if flag == "--quit" => Mode::Quit,
            [flag] if flag == "--uninstall" => Mode::Uninstall,
            _ => return None,
        })
    }
}

/// The Start with Windows entry called `value` (`Replica`): the tray, in the background. With
/// a test's own name ([`runner::test_tag`]) it's in that test's key, never the Run key.
pub fn autostart(value: &str) -> replica_win::autostart::AutoStart {
    let entry = replica_win::autostart::AutoStart::new(value).with_args(BACKGROUND);
    match runner::test_tag() {
        Some(tag) => entry.in_key(&replica_win::autostart::test_key(&tag)),
        None => entry,
    }
}

/// Runs `replica-tray.exe` and returns its exit code.
pub fn main() -> i32 {
    replica_win::tray::set_dpi_aware();
    i18n::init(
        &std::env::var("REPLICA_LANG")
            .unwrap_or_else(|_| replica_win::language::display_language()),
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Mode::parse(&args) {
        Some(Mode::Tray { show }) => app::run_tray(show),
        Some(Mode::Run(job)) => run_hidden(&job),
        Some(Mode::Quit) => quit_running(),
        Some(Mode::Uninstall) => uninstall(),
        None => {
            notify_alone(
                Level::Error,
                tr("tray-settings-error-title"),
                tr("cli-see-help"),
            );
            EXIT_USAGE
        }
    }
}

/// How long `--quit` waits for a running tray to end (it stops its job first).
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Asks the running tray to quit and waits for it: 0 once none runs, 1 if it's still running.
fn quit_running() -> i32 {
    match replica_win::tray::request_quit(&runner::instance_name(), QUIT_WAIT) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            tracing::warn!(%error, "couldn't ask the tray to quit");
            1
        }
    }
}

/// What the uninstaller runs before it removes the files: the tray quits, then Replica's tasks
/// and Start with Windows entry go (they'd point at an exe that's gone). Settings, history,
/// logs and backups are the user's and stay. It never fails the uninstall: problems are logged.
fn uninstall() -> i32 {
    let _ = quit_running();
    if let Err(error) = runner::scheduler().delete_all() {
        tracing::warn!(error = %error.0, "couldn't remove Replica's scheduled tasks");
    }
    let exe = std::env::current_exe().unwrap_or_default();
    if let Err(error) = autostart(APP).apply(false, &exe) {
        tracing::warn!(%error, "couldn't remove Start with Windows");
    }
    0
}

/// The app log, beside the settings (or none when that folder can't be written to), with
/// every panic in it.
pub(crate) fn start_logging(paths: &AppPaths) {
    let dir = paths.is_writable().then(|| paths.logs_dir());
    let _ = logging::init(&LogConfig::new(dir, "replica-tray"));
    logging::log_panics();
}

fn notify_alone(level: Level, title: String, body: String) {
    let note = Notification {
        level,
        job: None,
        title,
        body,
    };
    show(&note);
}

fn show(note: &Notification) {
    if let Err(error) =
        replica_win::notify::show_alone(icons::IDLE, &tr("tray-tooltip-idle"), note, linger())
    {
        tracing::error!(%error, "couldn't show a notification");
    }
}

/// `--run <job>`: what a Task Scheduler task runs. It runs the job (unless schedules are
/// paused, or the Task Scheduler option is off), notifies as the tray would, and exits with the
/// job's code (0 ok, 1 failed, 2 partly, 3 skipped, 4 a settings problem).
fn run_hidden(name: &str) -> i32 {
    let paths = match AppPaths::detect(APP) {
        Ok(p) => p,
        Err(error) => {
            notify_alone(
                Level::Error,
                tr("tray-settings-error-title"),
                crate::cli::paths_error(&error),
            );
            return EXIT_USAGE;
        }
    };
    start_logging(&paths);
    let settings = match runner::load_settings(&paths) {
        Ok(loaded) => loaded.settings,
        Err(error) => {
            notify_alone(
                Level::Error,
                tr("tray-settings-error-title"),
                error.to_string(),
            );
            return EXIT_USAGE;
        }
    };
    if settings.app.pause_schedules {
        tracing::info!(job = name, "schedules are paused: not running");
        return 0;
    }
    // A task left over after Task Scheduler was turned off mustn't keep running jobs (the
    // tray removes such tasks when it next loads the settings).
    if settings.app.schedule_mode != replica_core::settings::ScheduleMode::TaskScheduler {
        tracing::info!(job = name, "Task Scheduler is turned off: not running");
        return 0;
    }
    let Some(job) = settings
        .jobs
        .iter()
        .find(|j| j.name.eq_ignore_ascii_case(name))
    else {
        notify_alone(
            Level::Error,
            tr("tray-schedule-error-title"),
            tr!("error-job-not-found", job = name),
        );
        return EXIT_USAGE;
    };
    let mut history = match runner::open_history(&paths) {
        Ok(h) => h,
        Err(error) => {
            notify_alone(Level::Error, tr("tray-settings-error-title"), error);
            return 1;
        }
    };
    let cancel = replica_core::cancel::CancelToken::new();
    let report = runner::run_one(
        &paths,
        &settings,
        job,
        &mut history,
        &cancel,
        runner::QueueTurn::default(),
        &mut |_| {},
    );
    tracing::info!(job = %report.job, outcome = report.outcome.as_str(), "a scheduled run ended");
    if let Some(note) = state::notification_for(&report, Some(job)) {
        show(&note);
    }
    report.outcome.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Option<Mode> {
        Mode::parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn started_by_hand_or_by_the_installer_the_window_opens() {
        assert_eq!(parse(&[]), Some(Mode::Tray { show: true }));
        assert_eq!(parse(&["--show"]), Some(Mode::Tray { show: true }));
    }

    #[test]
    fn background_is_the_tray_alone() {
        assert_eq!(parse(&["--background"]), Some(Mode::Tray { show: false }));
    }

    #[test]
    fn the_other_modes_and_a_usage_error() {
        assert_eq!(parse(&["--run", "docs"]), Some(Mode::Run("docs".into())));
        assert_eq!(parse(&["--quit"]), Some(Mode::Quit));
        assert_eq!(parse(&["--uninstall"]), Some(Mode::Uninstall));
        assert_eq!(parse(&["--run"]), None);
        assert_eq!(parse(&["--background", "--show"]), None);
        assert_eq!(parse(&["--tray"]), None);
    }
}
