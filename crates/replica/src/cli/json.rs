//! The `--json` output: stable, documented in `docs/cli-json.md`, keys never translated.
//! Every document has `"schema": 1`; a change that breaks a reader raises it.

use serde::Serialize;

pub const SCHEMA: u32 = 1;

/// `replica run --json`.
#[derive(Debug, Serialize)]
pub struct Run {
    pub schema: u32,
    pub outcome: &'static str,
    pub exit_code: i32,
    pub jobs: Vec<JobRun>,
}

#[derive(Debug, Serialize)]
pub struct JobRun {
    pub job: String,
    pub run_id: Option<i64>,
    pub outcome: &'static str,
    pub exit_code: i32,
    pub started_at: String,
    pub duration_ms: u64,
    pub files: u64,
    pub bytes: u64,
    pub error: Option<String>,
    pub log: Option<String>,
    /// What the run waited for before it could go on.
    pub waits: Vec<Wait>,
    pub targets: Vec<TargetRun>,
}

#[derive(Debug, Serialize)]
pub struct Wait {
    /// `turn` (another Replica run using the repository) or `restic-lock` (a restic outside
    /// Replica holding its lock).
    pub kind: &'static str,
    pub target: String,
    /// The job it waited for, if it was another job's turn and its name is known.
    pub job: Option<String>,
    pub duration_ms: u64,
    /// How the wait ended: `ok` (the run went on), `timed-out`, `cancelled`, or `gave-way` (it
    /// let a job queued behind it go first, and was tried again later).
    pub ended: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TargetRun {
    pub target: String,
    pub mode: &'static str,
    pub outcome: &'static str,
    pub reason: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub snapshot_id: Option<String>,
}

/// `replica list --json` and `replica status --json`.
#[derive(Debug, Serialize)]
pub struct Jobs {
    pub schema: u32,
    pub jobs: Vec<JobInfo>,
}

#[derive(Debug, Serialize)]
pub struct JobInfo {
    pub name: String,
    pub enabled: bool,
    pub mode: &'static str,
    pub schedule: Option<String>,
    pub targets: Vec<String>,
    pub last_run: Option<LastRun>,
    pub last_success: Option<String>,
    pub next_run: Option<String>,
    pub overdue: bool,
    /// Whether it keeps every snapshot, forever.
    pub keep_forever: bool,
    /// `status` only: whether a run of the job is going on now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct LastRun {
    pub run_id: i64,
    pub started_at: String,
    pub outcome: &'static str,
    pub exit_code: i32,
}

/// `replica snapshots <job> --json`.
#[derive(Debug, Serialize)]
pub struct Snapshots {
    pub schema: u32,
    pub job: String,
    pub snapshots: Vec<Snapshot>,
    /// Targets that weren't read, and why.
    pub unavailable: Vec<Unavailable>,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub target: String,
    pub id: String,
    pub short_id: String,
    pub time: String,
    pub paths: Vec<String>,
    pub hostname: String,
    /// Whether it's pinned: kept whatever the job's retention.
    pub pinned: bool,
}

#[derive(Debug, Serialize)]
pub struct Unavailable {
    pub target: String,
    pub outcome: &'static str,
    pub reason: String,
}

pub fn print<T: Serialize>(value: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("JSON output serialises")
    );
}
