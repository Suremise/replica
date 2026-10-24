//! A test restore: restore a few random files of a job's latest snapshot to a scratch folder
//! and compare each, byte for byte, with its source.
//!
//! A file whose source has changed since the snapshot (another size or modified time) isn't
//! compared: it's expected to differ. One with the same size and time but other bytes is
//! what this is for: the backup doesn't hold what the source does.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};

use crate::cancel::CancelToken;
use crate::history::TestRestoreRecord;
use crate::restic::{Node, Repository, Restic, ResticError, RestoreRequest, local_path};
use crate::run::Outcome;

/// How many files a test restore picks, unless asked otherwise.
pub const DEFAULT_COUNT: usize = 5;
/// A source's modified time may be this far from the snapshot's and still count as the same
/// (file systems keep times to different precisions).
const TIME_SLACK: Duration = Duration::from_secs(2);

/// What one file came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The restored file is the same as the source.
    Same,
    /// The source looks unchanged since the snapshot, but the restored file differs from it.
    Differs,
    /// The source changed after the snapshot (size or time), so it wasn't compared.
    ChangedSince,
    /// The source isn't there any more, so it wasn't compared.
    SourceGone,
    /// restic didn't restore it.
    NotRestored,
}

/// One file of a test restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedFile {
    /// Its path in the snapshot (`/C/Work/a.txt`).
    pub item: String,
    /// Its source on disk.
    pub source: PathBuf,
    pub check: Check,
}

/// A finished test restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRestore {
    pub snapshot_id: String,
    pub files: Vec<CheckedFile>,
}

impl TestRestore {
    pub fn count(&self, check: Check) -> usize {
        self.files.iter().filter(|f| f.check == check).count()
    }

    /// Failed when a file differs or wasn't restored; skipped when nothing could be compared
    /// (no files, or every source changed or gone); otherwise ok.
    pub fn outcome(&self) -> Outcome {
        if self.count(Check::Differs) > 0 || self.count(Check::NotRestored) > 0 {
            Outcome::Failed
        } else if self.count(Check::Same) == 0 {
            Outcome::Skipped
        } else {
            Outcome::Ok
        }
    }

    /// As the history keeps it. `detail` lists the files that differ or weren't restored.
    pub fn record(
        &self,
        job: &str,
        target: &str,
        started_at: DateTime<Utc>,
        duration: Duration,
    ) -> TestRestoreRecord {
        let bad: Vec<&str> = self
            .files
            .iter()
            .filter(|f| matches!(f.check, Check::Differs | Check::NotRestored))
            .map(|f| f.item.as_str())
            .collect();
        TestRestoreRecord {
            id: 0,
            job: job.to_owned(),
            target: target.to_owned(),
            snapshot_id: Some(self.snapshot_id.clone()),
            started_at,
            duration,
            outcome: self.outcome(),
            checked: (self.count(Check::Same) + self.count(Check::Differs)) as u64,
            differing: self.count(Check::Differs) as u64,
            detail: (!bad.is_empty()).then(|| bad.join("\n")),
        }
    }
}

/// A small random number source (xorshift), seeded by the caller: enough to pick files, and
/// a test can fix the seed.
pub struct Picker(u64);

impl Picker {
    pub fn new(seed: u64) -> Self {
        Picker(seed | 1)
    }

    /// Seeded from the clock.
    pub fn from_clock() -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Picker::new(nanos)
    }

    /// A number below `n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x % n.max(1)
    }
}

/// Restores up to `count` random files of `job`'s latest snapshot under `scratch` (an empty
/// folder the caller removes) and compares each with its source.
pub fn run(
    restic: &Restic,
    repo: &Repository,
    job: &str,
    count: usize,
    scratch: &Path,
    picker: &mut Picker,
    cancel: &CancelToken,
) -> Result<TestRestore, ResticError> {
    let snapshot_id = restic.job_snapshot(repo, job, "latest")?;
    let nodes = restic.sample_files(repo, &snapshot_id, count, &mut |n| picker.below(n), cancel)?;
    let mut files = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        // Each in a folder of its own: two picked files may have the same name.
        let into = scratch.join(i.to_string());
        let request = RestoreRequest {
            job,
            snapshot: &snapshot_id,
            item: Some(&node.path),
            target: &into,
            sources: &[],
            allow_over_source: false,
        };
        let restored = match restic.restore(repo, &request, cancel) {
            Ok(_) => Some(into.join(&node.name)),
            Err(ResticError::Cancelled) => return Err(ResticError::Cancelled),
            Err(ResticError::RestoreNothing { .. }) => None,
            Err(e) => return Err(e),
        };
        let source = local_path(&node.path);
        let check = match restored {
            None => Check::NotRestored,
            Some(restored) => compare(node, &source, &restored),
        };
        files.push(CheckedFile {
            item: node.path.clone(),
            source,
            check,
        });
    }
    Ok(TestRestore { snapshot_id, files })
}

