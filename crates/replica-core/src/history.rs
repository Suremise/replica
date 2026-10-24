//! The run history: SQLite, in typed tables.
//!
//! One row per run and one per target of each run, when each versioned target was last
//! verified, and test restores (kept apart from runs, so a test restore never counts as a job's
//! last run for its schedule). The schema is versioned with `PRAGMA user_version`; [`History::open`] brings an
//! older file up to date, one migration at a time, each in a transaction.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::run::{Outcome, RunReport, TargetReport};

/// The migrations, in order: migration `n` brings the schema from version `n` to `n + 1`.
const MIGRATIONS: &[&str] = &[
    // 1: runs, their targets, and verify times.
    "CREATE TABLE runs (
        id          INTEGER PRIMARY KEY,
        job         TEXT    NOT NULL,
        started_at  TEXT    NOT NULL,
        duration_ms INTEGER NOT NULL,
        outcome     TEXT    NOT NULL,
        exit_code   INTEGER NOT NULL,
        files       INTEGER NOT NULL,
        bytes       INTEGER NOT NULL,
        error       TEXT,
        log_path    TEXT
    );
    CREATE INDEX runs_job_started ON runs (job, started_at);
    CREATE TABLE run_targets (
        run_id      INTEGER NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
        position    INTEGER NOT NULL,
        target      TEXT    NOT NULL,
        mode        TEXT    NOT NULL,
        outcome     TEXT    NOT NULL,
        files       INTEGER NOT NULL,
        bytes       INTEGER NOT NULL,
        reason      TEXT,
        snapshot_id TEXT,
        PRIMARY KEY (run_id, position)
    );
    CREATE TABLE verifies (
        job         TEXT NOT NULL,
        target      TEXT NOT NULL,
        verified_at TEXT NOT NULL,
        PRIMARY KEY (job, target)
    );",
    // 2: test restores.
    "CREATE TABLE test_restores (
        id          INTEGER PRIMARY KEY,
        job         TEXT    NOT NULL,
        target      TEXT    NOT NULL,
        snapshot_id TEXT,
        started_at  TEXT    NOT NULL,
        duration_ms INTEGER NOT NULL,
        outcome     TEXT    NOT NULL,
        checked     INTEGER NOT NULL,
        differing   INTEGER NOT NULL,
        detail      TEXT
    );
    CREATE INDEX test_restores_job_started ON test_restores (job, started_at);",
    // 3: how many files each target couldn't back up or copy; NULL in older rows.
    "ALTER TABLE run_targets ADD COLUMN skipped INTEGER;",
];

/// The history database.
pub struct History {
    conn: Connection,
}

/// A run as the history keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub id: i64,
    pub job: String,
    pub started_at: DateTime<Utc>,
    pub duration: Duration,
    pub outcome: Outcome,
    pub exit_code: i32,
    pub files: u64,
    pub bytes: u64,
    pub error: Option<String>,
    pub log_path: Option<PathBuf>,
    pub targets: Vec<TargetRecord>,
}

impl RunRecord {
    /// How many files its targets couldn't back up or copy, together; `None` if none of
    /// them has the count (rows recorded before the history kept it).
    pub fn skipped(&self) -> Option<u64> {
        self.targets
            .iter()
            .filter_map(|t| t.skipped)
            .reduce(|a, b| a + b)
    }
}

/// One target's part of a run, as the history keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRecord {
    pub target: String,
    pub mode: String,
    pub outcome: Outcome,
    pub files: u64,
    pub bytes: u64,
    pub reason: Option<String>,
    pub snapshot_id: Option<String>,
    /// How many files it couldn't back up or copy; `None` in a row recorded before the
    /// history kept the count.
    pub skipped: Option<u64>,
}

/// A test restore as the history keeps it: some files of a snapshot restored and compared with
/// their sources ([`crate::test_restore`]).
#[derive(Debug, Clone, PartialEq)]
pub struct TestRestoreRecord {
    /// Its ID in the history (ignored when recording).
    pub id: i64,
    pub job: String,
    pub target: String,
    pub snapshot_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub duration: Duration,
    pub outcome: Outcome,
    /// How many files were compared with their source.
    pub checked: u64,
    /// How many of them differ from it.
    pub differing: u64,
    /// What went wrong, or which files differ.
    pub detail: Option<String>,
}

/// Which runs and test restores to read: all, or one job's, or those with one outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    /// A job's name (any case).
    pub job: Option<String>,
    pub outcome: Option<Outcome>,
}

