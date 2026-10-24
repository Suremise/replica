//! `settings.toml`: the app's settings and its jobs.
//!
//! - **Human-readable, every key documented:** [`Settings::to_documented_toml`] writes a comment
//!   above each key, and [`Settings::save`] writes that.
//! - **Forgiving:** a missing key takes its default. An unknown key is a [`Warning`], never an
//!   error.
//! - **Strict where it matters:** a file that isn't valid TOML, or whose values have the wrong
//!   type, is a [`SettingsError`], and so are jobs that don't validate ([`crate::job`]). The CLI
//!   reports either with exit code [`SettingsError::EXIT_CODE`].
//!
//! Where the file lives (installed or portable) is the platform's business, not this module's.

use std::fmt::{self, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::job::{self, AfterStep, BeforeStep, Job, Target, ValidationError};
use crate::tr;

/// The settings file's name.
pub const FILE_NAME: &str = "settings.toml";

/// Everything in `settings.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub app: AppSettings,
    pub jobs: Vec<Job>,
}

/// App-wide settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// What runs scheduled jobs.
    pub schedule_mode: ScheduleMode,
    /// Whether the tray app starts when the user logs on.
    pub start_with_windows: bool,
    /// Whether schedules are paused. Jobs still run by hand.
    pub pause_schedules: bool,
    /// A restic to use instead of the bundled one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restic_path: Option<PathBuf>,
    /// How many days of run logs to keep.
    pub log_retention_days: u32,
    /// How long a run waits, in minutes, for another job to finish with a backup repository
    /// they share, before it fails.
    pub repository_wait_minutes: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            schedule_mode: ScheduleMode::default(),
            start_with_windows: false,
            pause_schedules: false,
            restic_path: None,
            log_retention_days: 30,
            repository_wait_minutes: 120,
        }
    }
}

/// What runs scheduled jobs. The two never both run a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleMode {
    /// The tray app's built-in timer, while the tray app runs (the default).
    #[default]
    Timer,
    /// Windows Task Scheduler tasks, so jobs run even when the tray app isn't running. Off by
    /// default.
    TaskScheduler,
}

/// Settings that loaded, and anything worth warning about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub settings: Settings,
    pub warnings: Vec<Warning>,
}

/// Something in the file that was ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// A key Replica doesn't know, by its dotted path (`jobs[0].sources[1].excludes`).
    UnknownKey(String),
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::UnknownKey(key) => {
                f.write_str(&tr!("warning-settings-unknown-key", key = key.as_str()))
            }
        }
    }
}

/// Settings that can't be used.
#[derive(Debug)]
pub enum SettingsError {
    /// The file couldn't be read or written.
    Io { path: PathBuf, source: io::Error },
    /// The file isn't valid TOML, or a value has the wrong type.
    Corrupt {
        path: Option<PathBuf>,
        line: Option<usize>,
        column: Option<usize>,
        message: String,
    },
    /// The settings parse but don't validate.
    Invalid {
        path: Option<PathBuf>,
        errors: Vec<ValidationError>,
    },
}

impl SettingsError {
    /// The CLI's exit code for any settings error (a usage or settings error).
    pub const EXIT_CODE: i32 = 4;

    fn with_path(mut self, file: &Path) -> Self {
        match &mut self {
            SettingsError::Corrupt { path, .. } | SettingsError::Invalid { path, .. } => {
                *path = Some(file.to_path_buf())
            }
            SettingsError::Io { .. } => {}
        }
        self
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |p: &Option<PathBuf>| {
            p.as_ref()
                .map_or(FILE_NAME.to_string(), |p| p.display().to_string())
        };
        let text = match self {
            SettingsError::Io { path, source } => tr!(
                "error-settings-io",
                path = path.display().to_string(),
                detail = source.to_string()
            ),
            SettingsError::Corrupt {
                path,
                line: Some(line),
                column: Some(column),
                message,
            } => tr!(
                "error-settings-corrupt-at",
                path = name(path),
                line = *line,
                column = *column,
                detail = message.as_str()
            ),
            SettingsError::Corrupt { path, message, .. } => tr!(
                "error-settings-corrupt",
                path = name(path),
                detail = message.as_str()
            ),
            SettingsError::Invalid { path, errors } => {
                let mut text = tr!("error-settings-invalid", path = name(path));
                for e in errors {
                    let _ = write!(
                        text,
                        "
  - {e}"
                    );
                }
                text
            }
        };
        f.write_str(&text)
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SettingsError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Settings {
    /// Parses and validates settings text.
    pub fn parse(text: &str) -> Result<Loaded, SettingsError> {
        let settings: Settings = toml::from_str(text).map_err(|e| {
            let (line, column) = e.span().map(|s| line_col(text, s.start)).unzip();
            SettingsError::Corrupt {
                path: None,
                line,
                column,
                message: e.message().trim().to_string(),
            }
        })?;
        let errors = settings.validate();
        if !errors.is_empty() {
            return Err(SettingsError::Invalid { path: None, errors });
        }
        let mut warnings = Vec::new();
        // The text parsed as Settings, so it's a valid table, and Settings always serialises.
        let raw: toml::Table = toml::from_str(text).expect("parsed above");
        let known = toml::Table::try_from(&settings).expect("settings serialise");
        unknown_keys(&raw, &known, "", &mut warnings);
        Ok(Loaded { settings, warnings })
    }

    /// Loads and validates a settings file.
    pub fn load(path: &Path) -> Result<Loaded, SettingsError> {
        let text = fs::read_to_string(path).map_err(|source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|e| e.with_path(path))
    }

    /// Loads a settings file, or the defaults if there isn't one yet.
    pub fn load_or_default(path: &Path) -> Result<Loaded, SettingsError> {
        match fs::metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Loaded {
                settings: Settings::default(),
                warnings: Vec::new(),
            }),
            _ => Self::load(path),
        }
    }

