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
    // 4: a mirror's clashes, each saying what to do, one a line; NULL when it had none.
    "ALTER TABLE run_targets ADD COLUMN clashes TEXT;",
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
    /// A mirror's clashes (a file in one place, a folder in the other), each saying what to do.
    pub clashes: Vec<String>,
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

/// The column the window's history is listed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Started,
    /// The job's name, ignoring case.
    Job,
    /// The result, worse last going up: OK, skipped, partly done, failed.
    Result,
    Duration,
    /// The files backed up, or a test restore's files compared.
    Files,
    /// The size backed up; a test restore has none, which comes first going up.
    Size,
}

/// What the window's history lists ([`History::page`]): runs and test restores together,
/// filtered as [`HistoryFilter`] does and by when they started, in an order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    /// A job's name (any case).
    pub job: Option<String>,
    pub outcome: Option<Outcome>,
    /// Only those started then or later.
    pub since: Option<DateTime<Utc>>,
    /// The column and whether going up; `None`, newest first.
    pub sort: Option<(SortBy, bool)>,
}

impl HistoryQuery {
    /// Its ORDER BY over the listed columns. Ties go newest first, then runs before test
    /// restores, then the later recorded first, so every entry has one place and pages
    /// don't overlap.
    fn order_by(&self) -> String {
        let tie = "started_at DESC, kind ASC, id DESC";
        let Some((column, up)) = self.sort else {
            return tie.to_owned();
        };
        let key = match column {
            SortBy::Started => "started_at",
            SortBy::Job => "unicode_lower(job)",
            SortBy::Result => {
                "CASE outcome WHEN 'ok' THEN 0 WHEN 'skipped' THEN 1 WHEN 'partly' THEN 2 ELSE 3 END"
            }
            SortBy::Duration => "duration_ms",
            SortBy::Files => "files",
            SortBy::Size => "bytes",
        };
        format!("{key} {}, {tie}", if up { "ASC" } else { "DESC" })
    }
}

/// A run or a test restore, as the window's history lists them.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Run(RunRecord),
    Test(TestRestoreRecord),
}

impl Record {
    pub fn outcome(&self) -> Outcome {
        match self {
            Record::Run(r) => r.outcome,
            Record::Test(t) => t.outcome,
        }
    }
}

/// How many entries a [`HistoryQuery`] lists, by kind and by result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub runs: usize,
    pub tests: usize,
    pub ok: usize,
    pub partly: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.runs + self.tests
    }

    /// The entries with `outcome`.
    pub fn of(&self, outcome: Outcome) -> usize {
        match outcome {
            Outcome::Ok => self.ok,
            Outcome::Partly => self.partly,
            Outcome::Failed => self.failed,
            Outcome::Skipped => self.skipped,
        }
    }

    fn add(&mut self, test: bool, outcome: Outcome, n: usize) {
        if test {
            self.tests += n;
        } else {
            self.runs += n;
        }
        match outcome {
            Outcome::Ok => self.ok += n,
            Outcome::Partly => self.partly += n,
            Outcome::Failed => self.failed += n,
            Outcome::Skipped => self.skipped += n,
        }
    }
}

/// A page of the history, and how many entries it's a page of ([`History::page`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HistoryPage {
    pub counts: Counts,
    pub records: Vec<Record>,
}

/// A run's columns, as [`run_from_row`] reads them.
const RUN_COLUMNS: &str =
    "id, job, started_at, duration_ms, outcome, exit_code, files, bytes, error, log_path";

/// A run from a row of [`RUN_COLUMNS`], without its targets.
fn run_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
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
}

/// A test restore's columns, as [`test_from_row`] reads them.
const TEST_COLUMNS: &str =
    "id, job, target, snapshot_id, started_at, duration_ms, outcome, checked, differing, detail";