impl HistoryFilter {
    fn params(&self, limit: usize) -> (Option<&str>, Option<&'static str>, i64) {
        (
            self.job.as_deref(),
            self.outcome.map(Outcome::as_str),
            limit.min(i64::MAX as usize) as i64,
        )
    }
}

/// A history file that can't be opened, read or written.
#[derive(Debug)]
pub struct HistoryError(pub rusqlite::Error);

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::tr!("error-history", detail = self.0.to_string()))
    }
}

impl std::error::Error for HistoryError {}

impl From<rusqlite::Error> for HistoryError {
    fn from(e: rusqlite::Error) -> Self {
        HistoryError(e)
    }
}

type Result<T> = std::result::Result<T, HistoryError>;

impl History {
    /// Opens (or creates) the history at `path`, bringing its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        Self::with_connection(Connection::open(path)?)
    }

    /// A history in memory, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let version = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        for (n, migration) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(migration)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", n + 1))?;
            tx.commit()?;
        }
        Ok(History { conn })
    }

    /// The schema version.
    pub fn version(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize)
    }

    /// Records a finished run and its targets, and returns its ID.
    pub fn record(&mut self, report: &RunReport) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO runs (job, started_at, duration_ms, outcome, exit_code, files, bytes, error, log_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                report.job,
                report.started_at.to_rfc3339(),
                report.duration.as_millis() as i64,
                report.outcome.as_str(),
                report.outcome.exit_code(),
                report.files() as i64,
                report.bytes() as i64,
                report.error,
                report.log_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            ],
        )?;
        let id = tx.last_insert_rowid();
        for (position, t) in report.targets.iter().enumerate() {
            tx.execute(
                "INSERT INTO run_targets (run_id, position, target, mode, outcome, files, bytes, reason, snapshot_id, skipped)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
                    position as i64,
                    t.target,
                    t.mode,
                    t.outcome.as_str(),
                    t.files as i64,
                    t.bytes as i64,
                    t.reason,
                    t.snapshot_id,
                    t.problems.len() as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// The most recent runs, newest first; of one job if given.
    pub fn recent(&self, job: Option<&str>, limit: usize) -> Result<Vec<RunRecord>> {
        self.runs(
            &HistoryFilter {
                job: job.map(str::to_owned),
                outcome: None,
            },
            limit,
        )
    }

    /// The most recent runs that `filter` lets through, newest first. The filter is applied
    /// before the limit, so an old run of a job is reached by asking for that job.
    pub fn runs(&self, filter: &HistoryFilter, limit: usize) -> Result<Vec<RunRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job, started_at, duration_ms, outcome, exit_code, files, bytes, error, log_path
             FROM runs WHERE (?1 IS NULL OR job = ?1 COLLATE NOCASE) AND (?2 IS NULL OR outcome = ?2)
             ORDER BY started_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(filter.params(limit), |r| {
            Ok(RunRecord {
                id: r.get(0)?,
                job: r.get(1)?,
                started_at: parse_time(&r.get::<_, String>(2)?),
                duration: Duration::from_millis(r.get::<_, i64>(3)? as u64),
                outcome: Outcome::parse(&r.get::<_, String>(4)?),
                exit_code: r.get(5)?,
                files: r.get::<_, i64>(6)? as u64,
                bytes: r.get::<_, i64>(7)? as u64,
                error: r.get(8)?,
                log_path: r.get::<_, Option<String>>(9)?.map(PathBuf::from),
                targets: Vec::new(),
            })
        })?;
        let mut runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for run in &mut runs {
            run.targets = self.targets_of(run.id)?;
        }
        Ok(runs)
    }

    fn targets_of(&self, run_id: i64) -> Result<Vec<TargetRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT target, mode, outcome, files, bytes, reason, snapshot_id, skipped
             FROM run_targets WHERE run_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map([run_id], |r| {
            Ok(TargetRecord {
                target: r.get(0)?,
                mode: r.get(1)?,
                outcome: Outcome::parse(&r.get::<_, String>(2)?),
                files: r.get::<_, i64>(3)? as u64,
                bytes: r.get::<_, i64>(4)? as u64,
                reason: r.get(5)?,
                snapshot_id: r.get(6)?,
                skipped: r.get::<_, Option<i64>>(7)?.map(|n| n as u64),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// When the job last succeeded, for "overdue": its latest run in which at least one target
    /// backed up or mirrored, that target's own result being "ok" or "partly" (a snapshot or a
    /// mirror was made, perhaps without some files). A run whose targets were all skipped or
    /// failed isn't a success, whatever its steps did.
    pub fn last_success(&self, job: &str) -> Result<Option<DateTime<Utc>>> {
        let at: Option<String> = self
            .conn
            .query_row(
                "SELECT MAX(r.started_at) FROM runs r WHERE r.job = ?1 AND EXISTS (
                     SELECT 1 FROM run_targets t
                     WHERE t.run_id = r.id AND t.outcome IN ('ok', 'partly'))",
                [job],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(at.as_deref().map(parse_time))
    }

    /// When a versioned target of a job was last verified.
    pub fn last_verify(&self, job: &str, target: &str) -> Result<Option<DateTime<Utc>>> {
        let at: Option<String> = self
            .conn
            .query_row(
                "SELECT verified_at FROM verifies WHERE job = ?1 AND target = ?2",
                [job, target],
                |r| r.get(0),
            )
            .optional()?;
        Ok(at.as_deref().map(parse_time))
    }

    /// Records a test restore and returns its ID.
    pub fn record_test_restore(&self, r: &TestRestoreRecord) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO test_restores
                 (job, target, snapshot_id, started_at, duration_ms, outcome, checked, differing, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                r.job,
                r.target,
                r.snapshot_id,
                r.started_at.to_rfc3339(),
                r.duration.as_millis() as i64,
                r.outcome.as_str(),
                r.checked as i64,
                r.differing as i64,
                r.detail,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The most recent test restores that `filter` lets through, newest first.
    pub fn test_restores(
        &self,
        filter: &HistoryFilter,
        limit: usize,
    ) -> Result<Vec<TestRestoreRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job, target, snapshot_id, started_at, duration_ms, outcome, checked, differing, detail
             FROM test_restores
             WHERE (?1 IS NULL OR job = ?1 COLLATE NOCASE) AND (?2 IS NULL OR outcome = ?2)
             ORDER BY started_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(filter.params(limit), |r| {
            Ok(TestRestoreRecord {
                id: r.get(0)?,
                job: r.get(1)?,
                target: r.get(2)?,
                snapshot_id: r.get(3)?,
                started_at: parse_time(&r.get::<_, String>(4)?),
                duration: Duration::from_millis(r.get::<_, i64>(5)? as u64),
                outcome: Outcome::parse(&r.get::<_, String>(6)?),
                checked: r.get::<_, i64>(7)? as u64,
                differing: r.get::<_, i64>(8)? as u64,
                detail: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every job name in the history (runs and test restores), each once ignoring case,
    /// sorted ignoring case.
    pub fn job_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT job FROM runs UNION SELECT job FROM test_restores
             ORDER BY 1 COLLATE NOCASE",
        )?;
        let mut names: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        Ok(names)
    }

    pub fn record_verify(&self, job: &str, target: &str, at: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO verifies (job, target, verified_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (job, target) DO UPDATE SET verified_at = excluded.verified_at",
            params![job, target, at.to_rfc3339()],
        )?;
        Ok(())
    }
}

fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_default()
}

impl From<&TargetReport> for TargetRecord {
    fn from(t: &TargetReport) -> Self {
        TargetRecord {
            target: t.target.clone(),
            mode: t.mode.to_string(),
            outcome: t.outcome,
            files: t.files,
            bytes: t.bytes,
            reason: t.reason.clone(),
            snapshot_id: t.snapshot_id.clone(),
            skipped: Some(t.problems.len() as u64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_2_history_gets_the_skipped_count_and_keeps_its_rows_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        {
            let conn = Connection::open(&path).unwrap();
            for migration in &MIGRATIONS[..2] {
                conn.execute_batch(migration).unwrap();
            }
            conn.execute_batch(
                "PRAGMA user_version = 2;
                 INSERT INTO runs (id, job, started_at, duration_ms, outcome, exit_code, files, bytes)
                 VALUES (1, 'old', '2026-01-01T00:00:00+00:00', 1000, 'partly', 2, 5, 50);
                 INSERT INTO run_targets (run_id, position, target, mode, outcome, files, bytes, reason)
                 VALUES (1, 0, 'BACKUP:old', 'mirror', 'partly', 5, 50, '3 files couldn''t be backed up.');",
            )
            .unwrap();
        }
        let history = History::open(&path).unwrap();
        assert_eq!(history.version().unwrap(), MIGRATIONS.len());
        let old = &history.recent(Some("old"), 1).unwrap()[0];
        assert_eq!(old.targets[0].skipped, None);
        assert_eq!(old.skipped(), None);
        assert_eq!(old.outcome, Outcome::Partly);
    }
}