    /// Every problem with the jobs.
    pub fn validate(&self) -> Vec<ValidationError> {
        job::validate_jobs(&self.jobs)
    }

    /// Writes the documented file, replacing the old one only once the new one is written.
    ///
    /// The text goes first into a temporary file of this call's own beside `path` ([`write_temp`]),
    /// so saves at once never share one, and a file of the user's there is never touched.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let io_err = |source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        };
        let tmp = write_temp(path, self.to_documented_toml().as_bytes()).map_err(io_err)?;
        replace(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io_err(e)
        })
    }

    /// The settings as TOML with a comment above every key.
    pub fn to_documented_toml(&self) -> String {
        let mut w = Writer::default();
        w.comment(
            "Replica settings. Every key is optional unless it says otherwise: a missing key takes",
        );
        w.comment(
            "the default shown. Replica rewrites this file when settings change in its window, so",
        );
        w.comment("comments you add are not kept.");
        w.blank();
        w.table("[app]");
        let app = &self.app;
        w.key(
            "What runs scheduled jobs: \"timer\" (the tray app's built-in timer, while it runs; the default)\nor \"task-scheduler\" (Windows Task Scheduler, so jobs run even when the tray app isn't running).",
            "schedule_mode",
            &str_value(match app.schedule_mode {
                ScheduleMode::Timer => "timer",
                ScheduleMode::TaskScheduler => "task-scheduler",
            }),
        );
        w.key(
            "Start the tray app when you log on to Windows (default false).",
            "start_with_windows",
            &app.start_with_windows.to_string(),
        );
        w.key(
            "Pause all schedules; jobs still run by hand (default false).",
            "pause_schedules",
            &app.pause_schedules.to_string(),
        );
        w.optional(
            "A restic.exe to use instead of the one bundled with Replica (default: the bundled one).",
            "restic_path",
            app.restic_path.as_ref().map(|p| path_value(p)),
            "'C:\\Tools\\restic.exe'",
        );
        w.key(
            "How many days of run logs to keep (default 30).",
            "log_retention_days",
            &app.log_retention_days.to_string(),
        );
        w.key(
            "How many minutes a job waits for another job to finish with a backup repository they share,\nbefore its run fails (default 120). Jobs that share a repository take turns.",
            "repository_wait_minutes",
            &app.repository_wait_minutes.to_string(),
        );

        w.blank();
        w.comment("Jobs. Each [[jobs]] table is one job; docs/settings.example.toml has examples.");
        if self.jobs.is_empty() {
            w.comment("There are no jobs yet.");
        }
        for job in &self.jobs {
            write_job(&mut w, job);
        }
        w.out
    }
}

/// Writes `text` to a new file beside `path` whose name no other file has (`<name>.<pid>-<n>.tmp`,
/// created exclusively), and returns its path. For a writer that then renames it to `path`:
/// [`Settings::save`] and `jobs_file::write`.
pub(crate) fn write_temp(path: &Path, text: &[u8]) -> io::Result<PathBuf> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let mut tmp = path.as_os_str().to_owned();
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        tmp.push(format!(".{}-{n}.tmp", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let written = file.write_all(text).and_then(|()| file.sync_all());
        drop(file);
        return match written {
            Ok(()) => Ok(tmp),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        };
    }
}

/// How many times [`replace`] tries the rename.
const REPLACE_TRIES: usize = 10;

/// Renames `tmp` over `path`, replacing it: for [`Settings::save`] and `jobs_file::write`.
///
/// Windows refuses a replacing rename for a moment (Access is denied) while another rename onto
/// the same file is under way, or something such as an antivirus scan holds it. So while
/// it's refused, it's tried again after a short, growing pause: [`REPLACE_TRIES`] tries within
/// about a second in all, then the last refusal is returned. Any other error is returned at once.
pub(crate) fn replace(tmp: &Path, path: &Path) -> io::Result<()> {
    replace_with(
        tmp,
        path,
        |from, to| fs::rename(from, to),
        std::thread::sleep,
    )
}

