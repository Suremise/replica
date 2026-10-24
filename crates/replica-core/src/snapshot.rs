//! `sqlite-snapshot` before steps: a consistent copy of a live SQLite database.
//!
//! SQLite's online backup API copies the database in one read transaction, so the copy is a
//! consistent point in time even while another process writes to it (in WAL mode, writers
//! carry on meanwhile). The copy is written to a new file in the staging folder, checked with
//! `PRAGMA integrity_check`, then renamed over the previous copy. The run backs up the copy;
//! [`live_files`] are excluded from the backup.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::exclude::ExcludeRules;
use crate::job::Source;

use crate::tr;

/// How long a snapshot keeps retrying while the database is locked by a writer.
const BUSY_LIMIT: Duration = Duration::from_secs(120);

/// Why a snapshot failed. Its text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotError {
    pub db: PathBuf,
    pub detail: String,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&tr!(
            "error-sqlite-snapshot",
            db = self.db.display().to_string(),
            detail = self.detail.as_str()
        ))
    }
}

impl std::error::Error for SnapshotError {}

/// The live database's files, which the backup leaves out: the database and its `-wal`,
/// `-shm` and `-journal` files.
pub fn live_files(db: &Path) -> Vec<PathBuf> {
    let mut files = vec![db.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        files.push(PathBuf::from(name));
    }
    files
}

/// Copies `db` consistently to `<staging>\<its file name>`, and returns that path.
pub fn sqlite_snapshot(db: &Path, staging: &Path) -> Result<PathBuf, SnapshotError> {
    let err = |detail: String| SnapshotError {
        db: db.to_path_buf(),
        detail,
    };
    let name = db
        .file_name()
        .ok_or_else(|| err(tr!("error-sqlite-no-file-name")))?;
    if !db.is_file() {
        return Err(err(tr!("error-sqlite-missing")));
    }
    std::fs::create_dir_all(staging).map_err(|e| err(e.to_string()))?;
    let copy = staging.join(name);
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    tmp_name.push(format!(
        ".{}-{}.snapshot-tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let tmp = staging.join(tmp_name);
    let _ = std::fs::remove_file(&tmp);

    let result = (|| -> Result<(), String> {
        let src = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| e.to_string())?;
        src.busy_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        let mut dst = Connection::open(&tmp).map_err(|e| e.to_string())?;
        {
            let backup = Backup::new(&src, &mut dst).map_err(|e| e.to_string())?;
            let deadline = Instant::now() + BUSY_LIMIT;
            loop {
                // All pages in one step: one read transaction, so one point in time.
                match backup.step(-1).map_err(|e| e.to_string())? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    StepResult::Busy | StepResult::Locked => {
                        if Instant::now() >= deadline {
                            return Err(tr!("error-sqlite-busy"));
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => {}
                }
            }
        }
        let check: String = dst
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if check != "ok" {
            return Err(check);
        }
        dst.close().map_err(|(_, e)| e.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&tmp, &copy).map_err(|e| err(e.to_string()))?;
            Ok(copy)
        }
        Err(detail) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err(detail))
        }
    }
}

/// Why a job's `sqlite-snapshot` steps can't run. Its text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A copy would be written over a live database or one of its files.
    CopyOverDatabase { copy: PathBuf, db: PathBuf },
    /// A live database (or one of its files) is inside Replica's staging folder, which Replica
    /// tidies: it could be removed.
    DatabaseInStaging { db: PathBuf },
    /// The copy is inside a source whose exclusions leave it out, so it would never be backed
    /// up. Replica's staging folder is only inside a source if a source holds Replica's data.
    CopyExcluded { copy: PathBuf, source: PathBuf },
    /// A path couldn't be resolved, so the plan can't be checked: the run fails before anything
    /// runs.
    Unresolvable { path: PathBuf, detail: String },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = |p: &PathBuf| p.display().to_string();
        f.write_str(&match self {
            PlanError::CopyOverDatabase { copy, db } => {
                tr!("error-snapshot-copy-over-db", copy = p(copy), db = p(db))
            }
            PlanError::DatabaseInStaging { db } => {
                tr!("error-snapshot-db-in-staging", db = p(db))
            }
            PlanError::CopyExcluded { copy, source } => tr!(
                "error-snapshot-copy-excluded",
                copy = p(copy),
                source = p(source)
            ),
            PlanError::Unresolvable { path, detail } => tr!(
                "error-snapshot-unresolvable",
                path = p(path),
                detail = detail.as_str()
            ),
        })
    }
}

