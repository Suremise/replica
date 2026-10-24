//! The real backend on a temp app folder, with what would reach Task Scheduler, the history
//! and restic faked from lists: the screenshots show tasks, runs and snapshots without making
//! a task, writing a history or running restic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use replica::ui::backend::{
    Backend, LiveBackend, Places, RestoreJob, Status, TargetCheck, UiRequest,
};
use replica_core::cancel::CancelToken;
use replica_core::history::{HistoryFilter, RunRecord, TargetRecord, TestRestoreRecord};
use replica_core::job::{Job, Target};
use replica_core::platform::{Secret, SecretStore};
use replica_core::restic::{BackupSummary, Node, RestoreSummary, Snapshot};
use replica_core::run::Outcome;
use replica_win::keys::{Entered, NotSaved};
use replica_win::scheduler::ScheduledTask;
use replica_win::targets::VolumeInfo;

pub struct Faked {
    pub live: Arc<LiveBackend>,
    pub tasks: Mutex<Vec<ScheduledTask>>,
    pub runs: Mutex<Vec<RunRecord>>,
    pub tests: Mutex<Vec<TestRestoreRecord>>,
    pub snapshots: Mutex<Vec<Snapshot>>,
    /// A folder's contents, by its path in the snapshot.
    pub folders: Mutex<HashMap<String, Vec<Node>>>,
    /// Each snapshots request: the job and the target's name.
    pub snapshot_requests: Mutex<Vec<(String, String)>>,
    /// This job's snapshots wait for the test (see [`Gate`]).
    pub gate: Mutex<Option<Gate>>,
    /// What the Save and Open dialogs for a jobs file answer.
    pub jobs_file: Mutex<Option<PathBuf>>,
}

/// Holds a job's snapshots back: `started` once they're asked for, then they come when the
/// test sends on (or drops) the other end of `release`, so a test decides when a late answer
/// lands rather than guessing how long it takes.
pub struct Gate {
    pub job: String,
    pub started: Arc<AtomicBool>,
    pub release: Receiver<()>,
}

impl Faked {
    pub fn new(live: Arc<LiveBackend>, tasks: Vec<ScheduledTask>) -> Faked {
        Faked {
            live,
            tasks: Mutex::new(tasks),
            runs: Mutex::default(),
            tests: Mutex::default(),
            snapshots: Mutex::default(),
            folders: Mutex::default(),
            snapshot_requests: Mutex::default(),
            gate: Mutex::default(),
            jobs_file: Mutex::default(),
        }
    }

    /// A history of mixed results: OK, partly done, failed, skipped and a test restore.
    pub fn with_history(self) -> Faked {
        let now = Utc::now();
        let run = |id: i64, job: &str, hours: i64, outcome: Outcome, label: &str| RunRecord {
            id,
            job: job.into(),
            started_at: now - chrono::TimeDelta::minutes(hours * 60 + id * 7),
            duration: Duration::from_secs(40 + id as u64 * 13),
            outcome,
            exit_code: outcome.exit_code(),
            files: 1200 + id as u64 * 311,
            bytes: 2_400_000_000 / (id as u64 + 1),
            error: (outcome == Outcome::Failed).then(|| {
                "restic couldn't open the repository: wrong password, or the files are damaged"
                    .into()
            }),
            log_path: Some(PathBuf::from(format!(r"C:\Replica\logs\runs\{id}.log"))),
            targets: vec![TargetRecord {
                target: format!("{label}:Replica"),
                mode: "versioned".into(),
                outcome,
                files: 1200 + id as u64 * 311,
                bytes: 2_400_000_000 / (id as u64 + 1),
                reason: match outcome {
                    Outcome::Skipped => Some(format!("the drive {label} isn't connected")),
                    Outcome::Partly => Some("3 files couldn't be read: they were in use".into()),
                    _ => None,
                },
                snapshot_id: None,
                skipped: Some(if outcome == Outcome::Partly { 3 } else { 0 }),
            }],
        };
        *self.runs.lock().unwrap() = vec![
            run(1, "Documents", 2, Outcome::Ok, "BACKUP"),
            run(2, "Photos", 3, Outcome::Partly, "PHOTOS"),
            run(3, "Music", 5, Outcome::Failed, "NAS"),
            run(4, "Documents", 26, Outcome::Ok, "BACKUP"),
            run(5, "Photos", 27, Outcome::Skipped, "PHOTOS"),
            run(6, "Documents", 50, Outcome::Ok, "BACKUP"),
        ];
        *self.tests.lock().unwrap() = vec![TestRestoreRecord {
            id: 1,
            job: "Documents".into(),
            target: "BACKUP:Replica".into(),
            snapshot_id: Some("4f2a91c0d3".into()),
            started_at: now - chrono::TimeDelta::hours(30),
            duration: Duration::from_secs(9),
            outcome: Outcome::Ok,
            checked: 20,
            differing: 0,
            detail: None,
        }];
        self
    }

