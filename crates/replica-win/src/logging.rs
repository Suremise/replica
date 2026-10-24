//! Logging with `tracing`: one app log file per process beside the settings, and cleanup of old
//! log files.
//!
//! Self-contained so other apps can copy it: nothing here is Replica's. Each process
//! writes its own file, `<prefix>-<start time>-<pid>.log` (the time in UTC, so names sort), and
//! never renames it, so processes running at once never touch each other's logs. At start-up the
//! oldest files beyond a count or an age are deleted; a file still open in another process can't
//! be deleted, so it's skipped, and cleanup never fails the app.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;

/// How the app logs.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// The logs folder, or `None` for no log file (a folder that can't be written to).
    pub dir: Option<PathBuf>,
    /// The start of the log files' names, one per exe (`replica`, `replica-tray`).
    pub prefix: String,
    pub level: LevelFilter,
    /// Also log to stderr (a console program run by hand).
    pub stderr: bool,
    /// Which of this prefix's old log files are kept.
    pub retention: Retention,
}

impl LogConfig {
    /// At `info`, without stderr, keeping 20 files and none older than 14 days.
    pub fn new(dir: Option<PathBuf>, prefix: &str) -> Self {
        Self {
            dir,
            prefix: prefix.to_owned(),
            level: LevelFilter::INFO,
            stderr: false,
            retention: Retention {
                keep: 20,
                max_age: Some(Duration::from_secs(14 * 24 * 60 * 60)),
            },
        }
    }
}

/// Sets up the global `tracing` subscriber, writing to a new log file for this process, then
/// cleans up old ones. Returns the new file's path. Fails if a subscriber is already set, or the
/// file can't be created (then nothing is set up, so the caller can retry without a file).
pub fn init(config: &LogConfig) -> io::Result<Option<PathBuf>> {
    let file = match &config.dir {
        Some(dir) => Some(create_log_file(dir, &config.prefix)?),
        None => None,
    };
    let path = file.as_ref().map(|(path, _)| path.clone());
    let file_layer = file.map(|(_, file)| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(file))
    });
    let stderr_layer = config
        .stderr
        .then(|| tracing_subscriber::fmt::layer().with_writer(io::stderr));
    tracing_subscriber::registry()
        .with(config.level)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(io::Error::other)?;
    if let Some(dir) = &config.dir {
        match prune_logs(dir, &config.prefix, config.retention) {
            Ok(deleted) => tracing::debug!(deleted, "cleaned up old log files"),
            Err(error) => tracing::warn!(%error, "couldn't clean up old log files"),
        }
    }
    Ok(path)
}

/// Logs every panic, on any thread, as an error with its thread and place, then does what
/// Rust would have (which writes to stderr: nowhere, in a GUI exe). Without this a panic on a
/// thread of a windowed app leaves no trace at all.
pub fn log_panics() {
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let place = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            place,
            message = panic_message(info.payload()),
            "panicked"
        );
        earlier(info);
    }));
}

/// A panic's message, from its payload (a `&str` or a `String`, as `panic!` makes them).
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".into())
}

