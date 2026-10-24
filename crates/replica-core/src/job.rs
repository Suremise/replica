//! The job model: what to back up, where to, how and when.
//!
//! These types are the `[[jobs]]` part of `settings.toml` (see [`crate::settings`]). Every field
//! has a default, so a settings file may leave out any key except a job's name, sources and
//! targets.

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::exclude;
use crate::i18n::tr;
use crate::paths;
use crate::schedule::Schedule;
use crate::tr;

/// The longest job name allowed.
pub const MAX_NAME_LEN: usize = 64;

/// The start of every repository password's name in the secret store
/// ([`crate::restic::password_secret_name`]). A share login can't have a name that starts with
/// it, ignoring case, or a target could hand a repository's password to a share as its login.
pub const REPOSITORY_SECRET_PREFIX: &str = "replica:repo:";

/// A backup job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// Its name, used on the command line: letters, digits, `.`, `_` and `-`, starting with a
    /// letter or digit, at most [`MAX_NAME_LEN`] characters, unique ignoring case.
    pub name: String,
    /// Whether it runs on its schedule and with `run --all`.
    pub enabled: bool,
    /// `versioned` (restic snapshots) or `mirror` (an exact copy).
    pub mode: Mode,
    /// A mirror copies deletions too. A mirror job must set this to show the user has accepted
    /// that, or it won't validate.
    pub mirror_deletes_confirmed: bool,
    /// A mirror run stops, without deleting anything, when it would delete more than this
    /// share of the target's files (a percentage; 100 allows any). See [`crate::mirror`].
    pub mirror_delete_limit_percent: u32,
    /// When it runs on its own. `None` runs it only by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// Whether a successful run notifies. A failure always does.
    pub notify_on_success: bool,
    /// How many snapshots a versioned job keeps.
    pub retention: Retention,
    /// What it backs up.
    pub sources: Vec<Source>,
    /// Where it backs up to, one after another, each with its own result.
    pub targets: Vec<Target>,
    /// Steps before the backup. A failed step fails the job.
    pub before: Vec<BeforeStep>,
    /// Steps after the backup.
    pub after: Vec<AfterStep>,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            name: String::new(),
            enabled: true,
            mode: Mode::default(),
            mirror_deletes_confirmed: false,
            mirror_delete_limit_percent: DEFAULT_MIRROR_DELETE_LIMIT_PERCENT,
            schedule: None,
            notify_on_success: false,
            retention: Retention::default(),
            sources: Vec::new(),
            targets: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
        }
    }
}

/// How a job copies its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Encrypted, deduplicated restic snapshots you can go back to.
    #[default]
    Versioned,
    /// An exact copy of the sources: changes and deletions are copied across.
    Mirror,
}

/// A folder or file to back up, with its own exclusions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    /// An absolute path.
    pub path: PathBuf,
    /// Exclude patterns, relative to `path` (see [`crate::exclude`]).
    pub exclude: Vec<String>,
}

