//! Jobs files: jobs exported from one PC and imported on another, or kept as a copy.
//!
//! - **The file** is TOML in `settings.toml`'s `[[jobs]]` shape, written by the same writer,
//!   under a header: `format = "replica-jobs"`, `version` and the Replica version that wrote it.
//!   It holds only jobs, never the app's settings, history, tasks, pins or locks.
//! - **No secrets, ever.** A job holds none: a share's login and a repository's password live in
//!   the secret store, and the job names the login at most (as `settings.toml` does). A file that
//!   carries other keys has them ignored, with a warning: nothing is read from them.
//! - **Importing** is a [`Plan`]: each job in the file, what it is on this PC ([`Status`]) and
//!   what importing it does ([`Action`]), with the settings that result. The CLI prints it and
//!   saves it; the window shows it as the dialog's preview and saves it when asked. Every job
//!   imported arrives disabled, by design, so nothing runs, and no task is made
//!   for it, until the user turns it on. A job that doesn't validate, alone or beside the jobs
//!   already here (overlapping targets, say), is never imported.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::i18n::tr;
use crate::job::{self, AfterStep, BeforeStep, Job, MAX_NAME_LEN, Mode, Target};
use crate::platform::{SecretStore, TargetResolver};
use crate::restic::password_secret_name;
use crate::settings::{self, Settings, Warning, Writer};
use crate::tr;

/// The `format` a jobs file declares.
pub const FORMAT: &str = "replica-jobs";
/// The newest `version` this Replica reads, and the one it writes.
pub const VERSION: i64 = 1;
/// The extension suggested for jobs files.
pub const EXTENSION: &str = ".replica-jobs.toml";

/// A jobs file that can't be written or read.
#[derive(Debug)]
pub enum FileError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    /// There's a file there already, and overwriting wasn't asked for.
    Exists(PathBuf),
    /// Not valid TOML.
    Corrupt {
        line: Option<usize>,
        column: Option<usize>,
        message: String,
    },
    /// Valid TOML, but not a jobs file (another `format`, or none, such as `settings.toml`).
    NotJobsFile,
    /// A jobs file from a newer Replica.
    NewerVersion(i64),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            FileError::Io { path, source } => tr!(
                "error-jobs-file-io",
                path = path.display().to_string(),
                detail = source.to_string()
            ),
            FileError::Exists(path) => {
                tr!("error-jobs-file-exists", path = path.display().to_string())
            }
            FileError::Corrupt {
                line: Some(line),
                column: Some(column),
                message,
            } => tr!(
                "error-jobs-file-corrupt-at",
                line = *line,
                column = *column,
                detail = message.as_str()
            ),
            FileError::Corrupt { message, .. } => {
                tr!("error-jobs-file-corrupt", detail = message.as_str())
            }
            FileError::NotJobsFile => tr!("error-jobs-file-not-jobs", format = FORMAT),
            FileError::NewerVersion(version) => tr!(
                "error-jobs-file-newer",
                version = *version,
                supported = VERSION
            ),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FileError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The file's text for `jobs`: the header, then each job as `settings.toml` writes it.
pub fn to_toml(jobs: &[Job]) -> String {
    let mut w = Writer::default();
    w.comment("Replica jobs, exported. Import them in Replica's window (Jobs, Import...) or with");
    w.comment(
        "replica import and this file's path. Each [[jobs]] table is one job, as in settings.toml.",
    );
    w.comment(
        "It holds no passwords: only the names of share logins saved in Windows Credential Manager.",
    );
    w.blank();
    w.key(
        "Required. What this file is.",
        "format",
        &settings::str_value(FORMAT),
    );
    w.key(
        "Required. The version of this file's format.",
        "version",
        &VERSION.to_string(),
    );
    w.key(
        "The Replica that wrote it.",
        "replica_version",
        &settings::str_value(crate::VERSION),
    );
    for job in jobs {
        settings::write_job(&mut w, job);
    }
    w.out
}

/// Writes `jobs` to `path`: the whole file appears at once, never part of one, and an existing
/// file is replaced only with `overwrite`.
///
/// The text goes first into a temporary file of this call's own beside `path` (a new, unique
/// name, created exclusively, so no other file and no other export's is touched), which is then
/// renamed to `path`, complete. With `overwrite` the rename replaces a file there (tried again for
/// a moment while Windows refuses it, `settings::replace`); without, it's
/// `rename_no_clobber`, a rename that fails with [`io::ErrorKind::AlreadyExists`] if `path`
/// exists, so an existing file, or one another export made a moment before, is refused by the
/// same step that publishes, and nothing is ever at `path` but a whole export. Only the call's
/// own temporary file is removed if it fails.
///
/// std's `rename` always replaces, so the platform gives the no-clobber rename: on Windows,
/// `replica_win::files::rename_no_clobber` (`MoveFileExW`, on every file system); elsewhere, and
/// in core's reader check, [`link_no_clobber`].
pub fn write(
    path: &Path,
    jobs: &[Job],
    overwrite: bool,
    rename_no_clobber: fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), FileError> {
    let io_err = |source| FileError::Io {
        path: path.to_path_buf(),
        source,
    };
    let tmp = settings::write_temp(path, to_toml(jobs).as_bytes()).map_err(io_err)?;
    let published = if overwrite {
        settings::replace(&tmp, path)
    } else {
        rename_no_clobber(&tmp, path)
    };
    published.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        if e.kind() == io::ErrorKind::AlreadyExists {
            FileError::Exists(path.to_path_buf())
        } else {
            io_err(e)
        }
    })
}