/// A test restore from a row of [`TEST_COLUMNS`].
fn test_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TestRestoreRecord> {
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
        // Rust's lower case, for ordering by a job's name ignoring case beyond ASCII too, as
        // the window does (SQLite's own `lower` folds ASCII only).
        conn.create_scalar_function(
            "unicode_lower",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| Ok(ctx.get::<String>(0)?.to_lowercase()),
        )?;
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
                "INSERT INTO run_targets (run_id, position, target, mode, outcome, files, bytes, reason, snapshot_id, skipped, clashes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                    (!t.clashes.is_empty()).then(|| t.clashes.iter().map(|c| c.advice()).collect::<Vec<_>>().join("\n")),
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS}
             FROM runs WHERE (?1 IS NULL OR job = ?1 COLLATE NOCASE) AND (?2 IS NULL OR outcome = ?2)
             ORDER BY started_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(filter.params(limit), run_from_row)?;
        let mut runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for run in &mut runs {
            run.targets = self.targets_of(run.id)?;
        }
        Ok(runs)
    }

    fn targets_of(&self, run_id: i64) -> Result<Vec<TargetRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT target, mode, outcome, files, bytes, reason, snapshot_id, skipped, clashes
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
                clashes: r
                    .get::<_, Option<String>>(8)?
                    .map(|c| c.lines().map(str::to_owned).collect())
                    .unwrap_or_default(),
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TEST_COLUMNS}
             FROM test_restores
             WHERE (?1 IS NULL OR job = ?1 COLLATE NOCASE) AND (?2 IS NULL OR outcome = ?2)
             ORDER BY started_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(filter.params(limit), test_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// A page of the history as `query` lists it: runs and test restores together, filtered,
    /// in its order, `limit` of them from `offset`, and how many there are of each kind and
    /// result. Only that page's records are loaded.
    ///
    /// With `matches` (the window's search), the filtered entries are read one at a time in
    /// that order and only those it keeps count: the page is the matches from `offset`. One
    /// entry and the page are all that's held, whatever the search matches; but every filtered
    /// entry is read, so a search costs a pass over them.
    pub fn page(
        &self,
        query: &HistoryQuery,
        offset: usize,
        limit: usize,
        mut matches: Option<&mut dyn FnMut(&Record) -> bool>,
    ) -> Result<HistoryPage> {
        let job = query.job.as_deref();
        let outcome = query.outcome.map(Outcome::as_str);
        let since = query.since.map(|s| s.to_rfc3339());
        let filter = "(?1 IS NULL OR job = ?1 COLLATE NOCASE) AND (?2 IS NULL OR outcome = ?2)
                      AND (?3 IS NULL OR started_at >= ?3)";
        let listed = format!(
            "SELECT 0 AS kind, id, job, started_at, duration_ms, outcome, files, bytes
             FROM runs WHERE {filter}
             UNION ALL
             SELECT 1, id, job, started_at, duration_ms, outcome, checked, NULL
             FROM test_restores WHERE {filter}"
        );
        let order = query.order_by();
        let mut page = HistoryPage::default();
        let window = offset..offset.saturating_add(limit);
        let Some(matches) = matches.as_mut() else {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT kind, outcome, COUNT(*) FROM ({listed}) GROUP BY kind, outcome"
            ))?;
            let mut rows = stmt.query(params![job, outcome, since])?;
            while let Some(r) = rows.next()? {
                let n = r.get::<_, i64>(2)? as usize;
                page.counts.add(
                    r.get::<_, i64>(0)? == 1,
                    Outcome::parse(&r.get::<_, String>(1)?),
                    n,
                );
            }
            let mut stmt = self.conn.prepare(&format!(
                "SELECT kind, id FROM ({listed}) ORDER BY {order} LIMIT ?4 OFFSET ?5"
            ))?;
            let keys = stmt
                .query_map(
                    params![
                        job,
                        outcome,
                        since,
                        limit.min(i64::MAX as usize) as i64,
                        offset.min(i64::MAX as usize) as i64
                    ],
                    |r| Ok((r.get::<_, i64>(0)? == 1, r.get::<_, i64>(1)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (test, id) in keys {
                page.records.extend(self.entry(test, id)?);
            }
            return Ok(page);
        };
        // Searched: every filtered entry in order, one at a time; the matches counted, and
        // those in the page kept.
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT kind, id FROM ({listed}) ORDER BY {order}"))?;
        let mut rows = stmt.query(params![job, outcome, since])?;
        let mut found = 0;
        while let Some(r) = rows.next()? {
            let (test, id) = (r.get::<_, i64>(0)? == 1, r.get::<_, i64>(1)?);
            let Some(record) = self.entry(test, id)? else {
                continue;
            };
            if !matches(&record) {
                continue;
            }
            page.counts.add(test, record.outcome(), 1);
            if window.contains(&found) {
                page.records.push(record);
            }
            found += 1;
        }
        Ok(page)
    }

    /// A run (or, if `test`, a test restore) by its ID, if it's there.
    fn entry(&self, test: bool, id: i64) -> Result<Option<Record>> {
        if test {
            let record = self
                .conn
                .query_row(
                    &format!("SELECT {TEST_COLUMNS} FROM test_restores WHERE id = ?1"),
                    [id],
                    test_from_row,
                )
                .optional()?;
            Ok(record.map(Record::Test))
        } else {
            let run = self
                .conn
                .query_row(
                    &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"),
                    [id],
                    run_from_row,
                )
                .optional()?;
            let Some(mut run) = run else {
                return Ok(None);
            };
            run.targets = self.targets_of(run.id)?;
            Ok(Some(Record::Run(run)))
        }
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

/// [`History::page`] over records already in memory: the same filter, order and paging,
/// for a test's stand-in for the history.
pub fn page_in_memory(
    records: Vec<Record>,
    query: &HistoryQuery,
    offset: usize,
    limit: usize,
    mut matches: Option<&mut dyn FnMut(&Record) -> bool>,
) -> HistoryPage {
    let mut all: Vec<Record> = records
        .into_iter()
        .filter(|r| {
            let (job, outcome, started) = match r {
                Record::Run(r) => (&r.job, r.outcome, r.started_at),
                Record::Test(t) => (&t.job, t.outcome, t.started_at),
            };
            query
                .job
                .as_ref()
                .is_none_or(|j| j.eq_ignore_ascii_case(job))
                && query.outcome.is_none_or(|o| o == outcome)
                && query.since.is_none_or(|s| started >= s)
        })
        .collect();
    all.sort_by(|a, b| {
        let (ka, kb) = (sort_keys(a), sort_keys(b));
        let primary = match query.sort {
            None => std::cmp::Ordering::Equal,
            Some((column, up)) => {
                let o = match column {
                    SortBy::Started => ka.0.cmp(&kb.0),
                    SortBy::Job => ka.3.cmp(&kb.3),
                    SortBy::Result => ka.4.cmp(&kb.4),
                    SortBy::Duration => ka.5.cmp(&kb.5),
                    SortBy::Files => ka.6.cmp(&kb.6),
                    SortBy::Size => ka.7.cmp(&kb.7),
                };
                if up { o } else { o.reverse() }
            }
        };
        // The ties, as `HistoryQuery::order_by`'s: newest first, runs first, later recorded
        // first.
        primary
            .then(kb.0.cmp(&ka.0))
            .then(ka.1.cmp(&kb.1))
            .then(kb.2.cmp(&ka.2))
    });
    let mut page = HistoryPage::default();
    let mut found = 0;
    for record in all {
        if matches.as_mut().is_some_and(|m| !m(&record)) {
            continue;
        }
        page.counts
            .add(matches!(record, Record::Test(_)), record.outcome(), 1);
        if found >= offset && found - offset < limit {
            page.records.push(record);
        }
        found += 1;
    }
    page
}

/// An entry's sort keys: started, whether a test restore, ID, job (lower case), result's rank
/// (worse higher), duration, files, size.
type SortKeys = (
    DateTime<Utc>,
    bool,
    i64,
    String,
    u8,
    Duration,
    u64,
    Option<u64>,
);

fn sort_keys(r: &Record) -> SortKeys {
    let rank = |o: Outcome| match o {
        Outcome::Ok => 0,
        Outcome::Skipped => 1,
        Outcome::Partly => 2,
        Outcome::Failed => 3,
    };
    match r {
        Record::Run(r) => (
            r.started_at,
            false,
            r.id,
            r.job.to_lowercase(),
            rank(r.outcome),
            r.duration,
            r.files,
            Some(r.bytes),
        ),
        Record::Test(t) => (
            t.started_at,
            true,
            t.id,
            t.job.to_lowercase(),
            rank(t.outcome),
            t.duration,
            t.checked,
            None,
        ),
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
            clashes: t.clashes.iter().map(|c| c.advice()).collect(),
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
        assert!(
            old.targets[0].clashes.is_empty(),
            "no clashes column before"
        );
        assert_eq!(old.skipped(), None);
        assert_eq!(old.outcome, Outcome::Partly);
    }

    /// Each released version's history, as that release wrote it (`fixtures/releases`; a
    /// release adds its own): two runs, a test restore and a verify. 0.1.0's mirror had no
    /// clashes column yet.
    #[test]
    fn every_released_history_opens_with_every_row_kept() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/releases");
        for (version, has_clashes) in [("0.1.0", false), ("0.2.0", true), ("0.3.0", true)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.db");
            std::fs::copy(fixtures.join(version).join("history.db"), &path).unwrap();
            let history = History::open(&path).unwrap_or_else(|e| panic!("{version}: {e}"));
            assert_eq!(history.version().unwrap(), MIGRATIONS.len(), "{version}");
            let t0 = parse_time("2026-10-20T01:30:00+00:00");

            let runs = history.recent(None, 10).unwrap();
            assert_eq!(runs.len(), 2, "{version}");
            let failed = &runs[0];
            assert_eq!(
                (failed.job.as_str(), failed.outcome, failed.exit_code),
                ("photos-mirror", Outcome::Failed, 1),
                "{version}"
            );
            assert_eq!(failed.started_at, t0 + chrono::Duration::hours(1));
            assert_eq!(failed.duration, Duration::from_millis(1500));
            assert_eq!(failed.error.as_deref(), Some("The before step failed."));
            assert_eq!(failed.targets, []);

            let run = &runs[1];
            assert_eq!(
                (run.job.as_str(), run.outcome, run.exit_code),
                ("hive", Outcome::Partly, 2),
                "{version}"
            );
            assert_eq!(run.started_at, t0);
            assert_eq!(run.duration, Duration::from_millis(83_250));
            assert_eq!((run.files, run.bytes), (1244, 5_680_949), "{version}");
            assert_eq!(run.error, None);
            assert_eq!(
                run.log_path.as_deref(),
                Some(Path::new(
                    r"C:\Users\Alex\AppData\Roaming\Replica\logs\hive.log"
                ))
            );
            assert_eq!(run.skipped(), Some(2), "{version}");
            let mirror = &run.targets[1];
            assert_eq!(
                run.targets[0],
                TargetRecord {
                    target: "External HDD".into(),
                    mode: "versioned".into(),
                    outcome: Outcome::Ok,
                    files: 1234,
                    bytes: 5_678_901,
                    reason: None,
                    snapshot_id: Some("4f2a9c1e".into()),
                    skipped: Some(0),
                    clashes: vec![],
                },
                "{version}"
            );
            assert_eq!(
                TargetRecord {
                    clashes: vec![],
                    ..mirror.clone()
                },
                TargetRecord {
                    target: r"\\nas\backup\photos".into(),
                    mode: "mirror".into(),
                    outcome: Outcome::Partly,
                    files: 10,
                    bytes: 2048,
                    reason: Some("2 files couldn't be copied.".into()),
                    snapshot_id: None,
                    skipped: Some(2),
                    clashes: vec![],
                },
                "{version}"
            );
            if has_clashes {
                assert_eq!(mirror.clashes.len(), 1, "{version}");
                assert!(
                    mirror.clashes[0].contains(r"E:\Mirror\2024"),
                    "{version}: {:?}",
                    mirror.clashes
                );
            } else {
                assert_eq!(mirror.clashes, [] as [String; 0], "{version}");
            }

            let filter = HistoryFilter::default();
            assert_eq!(
                history.test_restores(&filter, 10).unwrap(),
                [TestRestoreRecord {
                    id: 1,
                    job: "hive".into(),
                    target: "External HDD".into(),
                    snapshot_id: Some("4f2a9c1e".into()),
                    started_at: t0 + chrono::Duration::hours(2),
                    duration: Duration::from_millis(4200),
                    outcome: Outcome::Ok,
                    checked: 20,
                    differing: 0,
                    detail: None,
                }],
                "{version}"
            );
            assert_eq!(
                history.last_verify("hive", "External HDD").unwrap(),
                Some(t0 + chrono::Duration::hours(3)),
                "{version}"
            );
            assert_eq!(history.last_success("hive").unwrap(), Some(t0), "{version}");
            assert_eq!(history.job_names().unwrap(), ["hive", "photos-mirror"]);
        }
    }

    /// Writes this version's `history.db` into `fixtures/releases/<version>`, with the rows
    /// [`every_released_history_opens_with_every_row_kept`] checks (docs/RELEASING.md);
    /// ignored, so test runs leave the committed files alone.
    #[test]
    #[ignore = "writes this version's release fixtures (docs/RELEASING.md)"]
    fn write_release_fixtures() {
        use crate::mirror::{ClashKind, MirrorClash};
        let out = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/releases")
            .join(env!("CARGO_PKG_VERSION"));
        std::fs::create_dir_all(&out).unwrap();
        let path = out.join("history.db");
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("{e}"),
            _ => {}
        }
        let mut history = History::open(&path).unwrap();
        let t0 = parse_time("2026-10-20T01:30:00+00:00");
        let report = RunReport {
            id: None,
            job: "hive".into(),
            started_at: t0,
            duration: Duration::from_millis(83_250),
            outcome: Outcome::Partly,
            error: None,
            targets: vec![
                TargetReport {
                    target: "External HDD".into(),
                    mode: "versioned",
                    outcome: Outcome::Ok,
                    reason: None,
                    files: 1234,
                    bytes: 5_678_901,
                    snapshot_id: Some("4f2a9c1e".into()),
                    problems: vec![],
                    clashes: vec![],
                },
                TargetReport {
                    target: r"\\nas\backup\photos".into(),
                    mode: "mirror",
                    outcome: Outcome::Partly,
                    reason: Some("2 files couldn't be copied.".into()),
                    files: 10,
                    bytes: 2048,
                    snapshot_id: None,
                    problems: vec![r"D:\Photos\locked.jpg".into(), r"D:\Photos\2024".into()],
                    clashes: vec![MirrorClash {
                        path: r"\\?\E:\Mirror\2024".into(),
                        in_mirror: ClashKind::Folder,
                    }],
                },
            ],
            log_path: Some(r"C:\Users\Alex\AppData\Roaming\Replica\logs\hive.log".into()),
            waits: vec![],
            gave_way: false,
        };
        history.record(&report).unwrap();
        history
            .record(&RunReport {
                job: "photos-mirror".into(),
                started_at: t0 + chrono::Duration::hours(1),
                duration: Duration::from_millis(1500),
                outcome: Outcome::Failed,
                error: Some("The before step failed.".into()),
                targets: vec![],
                log_path: None,
                ..report
            })
            .unwrap();
        history
            .record_test_restore(&TestRestoreRecord {
                id: 0,
                job: "hive".into(),
                target: "External HDD".into(),
                snapshot_id: Some("4f2a9c1e".into()),
                started_at: t0 + chrono::Duration::hours(2),
                duration: Duration::from_millis(4200),
                outcome: Outcome::Ok,
                checked: 20,
                differing: 0,
                detail: None,
            })
            .unwrap();
        history
            .record_verify("hive", "External HDD", t0 + chrono::Duration::hours(3))
            .unwrap();
    }

    // --- Paging (the window's history) --------------------------------------------------

    use crate::run::TargetReport;

    // Names beyond ASCII in both cases: "Я" sorts after "а" ignoring case, before it by bytes.
    const JOBS: [&str; 5] = ["docs", "Music", "photos", "Яблоко", "архив"];
    const OUTCOMES: [Outcome; 4] = [
        Outcome::Ok,
        Outcome::Failed,
        Outcome::Partly,
        Outcome::Skipped,
    ];

    /// A history of `runs` runs and `tests` test restores, every few at the same moment (so
    /// ties are ordered too), with jobs, results, durations, files and sizes that vary.
    fn mixed(runs: usize, tests: usize) -> History {
        let mut history = History::in_memory().unwrap();
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |i: usize| t0 + chrono::Duration::minutes((i / 3) as i64 * 7);
        for i in 0..runs {
            history
                .record(&RunReport {
                    id: None,
                    job: JOBS[i % JOBS.len()].into(),
                    started_at: at(i),
                    duration: Duration::from_secs((i * 37 % 101) as u64),
                    outcome: OUTCOMES[i % 4],
                    error: (i % 5 == 0).then(|| format!("error {i}")),
                    targets: vec![TargetReport {
                        target: format!("drive-{}", i % 2),
                        mode: "versioned",
                        outcome: OUTCOMES[i % 4],
                        reason: Some(format!("reason {i}")),
                        files: (i * 13 % 97) as u64,
                        bytes: (i * 7919 % 10007) as u64,
                        snapshot_id: None,
                        problems: Vec::new(),
                        clashes: Vec::new(),
                    }],
                    log_path: None,
                    waits: Vec::new(),
                    gave_way: false,
                })
                .unwrap();
        }
        for i in 0..tests {
            history
                .record_test_restore(&TestRestoreRecord {
                    id: 0,
                    job: JOBS[(i + 1) % JOBS.len()].into(),
                    target: "nas".into(),
                    snapshot_id: None,
                    started_at: at(i * 5),
                    duration: Duration::from_secs((i * 11 % 53) as u64),
                    outcome: OUTCOMES[(i + 1) % 4],
                    checked: (i * 3 % 41) as u64,
                    differing: 0,
                    detail: Some(format!("detail {i}")),
                })
                .unwrap();
        }
        history
    }

    /// Every entry `query` lets through, in its order, worked out in memory from all of them.
    fn expected(history: &History, query: &HistoryQuery) -> Vec<Record> {
        let all: Vec<Record> = history
            .runs(&HistoryFilter::default(), usize::MAX)
            .unwrap()
            .into_iter()
            .map(Record::Run)
            .chain(
                history
                    .test_restores(&HistoryFilter::default(), usize::MAX)
                    .unwrap()
                    .into_iter()
                    .map(Record::Test),
            )
            .collect();
        page_in_memory(all, query, 0, usize::MAX, None).records
    }

    /// Every page of `query`, `size` a page, joined.
    fn all_pages(history: &History, query: &HistoryQuery, size: usize) -> (Counts, Vec<Record>) {
        let mut joined = Vec::new();
        let first = history.page(query, 0, size, None).unwrap();
        let counts = first.counts;
        joined.extend(first.records);
        let mut offset = size;
        while offset < counts.total() {
            let page = history.page(query, offset, size, None).unwrap();
            assert_eq!(page.counts, counts, "every page counts the same");
            assert!(page.records.len() <= size);
            joined.extend(page.records);
            offset += size;
        }
        (counts, joined)
    }

    #[test]
    fn the_pages_of_every_order_and_filter_join_up_to_the_whole_list() {
        let history = mixed(260, 52);
        let since = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut queries = vec![
            HistoryQuery::default(),
            HistoryQuery {
                job: Some("MUSIC".into()),
                ..Default::default()
            },
            HistoryQuery {
                outcome: Some(Outcome::Failed),
                ..Default::default()
            },
            HistoryQuery {
                since: Some(since),
                ..Default::default()
            },
        ];
        for column in [
            SortBy::Started,
            SortBy::Job,
            SortBy::Result,
            SortBy::Duration,
            SortBy::Files,
            SortBy::Size,
        ] {
            for up in [true, false] {
                queries.push(HistoryQuery {
                    sort: Some((column, up)),
                    ..Default::default()
                });
            }
        }
        for query in &queries {
            let want = expected(&history, query);
            for size in [25, 50, 100] {
                let (counts, joined) = all_pages(&history, query, size);
                assert_eq!(counts.total(), want.len(), "{query:?}");
                assert_eq!(joined, want, "{query:?}, {size} a page");
            }
        }
        // Everything: 260 runs and 52 test restores, by result.
        let counts = history
            .page(&HistoryQuery::default(), 0, 50, None)
            .unwrap()
            .counts;
        assert_eq!((counts.runs, counts.tests, counts.total()), (260, 52, 312));
        assert_eq!(
            counts.ok + counts.failed + counts.partly + counts.skipped,
            312
        );
        assert_eq!(counts.of(Outcome::Failed), 65 + 13);
        // Past the end: none, still counted.
        let past = history
            .page(&HistoryQuery::default(), 400, 50, None)
            .unwrap();
        assert!(past.records.is_empty());
        assert_eq!(past.counts.total(), 312);
    }

    #[test]
    fn a_search_counts_its_matches_and_keeps_only_the_page() {
        let history = mixed(260, 52);
        let query = HistoryQuery {
            sort: Some((SortBy::Duration, true)),
            ..Default::default()
        };
        let want = expected(&history, &query);
        // A search that matches everything, page 4 at 25: rows 76 to 100, and 312 counted.
        let mut seen = 0;
        let mut everything = |_: &Record| {
            seen += 1;
            true
        };
        let page = history.page(&query, 75, 25, Some(&mut everything)).unwrap();
        assert_eq!(seen, 312, "each entry read once");
        assert_eq!(page.counts.total(), 312);
        assert_eq!(page.records, want[75..100]);
        // A narrower one: the failed music runs, and the counts of only those.
        let failed_music = |r: &Record| matches!(r, Record::Run(r) if r.job == "Music" && r.outcome == Outcome::Failed);
        let mut matcher = failed_music;
        let page = history.page(&query, 0, 10, Some(&mut matcher)).unwrap();
        let wanted: Vec<&Record> = want.iter().filter(|r| failed_music(r)).collect();
        assert_eq!(page.counts.total(), wanted.len());
        assert_eq!((page.counts.runs, page.counts.tests), (wanted.len(), 0));
        assert_eq!(page.counts.failed, wanted.len());
        assert_eq!(page.records.iter().collect::<Vec<_>>(), wanted[..10]);
    }

    /// How long a search takes over a big history: 10,000 entries, each read and matched
    /// (`cargo test -p replica-core search_time -- --ignored --nocapture`). Not a test of
    /// speed: a figure to report.
    #[test]
    #[ignore = "a timing to report, not a check"]
    fn search_time() {
        let history = mixed(8_000, 2_000);
        let start = std::time::Instant::now();
        let mut text = |r: &Record| format!("{r:?}").contains("reason 9999");
        let page = history
            .page(&HistoryQuery::default(), 0, 50, Some(&mut text))
            .unwrap();
        println!(
            "searched 10000 entries in {:?} ({} found)",
            start.elapsed(),
            page.counts.total()
        );
    }
}