    /// Long names in the history: [`super::LONG_JOB`]'s runs to a long drive label and
    /// a deep share, [`super::LONG_JOB_2`]'s, and a short job's, with a test restore.
    pub fn with_long_history(self) -> Faked {
        use super::{LONG_JOB, LONG_JOB_2, LONG_LABEL, LONG_UNC};
        let now = Utc::now();
        let target = |target: String, outcome: Outcome, reason: Option<String>| TargetRecord {
            target,
            mode: "versioned".into(),
            outcome,
            files: 48_210,
            bytes: 61_300_000_000,
            reason,
            snapshot_id: None,
            skipped: Some(if outcome == Outcome::Partly { 12 } else { 0 }),
        };
        let run = |id: i64, job: &str, hours: i64, outcome: Outcome, targets| RunRecord {
            id,
            job: job.into(),
            started_at: now - chrono::TimeDelta::minutes(hours * 60 + id * 7),
            duration: Duration::from_secs(600 + id as u64 * 13),
            outcome,
            exit_code: outcome.exit_code(),
            files: 48_210,
            bytes: 61_300_000_000,
            error: None,
            log_path: Some(PathBuf::from(format!(
                r"C:\Users\Alex\AppData\Roaming\Replica\logs\runs\{id}.log"
            ))),
            targets,
        };
        let drive = format!("{LONG_LABEL}:Replica");
        *self.runs.lock().unwrap() = vec![
            run(
                1,
                LONG_JOB,
                2,
                Outcome::Partly,
                vec![
                    target(drive.clone(), Outcome::Ok, None),
                    target(
                        LONG_UNC.into(),
                        Outcome::Partly,
                        Some(format!(
                            "12 files couldn't be read under {LONG_UNC}: they were in use"
                        )),
                    ),
                ],
            ),
            run(
                2,
                LONG_JOB_2,
                3,
                Outcome::Skipped,
                vec![target(
                    drive.clone(),
                    Outcome::Skipped,
                    Some(format!("the drive {LONG_LABEL} isn't connected")),
                )],
            ),
            run(
                3,
                "Music",
                5,
                Outcome::Ok,
                vec![target("BACKUP:Replica".into(), Outcome::Ok, None)],
            ),
            run(
                4,
                LONG_JOB,
                26,
                Outcome::Ok,
                vec![
                    target(drive.clone(), Outcome::Ok, None),
                    target(LONG_UNC.into(), Outcome::Ok, None),
                ],
            ),
        ];
        *self.tests.lock().unwrap() = vec![TestRestoreRecord {
            id: 1,
            job: LONG_JOB.into(),
            target: drive,
            snapshot_id: Some("4f2a91c0d3".into()),
            started_at: now - chrono::TimeDelta::hours(30),
            duration: Duration::from_secs(9),
            outcome: Outcome::Ok,
            checked: 20,
            differing: 0,
            detail: None,
        }];
        self
    }

    /// [`Faked::with_snapshots`], each also tagged [`super::LONG_TAG`], with a file whose name
    /// is long among the folders.
    pub fn with_long_snapshots(self) -> Faked {
        let faked = self.with_snapshots();
        for s in faked.snapshots.lock().unwrap().iter_mut() {
            s.tags.push(super::LONG_TAG.into());
        }
        let projects = "/C/Users/Alex/Documents/Projects";
        faked
            .folders
            .lock()
            .unwrap()
            .get_mut(projects)
            .unwrap()
            .push(Node {
                name: "northwind-specification-for-the-new-release-final-final-v3.docx".into(),
                kind: "file".into(),
                path: format!(
                    "{projects}/northwind-specification-for-the-new-release-final-final-v3.docx"
                ),
                size: 2_310_000,
                mtime: None,
            });
        faked
    }