/// A no-clobber rename for [`write`] from std alone: a hard link (made in one step, and refused
/// if `to` exists), then removing `from`. For file systems with hard links (NTFS, and those of
/// other OSes); not FAT or exFAT, so Windows uses `replica_win::files::rename_no_clobber`.
///
/// On Windows a file it made can't be replaced for a moment if something (an antivirus scan,
/// say) still holds the removed name open: the replace fails with Access is denied.
pub fn link_no_clobber(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    let _ = fs::remove_file(from);
    Ok(())
}

/// A jobs file, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsFile {
    /// The Replica that wrote it, if it says.
    pub replica_version: Option<String>,
    /// Its jobs, in order.
    pub entries: Vec<Entry>,
    /// Keys in its header that Replica doesn't know, ignored.
    pub warnings: Vec<Warning>,
}

/// One job in a jobs file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its name, or its position (`jobs[2]`) when it has none.
    pub label: String,
    /// The job, or why it can't be read (a value of the wrong type, an unknown target kind).
    pub job: Result<Job, String>,
    /// Keys in it that Replica doesn't know, ignored.
    pub warnings: Vec<Warning>,
}

/// Reads a jobs file.
pub fn read(path: &Path) -> Result<JobsFile, FileError> {
    let text = fs::read_to_string(path).map_err(|source| FileError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}

/// Reads a jobs file's text. A file of another format, or a newer version, is refused whole; a
/// job in it that can't be read is an [`Entry`] with the reason, and the others still read.
pub fn parse(text: &str) -> Result<JobsFile, FileError> {
    let mut raw: toml::Table = toml::from_str(text).map_err(|e: toml::de::Error| {
        let (line, column) = e.span().map(|s| settings::line_col(text, s.start)).unzip();
        FileError::Corrupt {
            line,
            column,
            message: e.message().trim().to_string(),
        }
    })?;
    if raw.get("format").and_then(|v| v.as_str()) != Some(FORMAT) {
        return Err(FileError::NotJobsFile);
    }
    match raw.get("version").and_then(|v| v.as_integer()) {
        Some(v) if v > VERSION => return Err(FileError::NewerVersion(v)),
        Some(v) if v >= 1 => {}
        _ => return Err(FileError::NotJobsFile),
    }
    let replica_version = raw
        .get("replica_version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let jobs = match raw.remove("jobs") {
        None => Vec::new(),
        Some(toml::Value::Array(jobs)) => jobs,
        Some(_) => {
            return Err(FileError::Corrupt {
                line: None,
                column: None,
                message: tr("error-jobs-file-jobs-not-tables"),
            });
        }
    };
    let warnings = raw
        .keys()
        .filter(|k| !matches!(k.as_str(), "format" | "version" | "replica_version"))
        .map(|k| Warning::UnknownKey(k.clone()))
        .collect();
    let entries = jobs
        .into_iter()
        .enumerate()
        .map(|(i, value)| entry(i, value))
        .collect();
    Ok(JobsFile {
        replica_version,
        entries,
        warnings,
    })
}

fn entry(i: usize, value: toml::Value) -> Entry {
    let position = format!("jobs[{i}]");
    let named = value
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    let label = named.unwrap_or_else(|| position.clone());
    let mut warnings = Vec::new();
    let job = match value.clone().try_into::<Job>() {
        Ok(job) => {
            if let (toml::Value::Table(raw), Ok(known)) = (&value, toml::Table::try_from(&job)) {
                settings::unknown_keys(raw, &known, &position, &mut warnings);
            }
            Ok(job)
        }
        Err(e) => Err(tr!(
            "error-jobs-file-job-unreadable",
            job = label.as_str(),
            detail = e.message().trim()
        )),
    };
    Entry {
        label,
        job,
        warnings,
    }
}

/// What to do with a job whose name is already a job's here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConflict {
    /// Leave the job here as it is, and don't import this one (the default).
    #[default]
    Skip,
    /// Import it under the first free name: `Name-2`, `Name-3`… ([`free_name`]).
    Copy,
    /// Import it in place of the job here, which keeps its name, so its history stays with it.
    Replace,
}

/// What to do with one job in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// Whether it's chosen for importing.
    pub import: bool,
    pub on_conflict: OnConflict,
}

impl Default for Choice {
    fn default() -> Self {
        Choice {
            import: true,
            on_conflict: OnConflict::Skip,
        }
    }
}

/// What a job in the file is, here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No job here has its name.
    New,
    /// A job here has its name already (ignoring case): this one's.
    NameExists(String),
    /// It can't be imported: each reason, in the user's words.
    Invalid(Vec<String>),
}

/// What importing does with a job in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Added as a new job, by this name (a copy's new name, or its own).
    Add(String),
    /// Put in place of the job here by this name.
    Replace(String),
    /// Left out: its name is taken and the choice is to skip it.
    Skip,
    /// Left out: not chosen.
    NotChosen,
    /// Left out: it's [`Status::Invalid`].
    Refused,
}