/// [`replace`] with its rename and its pause given, so tests can refuse and count.
fn replace_with(
    tmp: &Path,
    path: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    mut pause: impl FnMut(std::time::Duration),
) -> io::Result<()> {
    // 10, 20, 40, 80, then 160 ms: 950 ms over the nine pauses.
    let mut wait = std::time::Duration::from_millis(10);
    for _ in 1..REPLACE_TRIES {
        match rename(tmp, path) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                pause(wait);
                wait = (wait * 2).min(std::time::Duration::from_millis(160));
            }
            done => return done,
        }
    }
    rename(tmp, path)
}

pub(crate) fn write_job(w: &mut Writer, job: &Job) {
    w.blank();
    w.table("[[jobs]]");
    w.key(
        "Required. The job's name, used on the command line: letters, digits, '.', '_' and '-', unique.",
        "name",
        &str_value(&job.name),
    );
    w.key(
        "Whether it runs on its schedule and with run --all (default true).",
        "enabled",
        &job.enabled.to_string(),
    );
    w.key(
        "\"versioned\" (restic snapshots you can go back to; the default) or \"mirror\" (an exact copy:\ndeletions and mistakes are copied too).",
        "mode",
        &str_value(match job.mode {
            job::Mode::Versioned => "versioned",
            job::Mode::Mirror => "mirror",
        }),
    );
    w.key(
        "A mirror job must set this to true, to show you accept that it copies deletions (default false).",
        "mirror_deletes_confirmed",
        &job.mirror_deletes_confirmed.to_string(),
    );
    w.key(
        "A mirror run stops without deleting anything if it would delete more than this percentage of
the target's files, such as when a source was emptied by mistake; 100 allows any (default 30).",
        "mirror_delete_limit_percent",
        &job.mirror_delete_limit_percent.to_string(),
    );
    w.optional(
        "When it runs: \"daily 02:30\", \"weekly mon,thu 21:00\", \"every 6h\" or \"every 90m\". Without one,\nit runs only by hand.",
        "schedule",
        job.schedule.as_ref().map(|s| str_value(&s.to_string())),
        "\"daily 02:30\"",
    );
    w.key(
        "Notify when a run succeeds; a failure always notifies (default false).",
        "notify_on_success",
        &job.notify_on_success.to_string(),
    );

    w.blank();
    w.comment(
        "How many snapshots a versioned job keeps (default 7 daily, 4 weekly, 6 monthly), or all of them.",
    );
    w.table("[jobs.retention]");
    w.key(
        "Keep the last snapshot of each of this many days.",
        "daily",
        &job.retention.daily.to_string(),
    );
    w.key(
        "Keep the last snapshot of each of this many weeks.",
        "weekly",
        &job.retention.weekly.to_string(),
    );
    w.key(
        "Keep the last snapshot of each of this many months.",
        "monthly",
        &job.retention.monthly.to_string(),
    );
    w.key(
        "Keep every snapshot, forever: nothing is ever deleted and the backup grows over time; the counts\nabove are kept for when it's turned off (default false).",
        "keep_forever",
        &job.retention.keep_forever.to_string(),
    );

    for source in &job.sources {
        w.blank();
        w.comment("A folder or file to back up. A job needs at least one.");
        w.table("[[jobs.sources]]");
        w.key(
            "Required. An absolute path.",
            "path",
            &path_value(&source.path),
        );
        w.key(
            "Patterns to leave out, relative to path. A name (node_modules, *.tmp) matches at any depth;\na path (build/cache, **/.godot) from path's root. Default none.",
            "exclude",
            &array_value(&source.exclude),
        );
    }

    for target in &job.targets {
        w.blank();
        w.comment("Where to back up to. A job needs at least one; several run one after another.");
        w.table("[[jobs.targets]]");
        match target {
            Target::Drive { name, volume, path } => {
                w.key(
                    "Required. \"drive\" (a folder on a local or external drive) or \"unc\" (a folder on a NAS share).",
                    "kind",
                    &str_value("drive"),
                );
                w.optional(
                    "A name for results and the window (default: the volume and path).",
                    "name",
                    name.as_deref().map(str_value),
                    "\"External HDD\"",
                );
                w.optional(
                    "The drive's volume ID, so it's found whatever letter it gets. A drive needs a volume_id or a label.",
                    "volume_id",
                    volume.volume_id.as_deref().map(str_value),
                    "'\\\\?\\Volume{01234567-89ab-cdef-0123-456789abcdef}\\'",
                );
                w.optional(
                    "The drive's label, used when there's no volume_id or it doesn't match.",
                    "label",
                    volume.label.as_deref().map(str_value),
                    "\"BACKUP\"",
                );
                w.key(
                    "Required. The folder on the drive, relative to its root.",
                    "path",
                    &path_value(path),
                );
            }
            Target::Unc {
                name,
                path,
                credential,
            } => {
                w.key(
                    "Required. \"drive\" (a folder on a local or external drive) or \"unc\" (a folder on a NAS share).",
                    "kind",
                    &str_value("unc"),
                );
                w.optional(
                    "A name for results and the window (default: the path).",
                    "name",
                    name.as_deref().map(str_value),
                    "\"NAS\"",
                );
                w.key(
                    "Required. The UNC path: \\\\server\\share\\folder.",
                    "path",
                    &str_value(path),
                );
                w.optional(
                    "The name of the share's login in Windows Credential Manager. Never the password itself.",
                    "credential",
                    credential.as_deref().map(str_value),
                    "\"replica-nas\"",
                );
            }
        }
    }

    for step in &job.before {
        w.blank();
        w.comment("A step before the backup; if it fails, the job fails.");
        w.table("[[jobs.before]]");
        match step {
            BeforeStep::SqliteSnapshot { db } => {
                w.key(
                    "Required. \"sqlite-snapshot\" or \"command\".",
                    "type",
                    &str_value("sqlite-snapshot"),
                );
                w.key(
                    "Required. The live SQLite database. Replica copies it consistently into its own staging\nfolder and backs the copy up instead; the live file and its -wal, -shm and -journal are left out.",
                    "db",
                    &path_value(db),
                );
            }
            BeforeStep::Command { cmd, timeout_secs } => {
                w.key(
                    "Required. \"sqlite-snapshot\" or \"command\".",
                    "type",
                    &str_value("command"),
                );
                w.key(
                    "Required. The command line; a non-zero exit code fails the job.",
                    "cmd",
                    &str_value(cmd),
                );
                w.key(
                    "Seconds before it's stopped and the job fails (default 300).",
                    "timeout_secs",
                    &timeout_secs.to_string(),
                );
            }
        }
    }

    for step in &job.after {
        w.blank();
        w.comment("A step after the backup.");
        w.table("[[jobs.after]]");
        match step {
            AfterStep::Command { cmd, timeout_secs } => {
                w.key(
                    "Required. \"command\" or \"verify\".",
                    "type",
                    &str_value("command"),
                );
                w.key(
                    "Required. The command line; a non-zero exit code is reported.",
                    "cmd",
                    &str_value(cmd),
                );
                w.key(
                    "Seconds before it's stopped (default 300).",
                    "timeout_secs",
                    &timeout_secs.to_string(),
                );
            }
            AfterStep::Verify {
                schedule,
                read_data_subset,
            } => {
                w.key(
                    "Required. \"command\" or \"verify\" (restic check; versioned jobs only).",
                    "type",
                    &str_value("verify"),
                );
                w.optional(
                    "When to verify, in the job schedule's syntax (default: after every backup).",
                    "schedule",
                    schedule.as_ref().map(|s| str_value(&s.to_string())),
                    "\"weekly sun 03:00\"",
                );
                w.key(
                    "How much stored data to read back: \"5%\" (the default), \"1/10\" or a size like \"500M\".",
                    "read_data_subset",
                    &str_value(read_data_subset),
                );
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Writer {
    pub(crate) out: String,
}

impl Writer {
    pub(crate) fn comment(&mut self, text: &str) {
        for line in text.lines() {
            let _ = writeln!(self.out, "# {line}");
        }
    }

    pub(crate) fn blank(&mut self) {
        self.out.push('\n');
    }

    fn table(&mut self, header: &str) {
        let _ = writeln!(self.out, "{header}");
    }

    pub(crate) fn key(&mut self, doc: &str, key: &str, value: &str) {
        self.comment(doc);
        let _ = writeln!(self.out, "{key} = {value}");
    }

    /// A key that may be absent: written when set, else shown commented out with an example.
    fn optional(&mut self, doc: &str, key: &str, value: Option<String>, example: &str) {
        self.comment(doc);
        match value {
            Some(v) => {
                let _ = writeln!(self.out, "{key} = {v}");
            }
            None => {
                let _ = writeln!(self.out, "# {key} = {example}");
            }
        }
    }
}

/// A TOML string: a literal 'string' when that needs no escapes (readable Windows paths),
/// otherwise a basic "string".
pub(crate) fn str_value(s: &str) -> String {
    if s.contains('\\') && !s.contains(['\'', '\n', '\r']) && !s.chars().any(char::is_control) {
        return format!("'{s}'");
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn path_value(p: &Path) -> String {
    str_value(&p.to_string_lossy())
}

fn array_value(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|s| str_value(s)).collect();
    format!("[{}]", items.join(", "))
}

/// Keys in `raw` that the typed settings don't have, as warnings.
pub(crate) fn unknown_keys(
    raw: &toml::Table,
    known: &toml::Table,
    prefix: &str,
    out: &mut Vec<Warning>,
) {
    for (key, value) in raw {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match known.get(key) {
            None => out.push(Warning::UnknownKey(path)),
            Some(k) => unknown_in_value(value, k, &path, out),
        }
    }
}

fn unknown_in_value(raw: &toml::Value, known: &toml::Value, path: &str, out: &mut Vec<Warning>) {
    match (raw, known) {
        (toml::Value::Table(r), toml::Value::Table(k)) => unknown_keys(r, k, path, out),
        (toml::Value::Array(r), toml::Value::Array(k)) => {
            for (i, (rv, kv)) in r.iter().zip(k).enumerate() {
                unknown_in_value(rv, kv, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// A byte offset as a 1-based line and column.
pub(crate) fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Mode, Retention, Source, VolumeRef};
    use crate::schedule::Schedule;
    use std::sync::{Arc, Mutex, RwLock, mpsc};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    fn full() -> Settings {
        Settings {
            app: AppSettings {
                schedule_mode: ScheduleMode::TaskScheduler,
                start_with_windows: true,
                pause_schedules: true,
                restic_path: Some(r"C:\Tools\restic.exe".into()),
                log_retention_days: 14,
                repository_wait_minutes: 45,
            },
            jobs: vec![
                Job {
                    name: "hive".into(),
                    schedule: Some("daily 02:30".parse::<Schedule>().unwrap()),
                    notify_on_success: true,
                    retention: Retention {
                        daily: 3,
                        weekly: 2,
                        monthly: 1,
                        keep_forever: true,
                    },
                    sources: vec![
                        Source::with_common_excludes(r"D:\Work\Hive"),
                        Source {
                            path: r"C:\Users\Alex\it's here".into(),
                            exclude: vec![],
                        },
                    ],
                    targets: vec![
                        Target::Drive {
                            name: Some("External HDD".into()),
                            volume: VolumeRef {
                                volume_id: Some(r"\\?\Volume{1234}\".into()),
                                label: Some("BACKUP".into()),
                            },
                            path: r"Replica\Hive".into(),
                        },
                        Target::Unc {
                            name: None,
                            path: r"\\nas\backup\hive".into(),
                            credential: Some("replica-nas".into()),
                        },
                    ],
                    before: vec![
                        BeforeStep::SqliteSnapshot {
                            db: r"D:\Work\Hive\hive.db".into(),
                        },
                        BeforeStep::Command {
                            cmd: "echo \"before\"".into(),
                            timeout_secs: 60,
                        },
                    ],
                    after: vec![
                        AfterStep::Command {
                            cmd: "echo after".into(),
                            timeout_secs: 300,
                        },
                        AfterStep::Verify {
                            schedule: Some("weekly sun 03:00".parse().unwrap()),
                            read_data_subset: "1/10".into(),
                        },
                    ],
                    ..Job::default()
                },
                Job {
                    name: "photos-mirror".into(),
                    enabled: false,
                    mode: Mode::Mirror,
                    mirror_deletes_confirmed: true,
                    mirror_delete_limit_percent: 50,
                    sources: vec![Source {
                        path: r"D:\Photos".into(),
                        exclude: vec!["*.tmp".into()],
                    }],
                    targets: vec![Target::Drive {
                        name: None,
                        volume: VolumeRef {
                            volume_id: None,
                            label: Some("PHOTOS".into()),
                        },
                        path: "Mirror".into(),
                    }],
                    ..Job::default()
                },
            ],
        }
    }

    #[test]
    fn documented_file_round_trips() {
        for settings in [full(), Settings::default()] {
            let text = settings.to_documented_toml();
            let loaded = Settings::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
            assert_eq!(loaded.settings, settings, "{text}");
            assert_eq!(loaded.warnings, []);
        }
    }

    #[test]
    fn every_key_has_a_comment_above_it() {
        let text = full().to_documented_toml();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let is_key = !line.starts_with('#') && !line.starts_with('[') && line.contains(" = ");
            if is_key {
                assert!(
                    lines[i - 1].starts_with('#'),
                    "no comment above line {}: {line}",
                    i + 1
                );
            }
        }
    }

    #[test]
    fn unset_optional_keys_are_shown_commented_out() {
        let text = Settings::default().to_documented_toml();
        assert!(text.contains("# restic_path = "), "{text}");
    }

    #[test]
    fn missing_keys_take_defaults() {
        let loaded = Settings::parse(
            r#"
            [[jobs]]
            name = "hive"
            [[jobs.sources]]
            path = 'D:\Work'
            [[jobs.targets]]
            kind = "unc"
            path = '\\nas\backup'
            "#,
        )
        .unwrap();
        let job = &loaded.settings.jobs[0];
        assert_eq!(loaded.settings.app, AppSettings::default());
        assert_eq!(
            loaded.settings.app.schedule_mode,
            ScheduleMode::Timer,
            "Task Scheduler is off by default"
        );
        assert!(job.enabled);
        assert_eq!(job.mode, Mode::Versioned);
        assert_eq!(
            job.retention,
            Retention {
                daily: 7,
                weekly: 4,
                monthly: 6,
                keep_forever: false,
            },
            "a file from before keep_forever loads as it did"
        );
        assert_eq!(job.schedule, None);
        assert!(job.sources[0].exclude.is_empty());
        assert_eq!(Settings::parse("").unwrap().settings, Settings::default());
    }

    #[test]
    fn an_old_snapshot_steps_staging_warns_and_is_ignored() {
        let loaded = Settings::parse(
            r#"
            [[jobs]]
            name = "a"
            [[jobs.sources]]
            path = 'D:\Work'
            [[jobs.targets]]
            kind = "drive"
            label = "X"
            path = "b"
            [[jobs.before]]
            type = "sqlite-snapshot"
            db = 'D:\Work\app.db'
            staging = 'D:\Somewhere\else'
            "#,
        )
        .unwrap();
        assert_eq!(
            loaded.settings.jobs[0].before[0],
            BeforeStep::SqliteSnapshot {
                db: r"D:\Work\app.db".into()
            }
        );
        let warnings: Vec<String> = loaded.warnings.iter().map(|w| w.to_string()).collect();
        assert_eq!(
            warnings,
            ["Unknown setting \"jobs[0].before[0].staging\" ignored."]
        );
    }

    #[test]
    fn step_defaults() {
        let loaded = Settings::parse(
            r#"
            [[jobs]]
            name = "a"
            [[jobs.sources]]
            path = 'D:\Work'
            [[jobs.targets]]
            kind = "drive"
            label = "X"
            path = "b"
            [[jobs.before]]
            type = "command"
            cmd = "x"
            [[jobs.after]]
            type = "verify"
            "#,
        )
        .unwrap();
        let job = &loaded.settings.jobs[0];
        assert_eq!(
            job.before[0],
            BeforeStep::Command {
                cmd: "x".into(),
                timeout_secs: 300
            }
        );
        assert_eq!(
            job.after[0],
            AfterStep::Verify {
                schedule: None,
                read_data_subset: "5%".into()
            }
        );
    }

    #[test]
    fn unknown_keys_warn_and_load() {
        let loaded = Settings::parse(
            r#"
            colour = "blue"
            [app]
            pause_schedules = true
            tray_theme = "dark"
            [[jobs]]
            name = "hive"
            priority = 3
            [[jobs.sources]]
            path = 'D:\Work'
            excludes = ["typo"]
            [[jobs.targets]]
            kind = "unc"
            path = '\\nas\backup'
            port = 445
            [future]
            x = 1
            "#,
        )
        .unwrap();
        assert!(loaded.settings.app.pause_schedules);
        let keys: Vec<String> = loaded.warnings.iter().map(|w| w.to_string()).collect();
        assert_eq!(
            keys,
            [
                "Unknown setting \"app.tray_theme\" ignored.",
                "Unknown setting \"colour\" ignored.",
                "Unknown setting \"future\" ignored.",
                "Unknown setting \"jobs[0].priority\" ignored.",
                "Unknown setting \"jobs[0].sources[0].excludes\" ignored.",
                "Unknown setting \"jobs[0].targets[0].port\" ignored.",
            ]
        );
    }

    #[test]
    fn corrupt_files_are_clear_errors() {
        let err = Settings::parse("[app]\npause_schedules = yes\n").unwrap_err();
        match &err {
            SettingsError::Corrupt { line, column, .. } => {
                assert_eq!(*line, Some(2));
                assert_eq!(*column, Some(19));
            }
            other => panic!("{other:?}"),
        }
        assert!(
            err.to_string()
                .starts_with("The settings file settings.toml can't be read (line 2, column 19): "),
            "{err}"
        );

        let wrong_type = Settings::parse("[app]\nlog_retention_days = \"thirty\"\n").unwrap_err();
        assert!(
            matches!(wrong_type, SettingsError::Corrupt { .. }),
            "{wrong_type}"
        );
        let bad_kind =
            Settings::parse("[[jobs]]\nname='a'\n[[jobs.targets]]\nkind='ftp'\npath='x'\n")
                .unwrap_err();
        assert!(
            matches!(bad_kind, SettingsError::Corrupt { .. }),
            "{bad_kind}"
        );
        let bad_schedule =
            Settings::parse("[[jobs]]\nname='a'\nschedule='sometimes'\n").unwrap_err();
        assert!(
            bad_schedule
                .to_string()
                .contains("The schedule \"sometimes\" isn't valid"),
            "{bad_schedule}"
        );
        assert_eq!(SettingsError::EXIT_CODE, 4);
    }

    #[test]
    fn exclude_patterns_that_wont_build_are_refused_when_loading() {
        for pattern in ["[z-a]", "[!]", "[!a].txt", "{a,b}", "x]"] {
            let text = format!(
                "[[jobs]]\nname = 'a'\n[[jobs.sources]]\npath = 'D:\\W'\nexclude = ['{pattern}']\n\
                 [[jobs.targets]]\nkind = 'drive'\nlabel = 'B'\npath = 'x'\n"
            );
            let err = Settings::parse(&text).unwrap_err();
            let SettingsError::Invalid { errors, .. } = &err else {
                panic!("{pattern}: {err:?}")
            };
            assert!(
                matches!(
                    errors[..],
                    [ValidationError {
                        problem: job::Problem::BadExclude(_),
                        ..
                    }]
                ),
                "{pattern}: {err}"
            );
        }
    }

    #[test]
    fn invalid_jobs_are_errors() {
        let err = Settings::parse("[[jobs]]\nname = 'bad name'\n").unwrap_err();
        let SettingsError::Invalid { errors, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(errors.len(), 3, "{err}");
        assert!(
            err.to_string()
                .contains("the name \"bad name\" may only use"),
            "{err}"
        );
    }

    #[test]
    fn save_and_load_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(
            Settings::load_or_default(&path).unwrap().settings,
            Settings::default()
        );
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::Io { .. })
        ));
        full().save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().settings, full());
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temp file left"
        );

        fs::write(&path, "[app\n").unwrap();
        let err = Settings::load(&path).unwrap_err();
        assert!(
            err.to_string().starts_with(&format!(
                "The settings file {} can't be read",
                path.display()
            )),
            "{err}"
        );
    }

    /// [`replace_with`] with a rename that's refused `refusals` times with `error`, then
    /// succeeds. Returns the outcome, how many renames were tried, and the pauses.
    fn replace_refused(
        refusals: usize,
        error: io::ErrorKind,
    ) -> (io::Result<()>, usize, Vec<Duration>) {
        let (mut tries, mut pauses) = (0, Vec::new());
        let outcome = replace_with(
            Path::new("a.tmp"),
            Path::new("a"),
            |_, _| {
                tries += 1;
                if tries <= refusals {
                    Err(io::Error::from(error))
                } else {
                    Ok(())
                }
            },
            |wait| pauses.push(wait),
        );
        (outcome, tries, pauses)
    }

    #[test]
    fn a_refused_replace_is_tried_again_after_growing_pauses() {
        let (outcome, tries, pauses) = replace_refused(0, io::ErrorKind::PermissionDenied);
        assert!(outcome.is_ok());
        assert_eq!(
            (tries, pauses),
            (1, vec![]),
            "no pause when it isn't refused"
        );

        let (outcome, tries, pauses) = replace_refused(3, io::ErrorKind::PermissionDenied);
        assert!(outcome.is_ok());
        assert_eq!(tries, 4);
        let ms = |v: &[u64]| {
            v.iter()
                .map(|&m| Duration::from_millis(m))
                .collect::<Vec<_>>()
        };
        assert_eq!(pauses, ms(&[10, 20, 40]));

        // Refused on the last try, it's still replaced.
        let (outcome, tries, _) =
            replace_refused(REPLACE_TRIES - 1, io::ErrorKind::PermissionDenied);
        assert!(outcome.is_ok());
        assert_eq!(tries, REPLACE_TRIES);
    }

    #[test]
    fn a_replace_refused_every_time_gives_up_within_about_a_second() {
        let (outcome, tries, pauses) = replace_refused(usize::MAX, io::ErrorKind::PermissionDenied);
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(tries, REPLACE_TRIES);
        let ms = |v: &[u64]| {
            v.iter()
                .map(|&m| Duration::from_millis(m))
                .collect::<Vec<_>>()
        };
        assert_eq!(pauses, ms(&[10, 20, 40, 80, 160, 160, 160, 160, 160]));
        assert_eq!(pauses.iter().sum::<Duration>(), Duration::from_millis(950));
    }

    #[test]
    fn other_errors_of_a_replace_are_not_tried_again() {
        for error in [
            io::ErrorKind::NotFound,
            io::ErrorKind::AlreadyExists,
            io::ErrorKind::Other,
        ] {
            let (outcome, tries, pauses) = replace_refused(usize::MAX, error);
            assert_eq!(outcome.unwrap_err().kind(), error);
            assert_eq!((tries, pauses), (1, vec![]), "{error:?}");
        }
    }

    #[test]
    fn saving_leaves_other_files_beside_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let sibling = dir.path().join("settings.toml.tmp");
        fs::write(&sibling, "someone's document").unwrap();
        full().save(&path).unwrap();
        Settings::default().save(&path).unwrap();
        assert_eq!(fs::read_to_string(&sibling).unwrap(), "someone's document");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(Settings::load(&path).unwrap().settings, Settings::default());
    }

    /// How long saves at once may take before the test fails, for a hung run only.
    const BOUND: Duration = Duration::from_secs(60);

    type Save = Arc<dyn Fn(&Settings, &Path) -> Result<(), SettingsError> + Send + Sync>;

    /// Joins the writers however [`saves_at_once`] ends, a panic included, but not one that hung.
    struct Writers {
        handles: Vec<JoinHandle<()>>,
        hung: bool,
    }

    impl Drop for Writers {
        fn drop(&mut self) {
            if !self.hung {
                for handle in self.handles.drain(..) {
                    let _ = handle.join();
                }
            }
        }
    }

    /// Saves each of `all` to the settings file in `dir` with `save`, the writers started at
    /// once, and waits up to [`BOUND`] for every one to finish. Returns `dir`, and each writer's
    /// outcome in `all`'s order (one that panicked as `Err("panicked")`), once all have finished,
    /// whatever the outcomes. If one is still running at the bound, the test fails and keeps
    /// `dir`, so nothing deletes it while a writer may still hold a file there.
    fn saves_at_once(
        dir: tempfile::TempDir,
        all: &[Settings],
        save: Save,
    ) -> (tempfile::TempDir, Vec<Result<(), String>>) {
        let path = dir.path().join(FILE_NAME);
        let gate = Arc::new(RwLock::new(()));
        let (tx, rx) = mpsc::channel();
        let mut writers = Writers {
            handles: Vec::new(),
            hung: false,
        };
        // Held while they're started; dropped before `writers` however this ends, so none waits.
        let held = gate.write().unwrap();
        for (i, settings) in all.iter().cloned().enumerate() {
            let (gate, tx, save, path) = (
                Arc::clone(&gate),
                tx.clone(),
                Arc::clone(&save),
                path.to_path_buf(),
            );
            writers.handles.push(std::thread::spawn(move || {
                let _go = gate.read();
                let outcome = save(&settings, &path).map_err(|e| e.to_string());
                let _ = tx.send((i, outcome));
            }));
        }
        drop(tx);
        drop(held);
        let mut outcomes = vec![Err("panicked".to_string()); all.len()];
        let deadline = Instant::now() + BOUND;
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok((i, outcome)) => outcomes[i] = outcome,
                // Every writer has ended: sent its outcome, or panicked.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    writers.hung = true;
                    let kept = dir.keep();
                    panic!(
                        "a save is still running after {BOUND:?}; its folder {} is kept",
                        kept.display()
                    );
                }
            }
        }
        drop(writers);
        (dir, outcomes)
    }

    /// Six settings, each different.
    fn six() -> Vec<Settings> {
        (1..=6)
            .map(|days| {
                let mut s = full();
                s.app.log_retention_days = days;
                s
            })
            .collect()
    }

    /// The names in `dir` other than the settings file's.
    fn others(dir: &Path) -> Vec<std::ffi::OsString> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != FILE_NAME)
            .collect()
    }

    #[test]
    fn saves_at_once_each_leave_a_whole_file() {
        let all = six();
        for _ in 0..20 {
            let (dir, outcomes) = saves_at_once(
                tempfile::tempdir().unwrap(),
                &all,
                Arc::new(|s, p| s.save(p)),
            );
            let path = dir.path().join(FILE_NAME);
            assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
            let loaded = Settings::load(&path).unwrap_or_else(|e| panic!("{e}"));
            assert!(
                all.contains(&loaded.settings),
                "one save's whole settings, not a mix"
            );
            assert_eq!(others(dir.path()), [] as [std::ffi::OsString; 0]);
            dir.close().unwrap();
        }
    }

    #[test]
    fn the_saves_at_once_check_waits_for_every_writer_when_one_fails() {
        // Writer 1 fails and writer 2 panics; writer 3 saves only once writer 1 has failed, so
        // its outcome is there only if the check waited for it after the failure.
        let (failed_tx, failed_rx) = mpsc::channel::<()>();
        let (failed_tx, failed_rx) = (Mutex::new(failed_tx), Mutex::new(failed_rx));
        let save: Save = Arc::new(
            move |s: &Settings, p: &Path| match s.app.log_retention_days {
                1 => {
                    let _ = failed_tx.lock().unwrap().send(());
                    Err(SettingsError::Io {
                        path: p.to_path_buf(),
                        source: io::Error::other("refused here"),
                    })
                }
                2 => panic!("a writer panicked"),
                3 => {
                    failed_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(BOUND)
                        .expect("writer 1 failed");
                    s.save(p)
                }
                _ => s.save(p),
            },
        );
        let (dir, outcomes) = saves_at_once(tempfile::tempdir().unwrap(), &six(), save);
        assert!(outcomes[0].as_ref().unwrap_err().contains("refused here"));
        assert_eq!(outcomes[1], Err("panicked".to_string()));
        assert!(outcomes[2..].iter().all(Result::is_ok), "{outcomes:?}");
        assert_eq!(others(dir.path()), [] as [std::ffi::OsString; 0]);
        dir.close().unwrap();
    }

    #[test]
    fn example_file_loads() {
        let text = include_str!("../../../docs/settings.example.toml");
        let loaded = Settings::parse(text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(loaded.warnings, []);
        let modes: Vec<Mode> = loaded.settings.jobs.iter().map(|j| j.mode).collect();
        assert!(modes.contains(&Mode::Versioned) && modes.contains(&Mode::Mirror));
        assert_eq!(
            Settings::parse(&loaded.settings.to_documented_toml())
                .unwrap()
                .settings,
            loaded.settings
        );
    }
}