impl Source {
    /// A source with the [`exclude::COMMON_EXCLUDES`], as a new job in the window starts.
    pub fn with_common_excludes(path: impl Into<PathBuf>) -> Self {
        Source {
            path: path.into(),
            exclude: exclude::COMMON_EXCLUDES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Where a job backs up to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Target {
    /// A folder on a local or external drive, found by its volume rather than its drive letter,
    /// so a drive that comes back as `F:` instead of `E:` still matches.
    Drive {
        /// A name for results and the window. Defaults to the volume and path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The volume: its ID or its label, or both (the ID is tried first).
        #[serde(flatten)]
        volume: VolumeRef,
        /// The folder on that volume, relative to its root.
        path: PathBuf,
    },
    /// A folder on an SMB share, by UNC path (`\\nas\share\folder`).
    Unc {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The UNC path.
        path: String,
        /// The share login's name in the secret store, if it needs one. Never the secret.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<String>,
    },
}

/// A volume, by ID or label.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VolumeRef {
    /// The volume's ID (on Windows, its volume GUID, `\\?\Volume{…}\`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_id: Option<String>,
    /// The volume's label, used when there's no ID or it doesn't match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Target {
    /// Its name, or one made from where it points.
    pub fn display_name(&self) -> String {
        match self {
            Target::Drive { name: Some(n), .. } | Target::Unc { name: Some(n), .. } => n.clone(),
            Target::Drive { volume, path, .. } => {
                let vol = volume
                    .label
                    .as_deref()
                    .or(volume.volume_id.as_deref())
                    .unwrap_or("?");
                format!("{vol}:{}", path.display())
            }
            Target::Unc { path, .. } => path.clone(),
        }
    }
}

/// How many snapshots a versioned job keeps (restic's `forget --keep-*`), or all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Retention {
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
    /// Keep everything, forever: its runs never `forget` or `prune`, and the backup grows
    /// over time. The counts above are kept for when it's turned off.
    pub keep_forever: bool,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            daily: 7,
            weekly: 4,
            monthly: 6,
            keep_forever: false,
        }
    }
}

/// A step before the backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum BeforeStep {
    /// A consistent copy of a SQLite database, into Replica's own staging folder (never a
    /// folder of the user's, by design). The copy is backed up instead of the live file, which
    /// is left out with its `-wal`, `-shm` and `-journal`.
    SqliteSnapshot { db: PathBuf },
    /// A command; a non-zero exit code or the timeout fails the job.
    Command {
        cmd: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
}

/// A step after the backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AfterStep {
    /// A command, as for [`BeforeStep::Command`].
    Command {
        cmd: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
    /// restic `check --read-data-subset`, on its own schedule (versioned jobs only).
    Verify {
        /// When it runs; `None` runs it after every backup.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schedule: Option<Schedule>,
        /// How much data to read back, as restic takes it: `5%`, `1/10` or a size like `500M`.
        #[serde(default = "default_subset")]
        read_data_subset: String,
    },
}

/// The share of a mirror's target files a run may delete unless set.
pub const DEFAULT_MIRROR_DELETE_LIMIT_PERCENT: u32 = 30;
/// A command step's timeout unless set.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// How much a verify step reads back unless set.
pub const DEFAULT_READ_DATA_SUBSET: &str = "5%";

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

fn default_subset() -> String {
    DEFAULT_READ_DATA_SUBSET.to_string()
}

/// A problem that stops settings from being used, with the job it's in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// The job's name, or its position (`jobs[2]`) if it has none.
    pub job: Option<String>,
    pub problem: Problem,
}