/// One job of the file in a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// Its name in the file, or its position (`jobs[2]`).
    pub label: String,
    pub status: Status,
    pub action: Action,
    /// The job here with its name (ignoring case), if there is one: whatever its status, so a
    /// job whose copy is [`Status::Invalid`] (a second mirror over the same folder) can still be
    /// skipped or put in place of that one instead. `None` for a job invalid in itself.
    pub conflict: Option<String>,
    /// Versioned targets with no saved repository password on this PC: the job imports, and
    /// asks for the password, as for a new repository. By target name.
    pub needs_password: Vec<String>,
    /// Drive targets whose drive isn't connected (no volume with that ID or label): the job
    /// imports, and shows the target as not connected, as any job does. By target name.
    pub drive_not_found: Vec<String>,
    /// The commands its steps run, so the preview can show them before anything is imported.
    pub commands: Vec<String>,
    /// Keys ignored, and anything that couldn't be checked (the secret store failing).
    pub warnings: Vec<String>,
}

/// Importing a jobs file: each job's status and action, and the settings that result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// One per job in the file, in its order.
    pub jobs: Vec<Planned>,
    /// The settings with the plan applied: the caller saves them.
    pub settings: Settings,
    /// The file's own warnings (unknown header keys).
    pub warnings: Vec<String>,
}

impl Plan {
    /// Whether applying it changes anything.
    pub fn changes_anything(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| matches!(j.action, Action::Add(_) | Action::Replace(_)))
    }
}

/// Plans importing `file` into `current`, with `choices[i]` for the file's job `i` (missing
/// ones take [`Choice::default`]). Jobs are taken in the file's order, each checked against
/// `current` and the jobs imported before it, so the plan is what saving it gives.
///
/// `secrets` and `targets` only look: whether a password is saved, whether a drive is here.
pub fn plan(
    file: &JobsFile,
    current: &Settings,
    choices: &[Choice],
    secrets: &dyn SecretStore,
    targets: &dyn TargetResolver,
) -> Plan {
    let mut settings = current.clone();
    // Problems the settings had already aren't the imported jobs' to answer for.
    let before: Vec<String> = current.validate().iter().map(|e| e.to_string()).collect();
    let new_problems = |s: &Settings| -> Vec<String> {
        s.validate()
            .iter()
            .map(|e| e.to_string())
            .filter(|e| !before.contains(e))
            .collect()
    };
    let mut planned = Vec::new();
    for (i, entry) in file.entries.iter().enumerate() {
        let choice = choices.get(i).copied().unwrap_or_default();
        let mut p = Planned {
            label: entry.label.clone(),
            status: Status::New,
            action: Action::NotChosen,
            conflict: None,
            needs_password: Vec::new(),
            drive_not_found: Vec::new(),
            commands: Vec::new(),
            warnings: entry.warnings.iter().map(|w| w.to_string()).collect(),
        };
        let refuse = |p: &mut Planned, reasons: Vec<String>| {
            p.status = Status::Invalid(reasons);
            p.action = if choice.import {
                Action::Refused
            } else {
                Action::NotChosen
            };
        };
        let mut job = match &entry.job {
            Ok(job) => job.clone(),
            Err(reason) => {
                refuse(&mut p, vec![reason.clone()]);
                planned.push(p);
                continue;
            }
        };
        p.commands = commands(&job);
        let own: Vec<String> = job::validate_jobs(std::slice::from_ref(&job))
            .iter()
            .map(|e| e.to_string())
            .collect();
        if !own.is_empty() {
            refuse(&mut p, own);
            planned.push(p);
            continue;
        }
        look_around(&job, secrets, targets, &mut p);
        job.enabled = false;
        let existing = settings
            .jobs
            .iter()
            .position(|j| j.name.eq_ignore_ascii_case(&job.name));
        let mut candidate = settings.clone();
        let action = match existing {
            None => {
                p.status = Status::New;
                let name = job.name.clone();
                candidate.jobs.push(job);
                Action::Add(name)
            }
            Some(at) => {
                let here = settings.jobs[at].name.clone();
                p.status = Status::NameExists(here.clone());
                p.conflict = Some(here.clone());
                match choice.on_conflict {
                    OnConflict::Skip => Action::Skip,
                    OnConflict::Copy => {
                        let name = free_name(&job.name, |n| {
                            settings.jobs.iter().any(|j| j.name.eq_ignore_ascii_case(n))
                        });
                        job.name = name.clone();
                        candidate.jobs.push(job);
                        Action::Add(name)
                    }
                    OnConflict::Replace => {
                        job.name = here.clone();
                        candidate.jobs[at] = job;
                        Action::Replace(here)
                    }
                }
            }
        };
        if matches!(action, Action::Skip) {
            p.action = if choice.import {
                Action::Skip
            } else {
                Action::NotChosen
            };
            planned.push(p);
            continue;
        }
        let problems = new_problems(&candidate);
        if !problems.is_empty() {
            refuse(&mut p, problems);
        } else if choice.import {
            settings = candidate;
            p.action = action;
        }
        planned.push(p);
    }
    Plan {
        jobs: planned,
        settings,
        warnings: file.warnings.iter().map(|w| w.to_string()).collect(),
    }
}