    /// Three snapshots of Documents, and its folders.
    pub fn with_snapshots(self) -> Faked {
        let now = Utc::now();
        let snapshot = |id: &str, hours: i64, bytes: u64| Snapshot {
            id: format!("{id}0123456789abcdef"),
            short_id: id.into(),
            time: (now - chrono::TimeDelta::hours(hours)).fixed_offset(),
            paths: vec![r"C:\Users\Alex\Documents".into()],
            tags: vec!["Documents".into(), "replica".into()],
            hostname: "ALEX-PC".into(),
            summary: Some(BackupSummary {
                total_bytes_processed: bytes,
                ..BackupSummary::default()
            }),
        };
        *self.snapshots.lock().unwrap() = vec![
            snapshot("4f2a91c0", 2, 12_400_000_000),
            snapshot("9b31d7e2", 26, 12_300_000_000),
            snapshot("c07e5a18", 50, 12_100_000_000),
        ];
        let node = |path: &str, kind: &str, size: u64| Node {
            name: path.rsplit('/').next().unwrap().into(),
            kind: kind.into(),
            path: path.into(),
            size,
            mtime: None,
        };
        let root = "/C/Users/Alex/Documents";
        let mut folders = HashMap::new();
        folders.insert(
            root.to_string(),
            vec![
                node(&format!("{root}/Projects"), "dir", 0),
                node(&format!("{root}/Taxes"), "dir", 0),
                node(&format!("{root}/notes.txt"), "file", 4_210),
                node(&format!("{root}/report.docx"), "file", 1_830_000),
            ],
        );
        folders.insert(
            format!("{root}/Projects"),
            vec![
                node(&format!("{root}/Projects/Northwind"), "dir", 0),
                node(&format!("{root}/Projects/plan.md"), "file", 12_800),
                node(&format!("{root}/Projects/budget.xlsx"), "file", 96_400),
            ],
        );
        *self.folders.lock().unwrap() = folders;
        self
    }
}

/// A task as Replica makes them, for `job`: next run in 9 hours (when on), last run 2 hours
/// ago with `last_result` (if it ran).
pub fn task(job: &str, schedule: &str, enabled: bool, last_result: Option<i32>) -> ScheduledTask {
    let now = chrono::Local::now().naive_local();
    ScheduledTask {
        name: job.into(),
        job: job.into(),
        orphan: false,
        schedule: Some(schedule.into()),
        enabled,
        running: false,
        next_run: enabled.then(|| now + chrono::Duration::hours(9)),
        last_run: last_result.map(|_| now - chrono::Duration::hours(2)),
        last_result,
    }
}

fn passes(filter: &HistoryFilter, job: &str, outcome: Outcome) -> bool {
    filter.job.as_deref().is_none_or(|j| j == job) && filter.outcome.is_none_or(|o| o == outcome)
}