/// What's wrong. Its [`fmt::Display`] text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NameMissing,
    NameInvalid(String),
    NameDuplicate(String),
    NoSources,
    NoTargets,
    SourceNotAbsolute(PathBuf),
    BadExclude(exclude::ExcludeError),
    DriveWithoutVolume,
    DrivePathOutside(PathBuf),
    UncPathInvalid(String),
    CredentialEmpty,
    /// A share login's name is a repository password's ([`REPOSITORY_SECRET_PREFIX`]).
    CredentialReserved(String),
    MirrorNotConfirmed,
    VerifyOnMirror,
    EmptyCommand,
    ZeroTimeout,
    RetentionKeepsNothing,
    DeleteLimitOver100,
    /// A mirror target is another mirror target (of this job or another), or inside or around
    /// it: each would delete the other's files.
    MirrorOverlapsMirror {
        target: String,
        other: String,
        other_target: String,
    },
    /// A mirror target is a versioned job's repository, or inside or around it: the mirror
    /// would delete the repository's files.
    MirrorOverlapsRepository {
        target: String,
        other: String,
        other_target: String,
    },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = |p: &PathBuf| p.display().to_string();
        let text = match self {
            Problem::NameMissing => tr("error-job-name-missing"),
            Problem::NameInvalid(n) => {
                tr!(
                    "error-job-name-invalid",
                    name = n.as_str(),
                    max = MAX_NAME_LEN
                )
            }
            Problem::NameDuplicate(n) => tr!("error-job-name-duplicate", name = n.as_str()),
            Problem::NoSources => tr("error-job-no-sources"),
            Problem::NoTargets => tr("error-job-no-targets"),
            Problem::SourceNotAbsolute(p) => tr!("error-job-source-not-absolute", path = path(p)),
            Problem::BadExclude(e) => e.to_string(),
            Problem::DriveWithoutVolume => tr("error-job-drive-without-volume"),
            Problem::DrivePathOutside(p) => tr!("error-job-drive-path-outside", path = path(p)),
            Problem::UncPathInvalid(p) => tr!("error-job-unc-invalid", path = p.as_str()),
            Problem::CredentialEmpty => tr("error-job-credential-empty"),
            Problem::CredentialReserved(name) => {
                tr!("error-job-credential-reserved", name = name.as_str())
            }
            Problem::MirrorNotConfirmed => tr("error-job-mirror-not-confirmed"),
            Problem::VerifyOnMirror => tr("error-job-verify-on-mirror"),
            Problem::EmptyCommand => tr("error-job-empty-command"),
            Problem::ZeroTimeout => tr("error-job-zero-timeout"),
            Problem::RetentionKeepsNothing => tr("error-job-retention-keeps-nothing"),
            Problem::DeleteLimitOver100 => tr("error-job-delete-limit"),
            Problem::MirrorOverlapsMirror {
                target,
                other,
                other_target,
            } => tr!(
                "error-job-mirror-overlaps-mirror",
                target = target.as_str(),
                other = other.as_str(),
                other_target = other_target.as_str()
            ),
            Problem::MirrorOverlapsRepository {
                target,
                other,
                other_target,
            } => tr!(
                "error-job-mirror-overlaps-repository",
                target = target.as_str(),
                other = other.as_str(),
                other_target = other_target.as_str()
            ),
        };
        f.write_str(&text)
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.job {
            Some(job) => f.write_str(&tr!(
                "error-job-problem",
                job = job.as_str(),
                problem = self.problem.to_string()
            )),
            None => write!(f, "{}", self.problem),
        }
    }
}

/// Whether a job name is in the safe set.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Checks every job, and the jobs together, collecting every problem.
pub fn validate_jobs(jobs: &[Job]) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    for (i, job) in jobs.iter().enumerate() {
        let label = if job.name.is_empty() {
            format!("jobs[{i}]")
        } else {
            job.name.clone()
        };
        let mut push = |problem| {
            errors.push(ValidationError {
                job: Some(label.clone()),
                problem,
            })
        };
        if job.name.is_empty() {
            push(Problem::NameMissing);
        } else if !is_valid_name(&job.name) {
            push(Problem::NameInvalid(job.name.clone()));
        } else if !seen.insert(job.name.to_ascii_lowercase()) {
            push(Problem::NameDuplicate(job.name.clone()));
        }
        for problem in job_problems(job) {
            push(problem);
        }
    }
    for (job, problem) in overlapping_targets(jobs) {
        errors.push(ValidationError {
            job: Some(job),
            problem,
        });
    }
    errors
}