/// Creates this process's log file in `dir` (creating `dir` if needed), named by the time now
/// and the process ID. While it's open, other processes can read and write it but not delete
/// or rename it, so their cleanup skips it. (A file opened the default way can be deleted while
/// it's open, and would vanish when the process closes it.)
pub fn create_log_file(dir: &Path, prefix: &str) -> io::Result<(PathBuf, File)> {
    fs::create_dir_all(dir)?;
    let name = log_file_name(prefix, SystemTime::now(), std::process::id());
    let stem = name.trim_end_matches(".log").to_owned();
    // The same name twice only if Windows reuses a PID within the second: then add a number.
    let mut n = 1;
    loop {
        let path = match n {
            1 => dir.join(&name),
            n => dir.join(format!("{stem}-{n}.log")),
        };
        match OpenOptions::new()
            .append(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// `<prefix>-YYYY-MM-DD-HHMMSS-<pid>.log`, with the time in UTC.
pub fn log_file_name(prefix: &str, time: SystemTime, pid: u32) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rest = secs % 86_400;
    let (hour, minute, second) = (rest / 3600, rest / 60 % 60, rest % 60);
    format!("{prefix}-{year:04}-{month:02}-{day:02}-{hour:02}{minute:02}{second:02}-{pid}.log")
}

/// The date of a day counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Whether `name` is one of `prefix`'s log files, `<prefix>-<digit>….log`, so `replica`
/// doesn't claim `replica-tray`'s files.
fn is_log_of(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|rest| {
            rest.starts_with(|c: char| c.is_ascii_digit()) && rest.ends_with(".log")
        })
}

/// How long log files are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// The newest this many files are kept.
    pub keep: usize,
    /// Files older than this go, even within `keep`.
    pub max_age: Option<Duration>,
}

/// Deletes `prefix`'s app log files in `dir` beyond `retention`. Other files, other prefixes'
/// logs and folders are left alone; a file that can't be deleted (open in another process) is
/// skipped. Returns how many were deleted.
pub fn prune_logs(dir: &Path, prefix: &str, retention: Retention) -> io::Result<usize> {
    prune_files(dir, retention, |name| is_log_of(name, prefix))
}

/// Deletes the files in `dir` beyond `retention`: for the per-run logs in `logs\runs\`. Folders
/// are left alone; a file that can't be deleted is skipped. Returns how many were deleted.
pub fn prune(dir: &Path, retention: Retention) -> io::Result<usize> {
    prune_files(dir, retention, |_| true)
}

fn prune_files(
    dir: &Path,
    retention: Retention,
    wanted: impl Fn(&str) -> bool,
) -> io::Result<usize> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() && entry.file_name().to_str().is_some_and(&wanted) {
            files.push((metadata.modified()?, entry.path()));
        }
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let now = SystemTime::now();
    let mut deleted = 0;
    for (n, (modified, path)) in files.iter().enumerate() {
        let too_old = retention
            .max_age
            .is_some_and(|age| now.duration_since(*modified).unwrap_or_default() > age);
        if (n >= retention.keep || too_old) && fs::remove_file(path).is_ok() {
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_s_message_from_either_payload() {
        let message = |f: fn()| panic_message(std::panic::catch_unwind(f).unwrap_err().as_ref());
        assert_eq!(message(|| panic!("plain")), "plain");
        assert_eq!(message(|| panic!("formatted {}", 1)), "formatted 1");
        assert_eq!(
            message(|| std::panic::panic_any(7)),
            "a panic without a message"
        );
    }

    #[test]
    fn names_by_utc_start_time_and_pid() {
        // 2026-10-21 18:00:00 UTC
        let time = UNIX_EPOCH + Duration::from_secs(1_792_605_600);
        assert_eq!(
            log_file_name("app", time, 1234),
            "app-2026-10-21-180000-1234.log"
        );
        assert_eq!(
            log_file_name("app", UNIX_EPOCH, 1),
            "app-1970-01-01-000000-1.log"
        );
        // 2000-02-29 23:59:59 UTC
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400 + 86_399);
        assert_eq!(log_file_name("x", leap, 7), "x-2000-02-29-235959-7.log");
    }

    #[test]
    fn a_prefix_doesnt_claim_a_longer_ones_files() {
        assert!(is_log_of("replica-2026-10-21-180000-1.log", "replica"));
        assert!(is_log_of("replica-2026-10-21-180000-1-2.log", "replica"));
        assert!(!is_log_of(
            "replica-tray-2026-10-21-180000-1.log",
            "replica"
        ));
        assert!(is_log_of(
            "replica-tray-2026-10-21-180000-1.log",
            "replica-tray"
        ));
        assert!(!is_log_of("replica.log", "replica"));
        assert!(!is_log_of("replica-2026.txt", "replica"));
    }
}