impl Backend for Faked {
    fn settings_file(&self) -> PathBuf {
        self.live.settings_file()
    }
    fn writable(&self) -> bool {
        self.live.writable()
    }
    fn window_state_file(&self) -> PathBuf {
        self.live.window_state_file()
    }
    fn send(&self, request: UiRequest) {
        self.live.send(request)
    }
    fn status(&self) -> Status {
        self.live.status()
    }
    fn secrets(&self) -> &dyn SecretStore {
        self.live.secrets()
    }
    fn volumes(&self) -> Result<Vec<VolumeInfo>, String> {
        self.live.volumes()
    }
    fn has_task(&self, job: &str) -> bool {
        self.tasks.lock().unwrap().iter().any(|t| t.name == job)
    }
    fn delete_task(&self, job: &str) -> Result<(), String> {
        self.tasks.lock().unwrap().retain(|t| t.name != job);
        Ok(())
    }
    fn check_target(&self, target: &Target) -> TargetCheck {
        self.live.check_target(target)
    }
    fn init_repository(&self, target: &Target, secret: Secret) -> Result<(), String> {
        self.live.init_repository(target, secret)
    }
    fn save_entered_password(
        &self,
        target: &Target,
        secret: Secret,
        force: bool,
    ) -> Result<Entered, NotSaved> {
        self.live.save_entered_password(target, secret, force)
    }
    fn pick_folder(&self) -> Option<PathBuf> {
        None
    }
    fn pick_file(&self) -> Option<PathBuf> {
        None
    }
    fn save_jobs_file(&self) -> Option<PathBuf> {
        self.jobs_file.lock().unwrap().clone()
    }
    fn open_jobs_file(&self) -> Option<PathBuf> {
        self.jobs_file.lock().unwrap().clone()
    }
    fn runs(&self, filter: &HistoryFilter, limit: usize) -> Result<Vec<RunRecord>, String> {
        let mut runs: Vec<RunRecord> = self
            .runs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| passes(filter, &r.job, r.outcome))
            .cloned()
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        runs.truncate(limit);
        Ok(runs)
    }
    fn test_restores(
        &self,
        filter: &HistoryFilter,
        limit: usize,
    ) -> Result<Vec<TestRestoreRecord>, String> {
        let mut tests: Vec<TestRestoreRecord> = self
            .tests
            .lock()
            .unwrap()
            .iter()
            .filter(|t| passes(filter, &t.job, t.outcome))
            .cloned()
            .collect();
        tests.sort_by_key(|t| std::cmp::Reverse(t.started_at));
        tests.truncate(limit);
        Ok(tests)
    }
    fn history_jobs(&self) -> Result<Vec<String>, String> {
        let mut jobs: Vec<String> = self
            .runs
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.job.clone())
            .collect();
        jobs.sort();
        jobs.dedup();
        Ok(jobs)
    }
    fn places(&self) -> Places {
        self.live.places()
    }
    fn tasks(&self, _: &[&str]) -> Result<Vec<ScheduledTask>, String> {
        Ok(self.tasks.lock().unwrap().clone())
    }
    fn set_task_enabled(&self, task: &str, enabled: bool) -> Result<(), String> {
        for t in self.tasks.lock().unwrap().iter_mut() {
            if t.name == task {
                t.enabled = enabled;
            }
        }
        Ok(())
    }
    fn delete_all_tasks(&self) -> Result<usize, String> {
        let mut tasks = self.tasks.lock().unwrap();
        let n = tasks.len();
        tasks.clear();
        Ok(n)
    }
    fn turn_off_tasks(&self, delete: bool) -> Result<usize, String> {
        if delete {
            self.delete_all_tasks()
        } else {
            Ok(0)
        }
    }
    fn open_task_scheduler(&self) -> Result<(), String> {
        Ok(())
    }
    fn open_url(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn restic_in_use(&self, override_path: Option<PathBuf>) -> Result<(PathBuf, String), String> {
        self.live.restic_in_use(override_path)
    }
    fn open_path(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    /// The snapshots, tagged with the job and target asked for, so a list shows whose it is,
    /// then any tags past a snapshot's first two.
    fn snapshots(&self, job: &Job, target: &Target) -> Result<Vec<Snapshot>, String> {
        self.snapshot_requests
            .lock()
            .unwrap()
            .push((job.name.clone(), target.display_name()));
        // Taken out, so other jobs' requests aren't held up behind this one.
        let gate = {
            let mut gate = self.gate.lock().unwrap();
            if gate.as_ref().is_some_and(|g| g.job == job.name) {
                gate.take()
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.started.store(true, Ordering::SeqCst);
            // Released by the test, or by its end dropping the sender; bounded all the same.
            let _ = gate.release.recv_timeout(Duration::from_secs(60));
        }
        Ok(self
            .snapshots
            .lock()
            .unwrap()
            .iter()
            .map(|s| Snapshot {
                tags: [job.name.clone(), target.display_name()]
                    .into_iter()
                    .chain(s.tags.iter().skip(2).cloned())
                    .collect(),
                ..s.clone()
            })
            .collect())
    }
    fn list(&self, _: &Target, _: &str, folder: &str) -> Result<Vec<Node>, String> {
        Ok(self
            .folders
            .lock()
            .unwrap()
            .get(folder)
            .cloned()
            .unwrap_or_default())
    }
    fn restore(
        &self,
        _: &Job,
        _: &Target,
        restore: &RestoreJob,
        _: &CancelToken,
    ) -> Result<RestoreSummary, String> {
        Ok(RestoreSummary {
            total_files: restore.items.len() as u64,
            files_restored: restore.items.len() as u64,
            files_skipped: 0,
            total_bytes: 1_000,
            bytes_restored: 1_000,
        })
    }
    /// As restic does: the tag changed, and a new ID for the rewritten snapshot.
    fn set_pinned(&self, _: &Job, _: &Target, id: &str, pinned: bool) -> Result<String, String> {
        let mut snapshots = self.snapshots.lock().unwrap();
        let s = snapshots
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("no snapshot {id}"))?;
        s.tags.retain(|t| t != replica_core::restic::PINNED_TAG);
        if pinned {
            s.tags.push(replica_core::restic::PINNED_TAG.to_string());
        }
        s.id = format!("{id}-{}", if pinned { "p" } else { "u" });
        s.short_id = s.id.chars().take(8).collect();
        Ok(s.id.clone())
    }
    fn test_restore(
        &self,
        job: &Job,
        target: &Target,
        cancel: &CancelToken,
    ) -> Result<TestRestoreRecord, String> {
        self.live.test_restore(job, target, cancel)
    }
}