/// Mirror targets that overlap another mirror target or a repository, each reported on
/// the mirror job (the later one, for two mirrors). Versioned jobs sharing a repository are the
/// supported case and aren't reported. Targets are compared as configured: drives by volume (ID
/// or label) and path, shares by UNC path. A drive and a share are never compared.
fn overlapping_targets(jobs: &[Job]) -> Vec<(String, Problem)> {
    let label = |i: usize, job: &Job| {
        if job.name.is_empty() {
            format!("jobs[{i}]")
        } else {
            job.name.clone()
        }
    };
    let all: Vec<(String, Mode, &Target)> = jobs
        .iter()
        .enumerate()
        .flat_map(|(i, job)| {
            job.targets
                .iter()
                .map(move |t| (label(i, job), job.mode, t))
        })
        .collect();
    let mut out = Vec::new();
    for (n, (job, mode, target)) in all.iter().enumerate() {
        for (other, other_mode, other_target) in &all[..n] {
            if !targets_overlap(target, other_target) {
                continue;
            }
            let problem = |mine: &Target, theirs: &Target, other: &str, mirror: bool| {
                let (target, other, other_target) = (
                    mine.display_name(),
                    other.to_string(),
                    theirs.display_name(),
                );
                if mirror {
                    Problem::MirrorOverlapsMirror {
                        target,
                        other,
                        other_target,
                    }
                } else {
                    Problem::MirrorOverlapsRepository {
                        target,
                        other,
                        other_target,
                    }
                }
            };
            match (mode, other_mode) {
                (Mode::Mirror, Mode::Mirror) => {
                    out.push((job.clone(), problem(target, other_target, other, true)))
                }
                (Mode::Mirror, Mode::Versioned) => {
                    out.push((job.clone(), problem(target, other_target, other, false)))
                }
                (Mode::Versioned, Mode::Mirror) => {
                    out.push((other.clone(), problem(other_target, target, job, false)))
                }
                (Mode::Versioned, Mode::Versioned) => {}
            }
        }
    }
    out
}

/// Whether two targets, as configured, are the same folder or one is inside the other.
fn targets_overlap(a: &Target, b: &Target) -> bool {
    match (a, b) {
        (
            Target::Drive {
                volume: va,
                path: pa,
                ..
            },
            Target::Drive {
                volume: vb,
                path: pb,
                ..
            },
        ) => {
            let same = |x: &Option<String>, y: &Option<String>| matches!((x, y), (Some(x), Some(y)) if x.trim().eq_ignore_ascii_case(y.trim()));
            // By components as Windows reads them, so `./m`, `m\` and `M.` are `m`.
            let parts = |p: &PathBuf| paths::typed_components(&p.to_string_lossy(), 0);
            (same(&va.volume_id, &vb.volume_id) || same(&va.label, &vb.label))
                && paths::components_overlap(&parts(pa), &parts(pb))
        }
        (Target::Unc { path: pa, .. }, Target::Unc { path: pb, .. }) => {
            // A share's server and name are its root: `..` never climbs above them.
            paths::components_overlap(
                &paths::typed_components(pa, 2),
                &paths::typed_components(pb, 2),
            )
        }
        _ => false,
    }
}