/// Runs [`run`] in a scratch folder of its own under `temp`, removed afterwards, and times it.
pub fn run_in_temp(
    restic: &Restic,
    repo: &Repository,
    job: &str,
    temp: &Path,
    cancel: &CancelToken,
) -> Result<(TestRestore, DateTime<Utc>, Duration), ResticError> {
    let started_at = Utc::now();
    let clock = Instant::now();
    let scratch = temp.join(format!(
        "replica-test-restore-{}-{}",
        std::process::id(),
        started_at.timestamp_nanos_opt().unwrap_or_default()
    ));
    let result = run(
        restic,
        repo,
        job,
        DEFAULT_COUNT,
        &scratch,
        &mut Picker::from_clock(),
        cancel,
    );
    let _ = std::fs::remove_dir_all(&scratch);
    result.map(|r| (r, started_at, clock.elapsed()))
}

fn compare(node: &Node, source: &Path, restored: &Path) -> Check {
    let Ok(meta) = std::fs::metadata(source) else {
        return Check::SourceGone;
    };
    let same_time = match (node.mtime, meta.modified()) {
        (Some(snap), Ok(now)) => {
            let now: DateTime<Utc> = now.into();
            (now - snap.with_timezone(&Utc))
                .abs()
                .to_std()
                .is_ok_and(|d| d <= TIME_SLACK)
        }
        _ => false,
    };
    if meta.len() != node.size || !same_time {
        return Check::ChangedSince;
    }
    match same_bytes(source, restored) {
        Ok(true) => Check::Same,
        Ok(false) => Check::Differs,
        Err(_) => Check::NotRestored,
    }
}

fn same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (mut a, mut b) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut a, &mut x)?;
        let m = read_full(&mut b, &mut y)?;
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends.
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(check: Check) -> CheckedFile {
        CheckedFile {
            item: format!("/C/{check:?}"),
            source: PathBuf::new(),
            check,
        }
    }

    fn of(checks: &[Check]) -> TestRestore {
        TestRestore {
            snapshot_id: "abc".into(),
            files: checks.iter().map(|c| file(*c)).collect(),
        }
    }

    #[test]
    fn the_outcome_follows_the_files() {
        assert_eq!(
            of(&[Check::Same, Check::ChangedSince]).outcome(),
            Outcome::Ok
        );
        assert_eq!(
            of(&[Check::Same, Check::Differs]).outcome(),
            Outcome::Failed
        );
        assert_eq!(of(&[Check::NotRestored]).outcome(), Outcome::Failed);
        assert_eq!(
            of(&[Check::ChangedSince, Check::SourceGone]).outcome(),
            Outcome::Skipped
        );
        assert_eq!(of(&[]).outcome(), Outcome::Skipped);
    }

    #[test]
    fn the_record_counts_and_names_the_bad_files() {
        let r = of(&[Check::Same, Check::Differs, Check::ChangedSince]).record(
            "job",
            "BACKUP:x",
            Utc::now(),
            Duration::from_secs(1),
        );
        assert_eq!((r.checked, r.differing), (2, 1));
        assert_eq!(r.detail.as_deref(), Some("/C/Differs"));
        assert_eq!(r.outcome, Outcome::Failed);
    }

    #[test]
    fn the_picker_stays_below_its_bound() {
        let mut p = Picker::new(42);
        for n in 1..200 {
            assert!(p.below(n) < n);
        }
    }

    #[test]
    fn bytes_are_compared_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        std::fs::write(&a, &big).unwrap();
        std::fs::write(&b, &big).unwrap();
        assert!(same_bytes(&a, &b).unwrap());
        let mut changed = big.clone();
        changed[150_000] ^= 1;
        std::fs::write(&b, &changed).unwrap();
        assert!(!same_bytes(&a, &b).unwrap());
    }
}
