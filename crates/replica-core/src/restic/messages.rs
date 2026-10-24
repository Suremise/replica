//! restic's `--json` output, as typed values.
//!
//! Field names follow restic 0.19's JSON. Fields Replica doesn't use are left out, and every
//! field has a default, so a newer restic that adds or drops one still parses.

use chrono::{DateTime, FixedOffset};
use serde::Deserialize;

/// One line of streamed output (`backup`, `restore`, `check`, `ls`), by its `message_type`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "message_type", rename_all = "snake_case")]
pub(crate) enum Line {
    Status(BackupProgress),
    Summary(serde_json::Value),
    Error(ItemErrorLine),
    ExitError(ExitError),
    /// A file or folder, from `ls`.
    Node(Node),
    #[serde(other)]
    Other,
}

/// How far a backup has got, for the tray.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BackupProgress {
    /// 0.0 to 1.0.
    pub percent_done: f64,
    pub seconds_elapsed: u64,
    /// restic's estimate; 0 while it doesn't know.
    pub seconds_remaining: u64,
    pub total_files: u64,
    pub files_done: u64,
    pub total_bytes: u64,
    pub bytes_done: u64,
    pub error_count: u64,
    /// The files being read now.
    pub current_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct ItemErrorLine {
    #[serde(default)]
    pub error: ErrorText,
    #[serde(default)]
    pub during: String,
    #[serde(default)]
    pub item: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ErrorText {
    #[serde(default)]
    pub message: String,
}

/// restic's last word when it exits with an error: its exit code and message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ExitError {
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub message: String,
}

/// What a backup did.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BackupSummary {
    pub files_new: u64,
    pub files_changed: u64,
    pub files_unmodified: u64,
    pub dirs_new: u64,
    pub dirs_changed: u64,
    pub dirs_unmodified: u64,
    /// Bytes added to the repository, before compression.
    pub data_added: u64,
    /// Bytes added to the repository, as stored.
    pub data_added_packed: u64,
    pub total_files_processed: u64,
    pub total_bytes_processed: u64,
    /// Seconds.
    pub total_duration: f64,
    /// The new snapshot's ID; empty if none was made.
    pub snapshot_id: String,
}

/// A snapshot in the repository.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Snapshot {
    pub id: String,
    #[serde(default)]
    pub short_id: String,
    pub time: DateTime<FixedOffset>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub summary: Option<BackupSummary>,
}

impl Snapshot {
    /// Whether it's pinned ([`super::PINNED_TAG`]): every `forget` keeps it.
    pub fn is_pinned(&self) -> bool {
        self.tags.iter().any(|t| t == super::PINNED_TAG)
    }
}

/// A file or folder in a snapshot, from `ls`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Node {
    pub name: String,
    /// `file`, `dir`, `symlink` and so on.
    #[serde(rename = "type")]
    pub kind: String,
    /// Its path in the snapshot, `/`-separated, a drive as its first part: `/C/Users/…`.
    pub path: String,
    #[serde(default)]
    pub size: u64,
    pub mtime: Option<DateTime<FixedOffset>>,
}

/// What a restore did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RestoreSummary {
    pub total_files: u64,
    pub files_restored: u64,
    pub files_skipped: u64,
    pub total_bytes: u64,
    pub bytes_restored: u64,
}

/// What `check` found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CheckSummary {
    pub num_errors: u64,
    pub suggest_repair_index: bool,
    pub suggest_prune: bool,
}

/// One group of snapshots from `forget --json`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub(crate) struct ForgetGroup {
    pub keep: Option<Vec<Snapshot>>,
    pub remove: Option<Vec<Snapshot>>,
}

/// A lock, from `cat lock --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub(crate) struct LockInfo {
    pub hostname: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub(crate) struct VersionInfo {
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_streamed_lines() {
        let status = r#"{"message_type":"status","seconds_elapsed":2,"percent_done":0.5,"total_files":4,"files_done":2,"total_bytes":100,"bytes_done":50,"current_files":["C:\\a.txt"]}"#;
        let Line::Status(p) = serde_json::from_str(status).unwrap() else {
            panic!()
        };
        assert_eq!(
            (p.percent_done, p.files_done, p.current_files.len()),
            (0.5, 2, 1)
        );

        let error = r#"{"message_type":"error","error":{"message":"open C:\\a.txt: in use"},"during":"archival","item":"C:\\a.txt"}"#;
        let Line::Error(e) = serde_json::from_str(error).unwrap() else {
            panic!()
        };
        assert_eq!(e.item, r"C:\a.txt");
        assert!(e.error.message.contains("in use"));

        let exit = r#"{"message_type":"exit_error","code":12,"message":"Fatal: wrong password or no key found"}"#;
        assert_eq!(
            serde_json::from_str::<Line>(exit).unwrap(),
            Line::ExitError(ExitError {
                code: 12,
                message: "Fatal: wrong password or no key found".into()
            })
        );

        let other = r#"{"message_type":"verbose_status","action":"new","item":"x"}"#;
        assert_eq!(serde_json::from_str::<Line>(other).unwrap(), Line::Other);
    }

    #[test]
    fn parses_a_backup_summary_and_a_snapshot() {
        let summary: BackupSummary = serde_json::from_str(
            r#"{"message_type":"summary","files_new":2,"data_added":9271,"total_duration":0.72,"snapshot_id":"fa1b"}"#,
        )
        .unwrap();
        assert_eq!(
            (
                summary.files_new,
                summary.data_added,
                summary.snapshot_id.as_str()
            ),
            (2, 9271, "fa1b")
        );

        let snaps: Vec<Snapshot> = serde_json::from_str(
            r#"[{"time":"2026-10-21T19:07:30.5963087+01:00","tree":"f6","paths":["C:\\src"],"hostname":"PC","tags":["hive"],"id":"fa1b05d7c4","short_id":"fa1b05d7","summary":{"files_new":2}}]"#,
        )
        .unwrap();
        assert_eq!(snaps[0].short_id, "fa1b05d7");
        assert_eq!(snaps[0].tags, ["hive"]);
        assert_eq!(snaps[0].summary.as_ref().unwrap().files_new, 2);
    }

    #[test]
    fn parses_ls_nodes() {
        let node: Node = serde_json::from_str(
            r#"{"name":"a.txt","type":"file","path":"/C/src/a.txt","size":6,"mtime":"2026-10-21T19:07:40.7796092+01:00","message_type":"node"}"#,
        )
        .unwrap();
        assert_eq!(
            (node.kind.as_str(), node.path.as_str(), node.size),
            ("file", "/C/src/a.txt", 6)
        );
    }
}