fn job_problems(job: &Job) -> Vec<Problem> {
    let mut out = Vec::new();
    if job.sources.is_empty() {
        out.push(Problem::NoSources);
    }
    if job.targets.is_empty() {
        out.push(Problem::NoTargets);
    }
    for source in &job.sources {
        if !is_absolute(&source.path) {
            out.push(Problem::SourceNotAbsolute(source.path.clone()));
        }
        for pattern in &source.exclude {
            if let Err(e) = exclude::check_pattern(pattern) {
                out.push(Problem::BadExclude(e));
            }
        }
    }
    for target in &job.targets {
        match target {
            Target::Drive { volume, path, .. } => {
                let blank = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());
                if blank(&volume.volume_id) && blank(&volume.label) {
                    out.push(Problem::DriveWithoutVolume);
                }
                if !is_inside_volume(path) {
                    out.push(Problem::DrivePathOutside(path.clone()));
                }
            }
            Target::Unc {
                path, credential, ..
            } => {
                if !is_unc(path) {
                    out.push(Problem::UncPathInvalid(path.clone()));
                }
                match credential.as_deref() {
                    Some(c) if c.trim().is_empty() => out.push(Problem::CredentialEmpty),
                    Some(c)
                        if c.trim()
                            .to_lowercase()
                            .starts_with(REPOSITORY_SECRET_PREFIX) =>
                    {
                        out.push(Problem::CredentialReserved(c.to_string()))
                    }
                    _ => {}
                }
            }
        }
    }
    if job.mirror_delete_limit_percent > 100 {
        out.push(Problem::DeleteLimitOver100);
    }
    if job.mode == Mode::Mirror && !job.mirror_deletes_confirmed {
        out.push(Problem::MirrorNotConfirmed);
    }
    if job.mode == Mode::Versioned {
        let r = job.retention;
        if !r.keep_forever && r.daily == 0 && r.weekly == 0 && r.monthly == 0 {
            out.push(Problem::RetentionKeepsNothing);
        }
    }
    let commands = job.before.iter().filter_map(|s| match s {
        BeforeStep::Command { cmd, timeout_secs } => Some((cmd, timeout_secs)),
        BeforeStep::SqliteSnapshot { .. } => None,
    });
    let after_commands = job.after.iter().filter_map(|s| match s {
        AfterStep::Command { cmd, timeout_secs } => Some((cmd, timeout_secs)),
        AfterStep::Verify { .. } => None,
    });
    for (cmd, timeout) in commands.chain(after_commands) {
        if cmd.trim().is_empty() {
            out.push(Problem::EmptyCommand);
        }
        if *timeout == 0 {
            out.push(Problem::ZeroTimeout);
        }
    }
    if job.mode == Mode::Mirror
        && job
            .after
            .iter()
            .any(|s| matches!(s, AfterStep::Verify { .. }))
    {
        out.push(Problem::VerifyOnMirror);
    }
    out
}

/// Absolute on Windows (`C:\…`, `\\server\share`, `\\?\C:\…`) or elsewhere (`/…`), judged the
/// same whatever OS this runs on, so settings validate the same everywhere.
fn is_absolute(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return match rest.strip_prefix(r"UNC\") {
            Some(unc) => is_unc(&format!(r"\\{unc}")),
            None => is_drive_absolute(rest),
        };
    }
    is_drive_absolute(&text) || is_unc(&text) || (text.starts_with('/') && !text.starts_with("//"))
}

/// Whether a drive target's path stays inside its volume: relative, with no drive letter or
/// prefix (`C:outside`, `\\?\`), no leading separator and no `..`. Judged the same on every OS.
pub fn is_inside_volume(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    !text.starts_with(['/', '\\'])
        && text
            .split(['/', '\\'])
            .all(|part| part != ".." && !part.contains(':'))
}

fn is_drive_absolute(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
}