impl std::error::Error for PlanError {}

/// What a job's `sqlite-snapshot` steps mean for its backup, worked out before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPlan {
    /// Each database, as the step names it, and the folder its copy goes in.
    pub copies: Vec<(PathBuf, PathBuf)>,
    /// The paths to leave out of the backup: every live file (the database, `-wal`, `-shm`,
    /// `-journal`), as the step names it, by its real path, and as each source that holds it
    /// spells it, so the engines match it however a source reaches it.
    pub live_files: Vec<PathBuf>,
    /// The sources to back up: the job's, without any that is a live file itself, and with the
    /// job's snapshots folder added when no source holds the copies.
    pub sources: Vec<Source>,
}

/// The folder of a job's copies, in its folder in Replica's staging folder. It holds nothing
/// else, so it's backed up as one source (a mirror keeps it as `sqlite-snapshots`).
pub fn snapshots_dir(job_staging: &Path) -> PathBuf {
    job_staging.join("sqlite-snapshots")
}

/// The folder a job's `n`th `sqlite-snapshot` step (from 1) copies into. Each step has its own,
/// so two databases of one name never meet.
pub fn copy_dir(job_staging: &Path, n: usize) -> PathBuf {
    snapshots_dir(job_staging).join(n.to_string())
}

/// Makes a job's snapshots folder hold nothing but this run's planned copies: everything else
/// (copies left by steps since removed or changed, unfinished temporary files) is removed, so a
/// stale copy is never backed up as current. Replica owns the folder, and [`plan`] has refused
/// any live database inside Replica's staging folder, so nothing here is a user's file.
pub fn tidy(job_staging: &Path, plan: &SnapshotPlan) -> std::io::Result<()> {
    let dir = snapshots_dir(job_staging);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let step = plan.copies.iter().find(|(_, d)| d == &path);
        match step {
            Some((db, step_dir)) if entry.file_type()?.is_dir() => {
                // Keep only this step's own copy.
                let keep = db.file_name().unwrap_or_default();
                for inner in std::fs::read_dir(step_dir)? {
                    let inner = inner?;
                    if inner.file_name() != keep {
                        remove(&inner)?;
                    }
                }
            }
            _ => remove(&entry)?,
        }
    }
    Ok(())
}

fn remove(entry: &std::fs::DirEntry) -> std::io::Result<()> {
    if entry.file_type()?.is_dir() {
        std::fs::remove_dir_all(entry.path())
    } else {
        std::fs::remove_file(entry.path())
    }
}