/// Whether the job's repositories have their passwords here, and its drives are connected.
fn look_around(
    job: &Job,
    secrets: &dyn SecretStore,
    targets: &dyn TargetResolver,
    p: &mut Planned,
) {
    for target in &job.targets {
        if job.mode == Mode::Versioned {
            match secrets.get(&password_secret_name(target)) {
                Ok(Some(_)) => {}
                Ok(None) => p.needs_password.push(target.display_name()),
                Err(e) => p.warnings.push(e.to_string()),
            }
        }
        if let Target::Drive { volume, .. } = target {
            match targets.locate_volume(volume) {
                Ok(Some(_)) => {}
                Ok(None) => p.drive_not_found.push(target.display_name()),
                Err(e) => p.warnings.push(e.to_string()),
            }
        }
    }
}

fn commands(job: &Job) -> Vec<String> {
    let before = job.before.iter().filter_map(|s| match s {
        BeforeStep::Command { cmd, .. } => Some(cmd.clone()),
        BeforeStep::SqliteSnapshot { .. } => None,
    });
    let after = job.after.iter().filter_map(|s| match s {
        AfterStep::Command { cmd, .. } => Some(cmd.clone()),
        AfterStep::Verify { .. } => None,
    });
    before.chain(after).collect()
}

/// The first of `Name-2`, `Name-3`… that isn't `taken`, shortening `name` so it stays within
/// [`MAX_NAME_LEN`]. Job names are ASCII, so shortening never splits a character.
pub fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let keep = name.len().min(MAX_NAME_LEN - suffix.len());
            format!("{}{suffix}", &name[..keep])
        })
        .find(|candidate| !taken(candidate))
        .expect("some number is free")
}

/// For tests, here and in `replica-win` (whose no-clobber rename is the one that ships).
#[doc(hidden)]
pub mod check {
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{FileError, to_toml, write};

    /// How long a round waits for the reader before the test fails, for a hung run only.
    const BOUND: Duration = Duration::from_secs(60);