fn is_unc(text: &str) -> bool {
    let rest = text.strip_prefix(r"\\").or_else(|| text.strip_prefix("//"));
    rest.is_some_and(|r| {
        let mut parts = r.split(['\\', '/']);
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        !server.is_empty() && server != "?" && server != "." && !share.is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn valid_job(name: &str) -> Job {
        Job {
            name: name.to_string(),
            sources: vec![Source::with_common_excludes(r"D:\Work\Hive")],
            targets: vec![Target::Drive {
                name: None,
                volume: VolumeRef {
                    volume_id: None,
                    label: Some("BACKUP".into()),
                },
                path: "Replica/Hive".into(),
            }],
            ..Job::default()
        }
    }

    fn problems(job: Job) -> Vec<Problem> {
        validate_jobs(&[job])
            .into_iter()
            .map(|e| e.problem)
            .collect()
    }

    #[test]
    fn names() {
        for ok in ["hive", "Hive-2", "a.b_c", "9", &"x".repeat(MAX_NAME_LEN)] {
            assert!(is_valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-x",
            ".x",
            "a b",
            "a/b",
            "ä",
            "a;b",
            &"x".repeat(MAX_NAME_LEN + 1),
        ] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_valid_job_passes() {
        assert_eq!(problems(valid_job("hive")), []);
    }

    fn mirror(name: &str, target: Target) -> Job {
        Job {
            mode: Mode::Mirror,
            mirror_deletes_confirmed: true,
            targets: vec![target],
            ..valid_job(name)
        }
    }

    fn drive(label: &str, id: Option<&str>, path: &str) -> Target {
        Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: id.map(Into::into),
                label: Some(label.into()),
            },
            path: path.into(),
        }
    }

    fn unc(path: &str) -> Target {
        Target::Unc {
            name: None,
            path: path.into(),
            credential: None,
        }
    }

    #[test]
    fn overlapping_mirrors_are_refused() {
        let same = validate_jobs(&[
            mirror("a", drive("BACKUP", None, "Mirror")),
            mirror("b", drive("backup", None, r"mirror\")),
        ]);
        assert_eq!(same.len(), 1, "{same:?}");
        assert_eq!(same[0].job.as_deref(), Some("b"));
        assert!(matches!(
            &same[0].problem,
            Problem::MirrorOverlapsMirror { other, .. } if other == "a"
        ));
        let text = same[0].to_string();
        assert!(text.contains("delete the other's files"), "{text}");

        let nested = validate_jobs(&[
            mirror("a", unc(r"\\nas\share\mirror")),
            mirror("b", unc(r"\\NAS\share\Mirror\inner")),
        ]);
        assert_eq!(nested.len(), 1, "nested, either way: {nested:?}");
        let root = validate_jobs(&[
            mirror("a", drive("BACKUP", None, "")),
            mirror("b", drive("BACKUP", None, "anything")),
        ]);
        assert_eq!(
            root.len(),
            1,
            "the volume's root holds everything: {root:?}"
        );
        let by_id = validate_jobs(&[
            mirror("a", drive("ONE", Some("vol-1"), "m")),
            mirror("b", drive("TWO", Some("VOL-1"), "m")),
        ]);
        assert_eq!(by_id.len(), 1, "the same volume by ID: {by_id:?}");

        // Aliases of one folder, as Windows opens them (round 1's finding: `m` and `./m`).
        for (a, b) in [
            ("m", "./m"),
            ("m", r".\m\"),
            ("m", "M."),
            ("m", "m "),
            ("m", "m/./inner"),
            ("m", "m//inner"),
        ] {
            let errors = validate_jobs(&[
                mirror("a", drive("BACKUP", None, a)),
                mirror("b", drive("BACKUP", None, b)),
            ]);
            assert_eq!(errors.len(), 1, "{a} and {b}: {errors:?}");
        }
        for (a, b) in [
            (r"\\nas\share\m", r"\\NAS\share\.\m"),
            (r"\\nas\share\m", r"\\nas\share\x\..\m\inner"),
            (r"\\nas\share", r"\\nas\share\..\m"),
        ] {
            let errors = validate_jobs(&[mirror("a", unc(a)), mirror("b", unc(b))]);
            assert_eq!(errors.len(), 1, "{a} and {b}: {errors:?}");
        }
        let repo_alias = validate_jobs(&[
            Job {
                targets: vec![drive("BACKUP", None, "Replica/Repo")],
                ..valid_job("repo")
            },
            mirror("m", drive("BACKUP", None, r".\replica\repo.")),
        ]);
        assert_eq!(
            repo_alias.len(),
            1,
            "a mirror on an alias of a repository: {repo_alias:?}"
        );

        let mut two_targets = mirror("a", drive("BACKUP", None, "m"));
        two_targets.targets.push(drive("BACKUP", None, "m/inner"));
        assert_eq!(validate_jobs(&[two_targets]).len(), 1, "within one job too");

        for apart in [
            [
                mirror("a", drive("BACKUP", None, "m")),
                mirror("b", drive("BACKUP", None, "m2")),
            ],
            [
                mirror("a", drive("BACKUP", None, "m")),
                mirror("b", drive("OTHER", None, "m")),
            ],
            [
                mirror("a", unc(r"\\nas\share\m")),
                mirror("b", drive("BACKUP", None, "m")),
            ],
        ] {
            assert_eq!(validate_jobs(&apart), [], "{apart:?}");
        }
    }

    #[test]
    fn a_mirror_and_a_repository_may_not_overlap_but_repositories_may_be_shared() {
        let repo = drive("BACKUP", None, "Replica/Repo");
        let versioned = |name: &str, target: Target| Job {
            targets: vec![target],
            ..valid_job(name)
        };
        for jobs in [
            [
                versioned("repo", repo.clone()),
                mirror("m", drive("BACKUP", None, "Replica/Repo/data")),
            ],
            [
                mirror("m", drive("BACKUP", None, "Replica")),
                versioned("repo", repo.clone()),
            ],
        ] {
            let errors = validate_jobs(&jobs);
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(
                errors[0].job.as_deref(),
                Some("m"),
                "reported on the mirror"
            );
            assert!(matches!(
                &errors[0].problem,
                Problem::MirrorOverlapsRepository { other, .. } if other == "repo"
            ));
            assert!(
                errors[0].to_string().contains("repository"),
                "{}",
                errors[0]
            );
        }
        assert_eq!(
            validate_jobs(&[versioned("one", repo.clone()), versioned("two", repo)]),
            [],
            "two versioned jobs sharing a repository are the supported case"
        );
    }

    #[test]
    fn duplicate_names_ignore_case() {
        let errors = validate_jobs(&[valid_job("Hive"), valid_job("hive")]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].problem, Problem::NameDuplicate("hive".into()));
        assert_eq!(
            errors[0].to_string(),
            "Job hive: another job is also called \"hive\" (names ignore case)."
        );
    }

    #[test]
    fn needs_sources_and_targets() {
        let job = Job {
            name: "x".into(),
            ..Job::default()
        };
        assert_eq!(problems(job), [Problem::NoSources, Problem::NoTargets]);
        let nameless = validate_jobs(&[Job::default()]);
        assert_eq!(nameless[0].job.as_deref(), Some("jobs[0]"));
        assert_eq!(nameless[0].problem, Problem::NameMissing);
    }

    #[test]
    fn mirror_needs_the_deletion_warning_confirmed() {
        let mut job = valid_job("m");
        job.mode = Mode::Mirror;
        assert_eq!(problems(job.clone()), [Problem::MirrorNotConfirmed]);
        job.mirror_deletes_confirmed = true;
        assert_eq!(problems(job.clone()), []);
        job.after.push(AfterStep::Verify {
            schedule: None,
            read_data_subset: "5%".into(),
        });
        assert_eq!(problems(job), [Problem::VerifyOnMirror]);
    }

    #[test]
    fn targets_and_sources_are_checked() {
        let mut job = valid_job("t");
        job.sources.push(Source {
            path: "relative/dir".into(),
            exclude: vec!["{a,b}".into()],
        });
        job.targets = vec![
            Target::Drive {
                name: None,
                volume: VolumeRef::default(),
                path: r"\abs".into(),
            },
            Target::Unc {
                name: None,
                path: r"\\nas".into(),
                credential: Some(" ".into()),
            },
            Target::Unc {
                name: None,
                path: r"\\nas\share\x".into(),
                credential: Some("nas-login".into()),
            },
        ];
        let p = problems(job);
        assert!(matches!(p[0], Problem::SourceNotAbsolute(_)));
        assert!(matches!(p[1], Problem::BadExclude(_)));
        assert_eq!(p[2], Problem::DriveWithoutVolume);
        assert!(matches!(p[3], Problem::DrivePathOutside(_)));
        assert!(matches!(p[4], Problem::UncPathInvalid(_)));
        assert_eq!(p[5], Problem::CredentialEmpty);
        assert_eq!(p.len(), 6);
    }

    #[test]
    fn a_share_login_can_t_be_a_repository_password() {
        let mut job = valid_job("r");
        job.targets = vec![Target::Unc {
            name: None,
            path: r"\\nas\share".into(),
            credential: Some(r" Replica:Repo:drive:label=backup:replica\hive".into()),
        }];
        let p = problems(job);
        assert!(matches!(&p[..], [Problem::CredentialReserved(_)]), "{p:?}");
    }

    #[test]
    fn the_mirror_delete_limit_is_a_percentage() {
        let mut job = valid_job("m");
        assert_eq!(job.mirror_delete_limit_percent, 30, "the default");
        job.mirror_delete_limit_percent = 100;
        assert_eq!(problems(job.clone()), []);
        job.mirror_delete_limit_percent = 101;
        assert_eq!(problems(job), [Problem::DeleteLimitOver100]);
    }

    #[test]
    fn steps_and_retention_are_checked() {
        let mut job = valid_job("s");
        job.before.push(BeforeStep::Command {
            cmd: " ".into(),
            timeout_secs: 0,
        });
        job.retention = Retention {
            daily: 0,
            weekly: 0,
            monthly: 0,
            keep_forever: false,
        };
        assert_eq!(
            problems(job.clone()),
            [
                Problem::RetentionKeepsNothing,
                Problem::EmptyCommand,
                Problem::ZeroTimeout
            ]
        );
        // Keeping everything forever needs no counts.
        job.retention.keep_forever = true;
        assert_eq!(problems(job), [Problem::EmptyCommand, Problem::ZeroTimeout]);
    }

    #[test]
    fn absolute_paths_are_judged_the_windows_way_everywhere() {
        for p in [
            r"C:\x",
            "d:/x",
            r"\\nas\share",
            "//nas/share/x",
            r"\\?\C:\x",
            r"\\?\UNC\nas\share",
            "/home/x",
        ] {
            assert!(is_absolute(std::path::Path::new(p)), "{p}");
        }
        for p in [
            "x", r"x\y", "C:", "C:x", r"\\nas", r"\\?\", r"\\?\x", "//nas",
        ] {
            assert!(!is_absolute(std::path::Path::new(p)), "{p}");
        }
    }

    #[test]
    fn drive_paths_stay_inside_their_volume() {
        for ok in [
            "Replica",
            r"Replica\Hive",
            "Replica/Hive",
            "a/./b",
            "",
            "names..with.dots",
        ] {
            assert!(is_inside_volume(std::path::Path::new(ok)), "{ok}");
        }
        for bad in [
            "C:outside",
            r"C:\x",
            "D:",
            r"Replica\C:x",
            "../outside",
            r"a\..\..\x",
            "a/..",
            r"\x",
            "/x",
            r"\\?\D:\x",
            r"\\nas\share",
            "file:stream",
        ] {
            assert!(!is_inside_volume(std::path::Path::new(bad)), "{bad}");
            let mut job = valid_job("j");
            job.targets = vec![Target::Drive {
                name: None,
                volume: VolumeRef {
                    volume_id: None,
                    label: Some("B".into()),
                },
                path: bad.into(),
            }];
            assert_eq!(
                problems(job),
                [Problem::DrivePathOutside(bad.into())],
                "{bad}"
            );
        }
    }

    #[test]
    fn target_names() {
        let job = valid_job("n");
        assert_eq!(job.targets[0].display_name(), "BACKUP:Replica/Hive");
        let unc = Target::Unc {
            name: None,
            path: r"\\nas\b".into(),
            credential: None,
        };
        assert_eq!(unc.display_name(), r"\\nas\b");
        let named = Target::Unc {
            name: Some("NAS".into()),
            path: r"\\nas\b".into(),
            credential: None,
        };
        assert_eq!(named.display_name(), "NAS");
    }
}