/// Plans a run's copies of `databases` into `job_staging` (`<staging>\<job>`, in Replica's
/// staging folder `staging`), for a job with `sources`. Every path is resolved to where it really
/// is ([`crate::paths::real_path`]: `.` and `..`, relative paths, links and junctions) and
/// compared that way; one that can't be resolved is an error, so the run fails before anything
/// runs. A live database file anywhere inside Replica's staging folder is refused: Replica
/// tidies that folder, so a database there could be removed.
pub fn plan(
    sources: &[Source],
    databases: &[PathBuf],
    staging: &Path,
    job_staging: &Path,
) -> Result<SnapshotPlan, PlanError> {
    let real = |path: &Path| {
        crate::paths::real_path(path).map_err(|e| PlanError::Unresolvable {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })
    };
    // Every live file, really.
    let mut live = Vec::new();
    for db in databases {
        for file in live_files(&real(db)?) {
            live.push((db.clone(), file));
        }
    }
    // No live file inside Replica's staging folder, which Replica tidies.
    let staging_real = real(staging)?;
    if let Some((db, _)) = live
        .iter()
        .find(|(_, l)| relative(l, &staging_real).is_some())
    {
        return Err(PlanError::DatabaseInStaging { db: db.clone() });
    }
    // Every copy: never over a live file.
    let mut copies = Vec::new();
    let mut copy_identities = Vec::new();
    for (i, db) in databases.iter().enumerate() {
        let dir = copy_dir(job_staging, i + 1);
        let copy = dir.join(db.file_name().unwrap_or_default());
        let copy_real = real(&copy)?;
        if let Some((over, _)) = live.iter().find(|(_, l)| same(l, &copy_real)) {
            return Err(PlanError::CopyOverDatabase {
                copy,
                db: over.clone(),
            });
        }
        copy_identities.push((copy, copy_real));
        copies.push((db.clone(), dir));
    }
    // The sources, really, without any that is a live file.
    let mut kept = Vec::new();
    for source in sources {
        let source_real = real(&source.path)?;
        if !live.iter().any(|(_, l)| same(l, &source_real)) {
            kept.push((source.clone(), source_real));
        }
    }
    // Each live file left out however it's reached.
    let mut exclusions: Vec<PathBuf> = Vec::new();
    for db in databases {
        exclusions.extend(live_files(db));
    }
    for (_, file) in &live {
        exclusions.push(file.clone());
        for (source, source_real) in &kept {
            if let Some(rel) = relative(file, source_real).filter(|r| !r.as_os_str().is_empty()) {
                exclusions.push(source.path.join(rel));
            }
        }
    }
    // Each copy backed up: under a source that doesn't exclude it, or in the snapshots folder
    // added as a source of its own.
    let mut planned: Vec<Source> = kept.iter().map(|(s, _)| s.clone()).collect();
    let mut add_folder = false;
    for (copy, copy_real) in &copy_identities {
        match kept
            .iter()
            .find_map(|(s, r)| relative(copy_real, r).map(|rel| (s, rel)))
        {
            Some((source, rel)) => {
                let spelled = source.path.join(rel);
                let rules =
                    ExcludeRules::new([(source.path.as_path(), source.exclude.iter())], true);
                if rules.is_ok_and(|r| r.is_excluded(&spelled)) {
                    return Err(PlanError::CopyExcluded {
                        copy: copy.clone(),
                        source: source.path.clone(),
                    });
                }
            }
            None => add_folder = true,
        }
    }
    if add_folder {
        planned.push(Source {
            path: snapshots_dir(job_staging),
            exclude: Vec::new(),
        });
    }
    Ok(SnapshotPlan {
        copies,
        live_files: exclusions,
        sources: planned,
    })
}

fn key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let text = text
        .strip_prefix("//?/unc/")
        .map(|r| format!("//{r}"))
        .unwrap_or_else(|| text.strip_prefix("//?/").unwrap_or(&text).to_string());
    text.trim_end_matches('/').to_string()
}

fn same(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// `inner`'s path below `outer`, if it's inside it (or is it: an empty path).
fn relative(inner: &Path, outer: &Path) -> Option<PathBuf> {
    let (i, o) = (key(inner), key(outer));
    if i == o {
        return Some(PathBuf::new());
    }
    let rest = i.strip_prefix(&format!("{o}/"))?;
    // The same number of trailing characters of the original text, keeping its case.
    let original = inner.to_string_lossy().replace('\\', "/");
    Some(PathBuf::from(&original[original.len() - rest.len()..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_files_include_the_wal_and_shm() {
        let files = live_files(Path::new("D:/data/hive.db"));
        let names: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            names,
            [
                "D:/data/hive.db",
                "D:/data/hive.db-wal",
                "D:/data/hive.db-shm",
                "D:/data/hive.db-journal"
            ]
        );
    }

    #[test]
    fn a_missing_database_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            sqlite_snapshot(&dir.path().join("none.db"), &dir.path().join("staging")).unwrap_err();
        assert!(err.to_string().contains("none.db"), "{err}");
    }
}