    /// Stops and joins the reader however the check ends, a failure or a panic included.
    struct Reader {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// `rounds` exports to a jobs file in `dir` without overwrite, each followed by a second
    /// that must be refused, then removed, while a reader reads the file in a loop: every read
    /// must find no file or the whole export. Each round waits (up to [`BOUND`]) for a read that
    /// started after its export was published. Returns how many reads saw the whole export, or
    /// the first thing that went wrong.
    pub fn whole_or_absent(
        dir: &Path,
        rounds: usize,
        rename_no_clobber: fn(&Path, &Path) -> io::Result<()>,
    ) -> Result<usize, String> {
        let path = Arc::new(dir.join("jobs.replica-jobs.toml"));
        let whole = to_toml(&[]);
        let stop = Arc::new(AtomicBool::new(false));
        let published = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel::<(usize, Result<bool, String>)>();
        let handle = {
            let (path, stop, published) =
                (Arc::clone(&path), Arc::clone(&stop), Arc::clone(&published));
            let whole = whole.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let round = published.load(Ordering::Acquire);
                    let outcome = match fs::read_to_string(&*path) {
                        Ok(text) if text == whole => Ok(true),
                        Ok(text) => Err(format!(
                            "a read found {} bytes, not the whole export's {}",
                            text.len(),
                            whole.len()
                        )),
                        // Not there, or being removed.
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                            ) =>
                        {
                            Ok(false)
                        }
                        Err(e) => Err(format!("a read failed: {e}")),
                    };
                    let failed = outcome.is_err();
                    if tx.send((round, outcome)).is_err() || failed {
                        return;
                    }
                }
            })
        };
        let _reader = Reader {
            stop,
            handle: Some(handle),
        };
        let mut seen = 0;
        for round in 1..=rounds {
            write(&path, &[], false, rename_no_clobber).map_err(|e| e.to_string())?;
            match write(&path, &[], false, rename_no_clobber) {
                Err(FileError::Exists(_)) => {}
                other => return Err(format!("a second export wasn't refused: {other:?}")),
            }
            published.store(round, Ordering::Release);
            let deadline = Instant::now() + BOUND;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok((_, Err(e))) => return Err(e),
                    Ok((r, Ok(whole))) => {
                        seen += usize::from(whole);
                        if r >= round {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        return Err(format!("no read in {BOUND:?} in round {round}"));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err("the reader stopped".into());
                    }
                }
            }
            fs::remove_file(&*path).map_err(|e| e.to_string())?;
        }
        let left = fs::read_dir(dir).map_err(|e| e.to_string())?.count();
        if left != 0 {
            return Err(format!("{left} files left behind"));
        }
        Ok(seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Retention, Source, VolumeRef};
    use crate::platform::fake::{FakeResolver, MemorySecretStore};
    use crate::platform::{Credential, Secret};

    fn drive(label: &str, path: &str) -> Target {
        Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: None,
                label: Some(label.into()),
            },
            path: path.into(),
        }
    }

    fn job(name: &str, target: Target) -> Job {
        Job {
            name: name.into(),
            sources: vec![Source::with_common_excludes(r"D:\Work")],
            targets: vec![target],
            ..Job::default()
        }
    }

    /// [`write`] with [`claim_no_clobber`].
    fn put(path: &Path, jobs: &[Job], overwrite: bool) -> Result<(), FileError> {
        write(path, jobs, overwrite, claim_no_clobber)
    }

    /// A no-clobber rename from std without hard links, for tests that replace what they
    /// published, which [`link_no_clobber`]'s files now and then refuse on Windows: claims
    /// `to` with a new, empty file (refused if it exists, so of writers at once exactly one
    /// wins), then renames `from` over the claim. Not for a test with a reader beside it, which
    /// could see the empty claim.
    fn claim_no_clobber(from: &Path, to: &Path) -> io::Result<()> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        fs::rename(from, to)
    }

    /// A PC with the drive BACKUP connected, and no passwords saved.
    struct Pc {
        secrets: MemorySecretStore,
        targets: FakeResolver,
    }

    impl Pc {
        fn new() -> Pc {
            Pc {
                secrets: MemorySecretStore::default(),
                targets: FakeResolver {
                    volumes: vec![(None, Some("BACKUP".into()), r"E:\".into())],
                    ..FakeResolver::default()
                },
            }
        }

        fn with_password(self, target: &Target) -> Pc {
            self.secrets
                .set(
                    &password_secret_name(target),
                    &Credential {
                        username: None,
                        secret: Secret::new("x"),
                    },
                )
                .unwrap();
            self
        }

        fn plan(&self, text: &str, current: &[Job], choices: &[Choice]) -> Plan {
            let file = parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
            let current = Settings {
                jobs: current.to_vec(),
                ..Settings::default()
            };
            plan(&file, &current, choices, &self.secrets, &self.targets)
        }
    }

    fn every_kind_of_job() -> Vec<Job> {
        vec![
            Job {
                schedule: Some("daily 02:30".parse().unwrap()),
                notify_on_success: true,
                retention: Retention {
                    daily: 1,
                    weekly: 2,
                    monthly: 3,
                    keep_forever: true,
                },
                targets: vec![
                    Target::Drive {
                        name: Some("HDD".into()),
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
                before: vec![BeforeStep::SqliteSnapshot {
                    db: r"D:\Work\app.db".into(),
                }],
                after: vec![
                    AfterStep::Command {
                        cmd: "echo \"done\"".into(),
                        timeout_secs: 9,
                    },
                    AfterStep::Verify {
                        schedule: None,
                        read_data_subset: "1/10".into(),
                    },
                ],
                ..job("hive", drive("X", "y"))
            },
            Job {
                enabled: false,
                mode: Mode::Mirror,
                mirror_deletes_confirmed: true,
                mirror_delete_limit_percent: 40,
                ..job("photos", drive("PHOTOS", "Mirror"))
            },
        ]
    }

    #[test]
    fn a_file_round_trips_every_kind_of_job() {
        let jobs = every_kind_of_job();
        let text = to_toml(&jobs);
        let file = parse(&text).unwrap();
        assert_eq!(file.replica_version.as_deref(), Some(crate::VERSION));
        assert_eq!(file.warnings, []);
        let read: Vec<Job> = file.entries.into_iter().map(|e| e.job.unwrap()).collect();
        assert_eq!(read, jobs, "{text}");
        assert!(text.contains("format = \"replica-jobs\""), "{text}");
        assert!(!text.contains("[app]"), "jobs only: {text}");
        let empty = parse(&to_toml(&[])).unwrap();
        assert_eq!(empty.entries, []);
    }

    #[test]
    fn an_export_holds_no_secret() {
        // A versioned job whose repository has a password, and whose share needs a login.
        let jobs = every_kind_of_job();
        let pc = Pc::new();
        let (password, login) = ("repo-pass-6Xq!", "share-pass-T9z?");
        for target in &jobs[0].targets {
            pc.secrets
                .set(
                    &password_secret_name(target),
                    &Credential {
                        username: None,
                        secret: Secret::new(password),
                    },
                )
                .unwrap();
        }
        pc.secrets
            .set(
                "replica-nas",
                &Credential {
                    username: Some("backup-user".into()),
                    secret: Secret::new(login),
                },
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.replica-jobs.toml");
        put(&path, &jobs, false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        for secret in [password, login, "backup-user"] {
            assert!(!text.contains(secret), "{secret} in:\n{text}");
        }
        assert!(
            text.contains("credential = \"replica-nas\""),
            "the login's name, as settings.toml holds it: {text}"
        );
        assert!(!text.contains(job::REPOSITORY_SECRET_PREFIX), "{text}");
    }

    #[test]
    fn writing_refuses_to_overwrite_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.replica-jobs.toml");
        let jobs = every_kind_of_job();
        put(&path, &jobs[..1], false).unwrap();
        let err = put(&path, &jobs, false).unwrap_err();
        assert!(matches!(err, FileError::Exists(_)), "{err}");
        assert_eq!(
            parse(&fs::read_to_string(&path).unwrap())
                .unwrap()
                .entries
                .len(),
            1
        );
        put(&path, &jobs, true).unwrap();
        assert_eq!(read(&path).unwrap().entries.len(), 2);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temp file left: {names:?}");
    }

    #[test]
    fn writing_leaves_other_files_beside_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.toml");
        let sibling = dir.path().join("export.toml.tmp");
        fs::write(&sibling, "someone's document").unwrap();
        put(&path, &every_kind_of_job(), false).unwrap();
        put(&path, &every_kind_of_job(), true).unwrap();
        assert_eq!(fs::read_to_string(&sibling).unwrap(), "someone's document");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    /// Exports to one file at once, each of a different job, started together: as many as
    /// threads, run `rounds` times. Returns each round's results and the file's text.
    fn competing_writes(overwrite: bool, rounds: usize) -> Vec<(Vec<bool>, String, Vec<String>)> {
        use std::sync::{Arc, Barrier};
        const WRITERS: usize = 6;
        let payloads: Vec<Vec<Job>> = (0..WRITERS)
            .map(|i| vec![job(&format!("job{i}"), drive("BACKUP", &format!("p{i}")))])
            .collect();
        let texts: Vec<String> = payloads.iter().map(|p| to_toml(p)).collect();
        (0..rounds)
            .map(|_| {
                let dir = tempfile::tempdir().unwrap();
                let path = Arc::new(dir.path().join("jobs.replica-jobs.toml"));
                let barrier = Arc::new(Barrier::new(WRITERS));
                let handles: Vec<_> = payloads
                    .iter()
                    .cloned()
                    .map(|jobs| {
                        let (path, barrier) = (Arc::clone(&path), Arc::clone(&barrier));
                        std::thread::spawn(move || {
                            barrier.wait();
                            put(&path, &jobs, overwrite)
                        })
                    })
                    .collect();
                let results: Vec<bool> = handles
                    .into_iter()
                    .map(|h| match h.join().unwrap() {
                        Ok(()) => true,
                        Err(FileError::Exists(_)) => false,
                        Err(e) => panic!("{e}"),
                    })
                    .collect();
                let text = fs::read_to_string(&*path).unwrap();
                let left: Vec<String> = fs::read_dir(dir.path())
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|n| n != "jobs.replica-jobs.toml")
                    .collect();
                assert_eq!(left, Vec::<String>::new(), "no temp file left");
                assert!(texts.contains(&text), "a whole export, not a mix: {text}");
                (results, text, texts.clone())
            })
            .collect()
    }

    #[test]
    fn exports_at_once_without_overwrite_let_exactly_one_write() {
        for (results, text, texts) in competing_writes(false, 20) {
            let won: Vec<usize> = (0..results.len()).filter(|&i| results[i]).collect();
            assert_eq!(won.len(), 1, "exactly one writes the file: {results:?}");
            assert_eq!(
                text, texts[won[0]],
                "and the file is the one that succeeded"
            );
        }
    }

    #[test]
    fn a_reader_sees_no_file_or_a_whole_export_never_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let seen = check::whole_or_absent(dir.path(), 300, link_no_clobber)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(seen > 0, "the reader saw the file");
    }

    #[test]
    fn the_reader_check_fails_at_once_on_an_empty_export() {
        // Publishes an empty file and drops the export: the regression the check is for.
        fn empty(from: &Path, to: &Path) -> io::Result<()> {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(to)?;
            fs::remove_file(from)
        }
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let err = check::whole_or_absent(dir.path(), 300, empty).unwrap_err();
        assert!(err.contains("a read found 0 bytes"), "{err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "a failure, not a wait for the bound: {:?}",
            started.elapsed()
        );

        // A writer that fails ends the check too, the reader stopped and joined.
        fn refused(_: &Path, _: &Path) -> io::Result<()> {
            Err(io::Error::other("refused here"))
        }
        let dir = tempfile::tempdir().unwrap();
        let err = check::whole_or_absent(dir.path(), 300, refused).unwrap_err();
        assert!(err.contains("refused here"), "{err}");
    }

    #[test]
    fn exports_at_once_with_overwrite_all_write_a_whole_file() {
        for (results, _, _) in competing_writes(true, 20) {
            assert!(results.iter().all(|&ok| ok), "{results:?}");
        }
    }

    #[test]
    fn other_files_and_newer_versions_are_refused() {
        let settings = Settings::default().to_documented_toml();
        assert!(matches!(parse(&settings), Err(FileError::NotJobsFile)));
        assert!(matches!(
            parse("format = \"other\"\nversion = 1\n"),
            Err(FileError::NotJobsFile)
        ));
        assert!(matches!(
            parse("format = \"replica-jobs\"\n"),
            Err(FileError::NotJobsFile)
        ));
        let newer = parse("format = \"replica-jobs\"\nversion = 2\n").unwrap_err();
        assert!(matches!(newer, FileError::NewerVersion(2)));
        assert!(newer.to_string().contains("newer"), "{newer}");
        let corrupt = parse("format = \"replica-jobs\"\nversion = \n").unwrap_err();
        assert!(
            matches!(corrupt, FileError::Corrupt { line: Some(2), .. }),
            "{corrupt:?}"
        );
        let not_tables = parse("format = \"replica-jobs\"\nversion = 1\njobs = 3\n").unwrap_err();
        assert!(
            matches!(not_tables, FileError::Corrupt { .. }),
            "{not_tables:?}"
        );
        let io = read(Path::new("Z:/no/such/file.replica-jobs.toml")).unwrap_err();
        assert!(matches!(io, FileError::Io { .. }), "{io:?}");
    }

    #[test]
    fn a_new_job_imports_disabled() {
        let target = drive("BACKUP", "Repo");
        let pc = Pc::new().with_password(&target);
        let enabled = job("hive", target);
        assert!(enabled.enabled);
        let plan = pc.plan(&to_toml(std::slice::from_ref(&enabled)), &[], &[]);
        assert_eq!(plan.jobs[0].status, Status::New);
        assert_eq!(plan.jobs[0].action, Action::Add("hive".into()));
        assert_eq!(plan.jobs[0].needs_password, Vec::<String>::new());
        assert_eq!(plan.jobs[0].drive_not_found, Vec::<String>::new());
        assert!(plan.changes_anything());
        assert_eq!(
            plan.settings.jobs,
            [Job {
                enabled: false,
                ..enabled
            }]
        );
    }

    #[test]
    fn a_taken_name_is_skipped_copied_or_replaced() {
        let pc = Pc::new();
        let here = job("Hive", drive("BACKUP", "Here"));
        let theirs = job("hive", drive("BACKUP", "Theirs"));
        let text = to_toml(std::slice::from_ref(&theirs));
        let with = |on_conflict| {
            pc.plan(
                &text,
                std::slice::from_ref(&here),
                &[Choice {
                    import: true,
                    on_conflict,
                }],
            )
        };

        let skip = pc.plan(&text, std::slice::from_ref(&here), &[]);
        assert_eq!(skip.jobs[0].status, Status::NameExists("Hive".into()));
        assert_eq!(skip.jobs[0].action, Action::Skip, "skip is the default");
        assert_eq!(skip.settings.jobs, std::slice::from_ref(&here));
        assert!(!skip.changes_anything());

        let copy = with(OnConflict::Copy);
        assert_eq!(copy.jobs[0].action, Action::Add("hive-2".into()));
        assert_eq!(copy.settings.jobs.len(), 2);
        assert_eq!(copy.settings.jobs[1].name, "hive-2");
        assert!(!copy.settings.jobs[1].enabled);

        let replace = with(OnConflict::Replace);
        assert_eq!(replace.jobs[0].action, Action::Replace("Hive".into()));
        assert_eq!(
            replace.settings.jobs,
            [Job {
                name: "Hive".into(),
                enabled: false,
                ..theirs
            }],
            "the job here keeps its name, so its history stays with it"
        );
    }

    #[test]
    fn copies_take_the_first_free_name_within_the_limit() {
        let taken = ["hive", "hive-2", "HIVE-3"];
        let is_taken = |n: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(n));
        assert_eq!(free_name("hive", is_taken), "hive-4");
        assert_eq!(free_name("photos", is_taken), "photos-2");
        let long = "x".repeat(MAX_NAME_LEN);
        let copy = free_name(&long, |n| n == format!("{}-2", &long[..62]));
        assert_eq!(copy, format!("{}-3", &long[..62]));
        assert_eq!(copy.len(), MAX_NAME_LEN);
        assert!(job::is_valid_name(&copy));

        // Two copies from one file take a name each.
        let pc = Pc::new();
        let a = job("hive", drive("BACKUP", "A"));
        let text = to_toml(&[
            a.clone(),
            Job {
                name: "HIVE".into(),
                ..a.clone()
            },
        ]);
        let copy = Choice {
            import: true,
            on_conflict: OnConflict::Copy,
        };
        let plan = pc.plan(&text, &[a], &[copy, copy]);
        let actions: Vec<_> = plan.jobs.iter().map(|j| j.action.clone()).collect();
        assert_eq!(
            actions,
            [Action::Add("hive-2".into()), Action::Add("HIVE-3".into())]
        );
    }

    #[test]
    fn a_missing_password_or_drive_still_imports() {
        let pc = Pc::new();
        let gone = drive("ELSEWHERE", "Repo");
        let share = Target::Unc {
            name: Some("NAS".into()),
            path: r"\\nas\backup".into(),
            credential: None,
        };
        let mut versioned = job("hive", gone.clone());
        versioned.targets.push(share);
        let mirror = Job {
            mode: Mode::Mirror,
            mirror_deletes_confirmed: true,
            ..job("m", drive("BACKUP", "Mirror"))
        };
        let plan = pc.plan(&to_toml(&[versioned, mirror]), &[], &[]);
        let hive = &plan.jobs[0];
        assert_eq!(hive.action, Action::Add("hive".into()));
        assert_eq!(hive.needs_password, ["ELSEWHERE:Repo", "NAS"]);
        assert_eq!(hive.drive_not_found, ["ELSEWHERE:Repo"]);
        let m = &plan.jobs[1];
        assert_eq!(m.action, Action::Add("m".into()));
        assert_eq!(
            m.needs_password,
            Vec::<String>::new(),
            "a mirror has no password"
        );
        assert_eq!(plan.settings.jobs.len(), 2);
    }

    #[test]
    fn invalid_jobs_are_never_imported() {
        let pc = Pc::new();
        let text = format!(
            "{}\n{}",
            to_toml(&[job("ok", drive("BACKUP", "Ok"))]),
            r#"
[[jobs]]
name = "bad name"
[[jobs.sources]]
path = 'D:\Work'
[[jobs.targets]]
kind = "drive"
label = "BACKUP"
path = "x"

[[jobs]]
name = "unreadable"
[[jobs.targets]]
kind = "ftp"
path = "x"

[[jobs]]
name = "unconfirmed"
mode = "mirror"
[[jobs.sources]]
path = 'D:\Work'
[[jobs.targets]]
kind = "drive"
label = "BACKUP"
path = "m"

[[jobs]]
name = "stolen"
[[jobs.sources]]
path = 'D:\Work'
[[jobs.targets]]
kind = "unc"
path = '\\evil\share'
credential = 'replica:repo:drive:label=backup:ok'
"#
        );
        let plan = pc.plan(&text, &[], &[]);
        let actions: Vec<_> = plan
            .jobs
            .iter()
            .map(|j| (j.label.as_str(), j.action.clone()))
            .collect();
        assert_eq!(
            actions,
            [
                ("ok", Action::Add("ok".into())),
                ("bad name", Action::Refused),
                ("unreadable", Action::Refused),
                ("unconfirmed", Action::Refused),
                ("stolen", Action::Refused),
            ]
        );
        for p in &plan.jobs[1..] {
            let Status::Invalid(reasons) = &p.status else {
                panic!("{p:?}")
            };
            assert!(!reasons.is_empty());
        }
        let Status::Invalid(r) = &plan.jobs[3].status else {
            unreachable!()
        };
        assert!(r[0].contains("mirror_deletes_confirmed"), "{r:?}");
        assert_eq!(plan.settings.jobs.len(), 1);
        assert_eq!(plan.settings.jobs[0].name, "ok");
    }

    #[test]
    fn a_copy_that_s_invalid_keeps_its_conflict_so_replace_can_still_be_chosen() {
        let pc = Pc::new();
        let mirror = Job {
            mode: Mode::Mirror,
            mirror_deletes_confirmed: true,
            ..job("music", drive("BACKUP", "Mirror"))
        };
        let text = to_toml(std::slice::from_ref(&mirror));
        let here = std::slice::from_ref(&mirror);
        let with = |on_conflict| {
            pc.plan(
                &text,
                here,
                &[Choice {
                    import: true,
                    on_conflict,
                }],
            )
        };
        // A copy would be a second mirror over the same folder: refused, the name still taken.
        let copy = with(OnConflict::Copy);
        assert!(
            matches!(copy.jobs[0].status, Status::Invalid(_)),
            "{copy:?}"
        );
        assert_eq!(copy.jobs[0].action, Action::Refused);
        assert_eq!(copy.jobs[0].conflict.as_deref(), Some("music"));
        let replace = with(OnConflict::Replace);
        assert_eq!(replace.jobs[0].action, Action::Replace("music".into()));
        assert_eq!(replace.jobs[0].conflict.as_deref(), Some("music"));
        assert!(!replace.settings.jobs[0].enabled);

        // A job invalid in itself has no conflict to resolve.
        let bad = pc.plan(
            &format!("{text}\n[[jobs]]\nname = \"music\"\nmode = \"mirror\"\n"),
            here,
            &[],
        );
        assert!(matches!(bad.jobs[1].status, Status::Invalid(_)));
        assert_eq!(bad.jobs[1].conflict, None);
        assert_eq!(bad.jobs[0].conflict.as_deref(), Some("music"), "skipped");
    }

    #[test]
    fn a_mirror_may_not_overlap_the_jobs_here() {
        let pc = Pc::new();
        let repo = job("repo", drive("BACKUP", "Repo"));
        let mirror = Job {
            mode: Mode::Mirror,
            mirror_deletes_confirmed: true,
            ..job("m", drive("BACKUP", r"Repo\inside"))
        };
        let plan = pc.plan(
            &to_toml(std::slice::from_ref(&mirror)),
            std::slice::from_ref(&repo),
            &[],
        );
        assert_eq!(plan.jobs[0].action, Action::Refused);
        let Status::Invalid(r) = &plan.jobs[0].status else {
            panic!("{:?}", plan.jobs[0])
        };
        assert!(r[0].contains("repository"), "{r:?}");
        assert_eq!(plan.settings.jobs, std::slice::from_ref(&repo));

        // Nor another job imported from the same file.
        let plan = pc.plan(&to_toml(&[repo.clone(), mirror]), &[], &[]);
        assert_eq!(plan.jobs[0].action, Action::Add("repo".into()));
        assert_eq!(plan.jobs[1].action, Action::Refused);
        assert_eq!(plan.settings.jobs.len(), 1);
    }

    #[test]
    fn jobs_not_chosen_are_left_out_but_still_shown() {
        let pc = Pc::new();
        let text = to_toml(&[
            job("a", drive("BACKUP", "A")),
            job("b", drive("BACKUP", "B")),
        ]);
        let plan = pc.plan(
            &text,
            &[],
            &[
                Choice {
                    import: false,
                    ..Choice::default()
                },
                Choice::default(),
            ],
        );
        assert_eq!(plan.jobs[0].status, Status::New);
        assert_eq!(plan.jobs[0].action, Action::NotChosen);
        assert_eq!(plan.jobs[1].action, Action::Add("b".into()));
        assert_eq!(plan.settings.jobs.len(), 1);
    }

    #[test]
    fn unknown_keys_and_commands_are_shown() {
        let pc = Pc::new();
        let mut j = job("a", drive("BACKUP", "A"));
        j.before.push(BeforeStep::Command {
            cmd: "net stop app".into(),
            timeout_secs: 60,
        });
        let text = format!(
            "{}\npassword = \"hunter2\"\n",
            to_toml(&[j]).replacen("format =", "colour = \"blue\"\nformat =", 1)
        );
        let plan = pc.plan(&text, &[], &[]);
        assert_eq!(plan.warnings, ["Unknown setting \"colour\" ignored."]);
        assert_eq!(plan.jobs[0].commands, ["net stop app"]);
        assert_eq!(
            plan.jobs[0].warnings,
            ["Unknown setting \"jobs[0].before[0].password\" ignored."]
        );
    }
}
