//! The run orchestrator, in temp folders only.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use replica_core::cancel::CancelToken;
use replica_core::history::History;
use replica_core::job::{AfterStep, BeforeStep, Job, Mode, Source, Target, VolumeRef};
use replica_core::platform::fake::{FakeResolver, MemorySecretStore, PlainTree};
use replica_core::platform::{Credential, Secret, SecretStore};
use replica_core::repo_lock::FileLocks;
use replica_core::restic::{Repository, Restic, password_secret_name, snapshot_path};
use replica_core::run::{JobLock, Outcome, RunContext, RunReport, Waits, run_job};
use replica_core::snapshot::sqlite_snapshot;
use rusqlite::Connection;

mod common;

struct Fixture {
    temp: tempfile::TempDir,
    resolver: FakeResolver,
    secrets: MemorySecretStore,
    history: History,
}

impl Fixture {
    /// One connected drive, labelled BACKUP, rooted at `<temp>/drive`.
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let drive = temp.path().join("drive");
        fs::create_dir_all(&drive).unwrap();
        let resolver = FakeResolver {
            volumes: vec![(None, Some("BACKUP".into()), drive)],
            ..Default::default()
        };
        let history = History::open(&temp.path().join("history.db")).unwrap();
        Fixture {
            temp,
            resolver,
            secrets: MemorySecretStore::default(),
            history,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// Replica's staging folder in these tests.
    fn staging(&self) -> PathBuf {
        self.path("data/staging")
    }

    /// Where a job's `n`th snapshot step's copy of `name` goes.
    fn copy(&self, job: &str, n: usize, name: &str) -> PathBuf {
        replica_core::snapshot::copy_dir(&self.staging().join(job), n).join(name)
    }

    fn run(&mut self, job: &Job, restic: Option<&Restic>) -> RunReport {
        let (logs, locks, staging) = (self.path("logs"), self.path("locks"), self.staging());
        let ctx = RunContext {
            restic,
            resolver: &self.resolver,
            secrets: &self.secrets,
            logs_dir: &logs,
            locks_dir: &locks,
            repo_locks: &FileLocks::new(&locks),
            waits: Waits::default(),
            give_way: None,
            staging_dir: &staging,
            processes: &PlainTree,
            cancel: &CancelToken::new(),
        };
        run_job(job, &ctx, &mut self.history, &mut |_| {})
    }
}

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

fn mirror_job(f: &Fixture, targets: Vec<Target>) -> Job {
    f.write("src/a.txt", "a");
    Job {
        name: "job".into(),
        mode: Mode::Mirror,
        mirror_deletes_confirmed: true,
        sources: vec![Source {
            path: f.path("src"),
            exclude: vec![],
        }],
        targets,
        ..Job::default()
    }
}

/// A command that runs until it's stopped, as the shell process alone (no program of its own),
/// so stopping the shell leaves nothing running: these tests run without a real process tree.
/// The tree itself is tested in `crates/replica/tests/command_tree.rs`, with the job object.
fn slow_command() -> String {
    if cfg!(windows) {
        // A loop inside cmd itself.
        "for /L %i in (0,0,1) do @rem".into()
    } else {
        // exec: the shell becomes sleep, so they're one process.
        "exec sleep 30".into()
    }
}

#[test]
fn a_mirror_run_succeeds_and_is_recorded() {
    let mut f = Fixture::new();
    let job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert_eq!(report.outcome.exit_code(), 0);
    assert_eq!(
        fs::read_to_string(f.path("drive/mirror/src/a.txt")).unwrap(),
        "a"
    );
    assert_eq!(report.targets[0].files, 1);
    let log = fs::read_to_string(report.log_path.as_ref().unwrap()).unwrap();
    assert!(log.contains("ended: ok"), "{log}");
    assert_eq!(
        f.history.recent(Some("job"), 10).unwrap()[0].id,
        report.id.unwrap()
    );
}

#[test]
fn a_failed_before_step_fails_the_job_and_skips_the_engine() {
    let mut f = Fixture::new();
    let mut job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    job.before.push(BeforeStep::Command {
        cmd: "echo before && exit 7".into(),
        timeout_secs: 30,
    });
    job.after.push(AfterStep::Command {
        cmd: "echo after".into(),
        timeout_secs: 30,
    });
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(report.outcome.exit_code(), 1);
    assert!(
        report.error.as_deref().unwrap().contains("exit code 7"),
        "{report:?}"
    );
    assert!(report.targets.is_empty(), "the engine didn't run");
    assert!(!f.path("drive/mirror").exists());
    let log = fs::read_to_string(report.log_path.unwrap()).unwrap();
    assert!(
        log.contains("| before"),
        "the step's output is logged: {log}"
    );
    assert_eq!(
        f.history.recent(None, 10).unwrap()[0].outcome,
        Outcome::Failed
    );
}

#[test]
fn a_before_command_that_runs_too_long_times_out_and_fails() {
    let mut f = Fixture::new();
    let mut job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    job.before.push(BeforeStep::Command {
        cmd: slow_command(),
        timeout_secs: 1,
    });
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report
            .error
            .as_deref()
            .unwrap()
            .contains("didn't finish within 1 s"),
        "{report:?}"
    );
    assert!(!f.path("drive/mirror").exists());
}

#[test]
fn a_missing_target_is_skipped_not_failed() {
    let mut f = Fixture::new();
    let job = mirror_job(&f, vec![drive("NOT-CONNECTED", "mirror")]);
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Skipped);
    assert_eq!(report.outcome.exit_code(), 3);
    assert!(
        report.targets[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("isn't available"),
        "{report:?}"
    );
    assert_eq!(
        f.history.last_success("job").unwrap(),
        None,
        "a skip isn't a success"
    );
}

#[test]
fn two_targets_each_have_their_own_result_and_the_worst_wins() {
    let mut f = Fixture::new();
    // One available, one not: ok and skipped, so skipped.
    let job = mirror_job(
        &f,
        vec![drive("BACKUP", "one"), drive("NOT-CONNECTED", "two")],
    );
    let report = f.run(&job, None);
    let outcomes: Vec<Outcome> = report.targets.iter().map(|t| t.outcome).collect();
    assert_eq!(outcomes, [Outcome::Ok, Outcome::Skipped]);
    assert_eq!(report.outcome, Outcome::Skipped);
    assert!(
        f.path("drive/one/src/a.txt").exists(),
        "the available target still ran"
    );

    // A share whose stored login is missing fails: failed beats ok.
    let unc = Target::Unc {
        name: None,
        path: r"\\nas\share".into(),
        credential: Some("missing".into()),
    };
    let job = mirror_job(&f, vec![drive("BACKUP", "three"), unc]);
    let report = f.run(&job, None);
    let outcomes: Vec<Outcome> = report.targets.iter().map(|t| t.outcome).collect();
    assert_eq!(outcomes, [Outcome::Ok, Outcome::Failed]);
    assert_eq!(report.outcome, Outcome::Failed);
    let recorded = &f.history.recent(Some("job"), 1).unwrap()[0];
    let recorded_outcomes: Vec<Outcome> = recorded.targets.iter().map(|t| t.outcome).collect();
    assert_eq!(
        recorded_outcomes,
        [Outcome::Ok, Outcome::Failed],
        "each target's own result is recorded"
    );
}

#[test]
fn a_run_of_a_job_already_running_is_skipped_and_touches_nothing() {
    let mut f = Fixture::new();
    let job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    let held = JobLock::try_acquire(&f.path("locks"), "job")
        .unwrap()
        .unwrap();
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Skipped);
    assert!(report.error.as_deref().unwrap().contains("already running"));
    assert!(!f.path("drive/mirror").exists());
    assert!(
        f.history.recent(None, 10).unwrap().is_empty(),
        "the running one records itself"
    );
    drop(held);
    assert_eq!(f.run(&job, None).outcome, Outcome::Ok);
}

#[test]
fn a_failing_after_command_makes_the_run_partly_done() {
    let mut f = Fixture::new();
    let mut job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    job.after.push(AfterStep::Command {
        cmd: "exit 2".into(),
        timeout_secs: 30,
    });
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Partly);
    assert_eq!(report.outcome.exit_code(), 2);
    assert_eq!(report.targets[0].outcome, Outcome::Ok);
}

#[test]
fn the_history_round_trips_and_survives_reopening() {
    let mut f = Fixture::new();
    let job = mirror_job(
        &f,
        vec![drive("BACKUP", "one"), drive("NOT-CONNECTED", "two")],
    );
    let report = f.run(&job, None);
    let ok_job = mirror_job(&f, vec![drive("BACKUP", "ok")]);
    let ok = f.run(
        &Job {
            name: "ok-job".into(),
            ..ok_job
        },
        None,
    );
    assert_eq!(ok.outcome, Outcome::Ok);

    drop(f.history);
    let history = History::open(&f.temp.path().join("history.db")).unwrap();
    assert_eq!(history.version().unwrap(), 3);
    let runs = history.recent(Some("job"), 10).unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.id, report.id.unwrap());
    assert_eq!(run.job, "job");
    assert_eq!(run.outcome, Outcome::Skipped);
    assert_eq!(run.exit_code, 3);
    assert_eq!(run.started_at.timestamp(), report.started_at.timestamp());
    assert_eq!(run.files, report.files());
    assert_eq!(run.log_path, report.log_path);
    assert_eq!(run.targets.len(), 2);
    assert_eq!(run.targets[0].target, "BACKUP:one");
    assert_eq!(run.targets[0].mode, "mirror");
    assert_eq!(run.targets[1].outcome, Outcome::Skipped);
    assert!(run.targets[1].reason.is_some());
    assert_eq!(
        history.last_success("job").unwrap().map(|t| t.timestamp()),
        Some(report.started_at.timestamp()),
        "one target mirrored, so the run counts, though the other was skipped"
    );
    assert_eq!(
        history
            .last_success("ok-job")
            .unwrap()
            .map(|t| t.timestamp()),
        Some(ok.started_at.timestamp())
    );
    assert_eq!(history.recent(None, 10).unwrap().len(), 2);
    history
        .record_verify("job", "BACKUP:one", ok.started_at)
        .unwrap();
    history
        .record_verify("job", "BACKUP:one", ok.started_at)
        .unwrap();
    assert_eq!(
        history
            .last_verify("job", "BACKUP:one")
            .unwrap()
            .map(|t| t.timestamp()),
        Some(ok.started_at.timestamp())
    );
    assert_eq!(history.last_verify("job", "other").unwrap(), None);
}

#[test]
fn a_sqlite_snapshot_under_write_load_is_consistent() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("live.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE t (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);",
        )
        .unwrap();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let writer = {
        let (db, stop, written) = (db.clone(), stop.clone(), written.clone());
        std::thread::spawn(move || {
            let conn = Connection::open(&db).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(30))
                .unwrap();
            while !stop.load(Ordering::SeqCst) {
                let tx = conn.unchecked_transaction().unwrap();
                for _ in 0..20 {
                    tx.execute("INSERT INTO t (payload) VALUES (randomblob(512))", [])
                        .unwrap();
                }
                tx.commit().unwrap();
                written.fetch_add(20, Ordering::SeqCst);
            }
        })
    };
    // Snapshots while the writer writes: wait until it has written something, each time.
    let staging = temp.path().join("staging");
    let mut seen = Vec::new();
    for round in 0..5 {
        let before = written.load(Ordering::SeqCst);
        let start = std::time::Instant::now();
        while written.load(Ordering::SeqCst) <= before {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(60),
                "the writer stalled"
            );
            std::thread::yield_now();
        }
        let copy = sqlite_snapshot(&db, &staging).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let conn = Connection::open(&copy).unwrap();
        let check: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(check, "ok");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows % 20, 0, "whole transactions only");
        seen.push(rows);
    }
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]) && seen[4] > 0,
        "{seen:?}"
    );
    let leftovers: Vec<_> = fs::read_dir(&staging)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["live.db"], "no temporary files are left");
}

#[test]
fn a_versioned_run_backs_up_the_snapshot_not_the_live_database_and_verifies() {
    let Some(restic_path) = common::restic() else {
        return;
    };
    let mut f = Fixture::new();
    // The run opens the repository itself: restic's cache stays in the temp folder too.
    let restic = Restic::new(restic_path)
        .unwrap()
        .with_cache_dir(f.path("restic-cache"));
    f.write("src/notes.txt", "notes");
    let db = f.path("src/data/app.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE t (x); INSERT INTO t VALUES (1);")
        .unwrap();
    let target = drive("BACKUP", "repo");
    let job = Job {
        name: "versioned".into(),
        sources: vec![Source {
            path: f.path("src"),
            exclude: vec![],
        }],
        targets: vec![target.clone()],
        before: vec![BeforeStep::SqliteSnapshot { db: db.clone() }],
        after: vec![AfterStep::Verify {
            schedule: None,
            read_data_subset: "100%".into(),
        }],
        ..Job::default()
    };
    let password = "run-test-password-5d1e";
    f.secrets
        .set(
            &password_secret_name(&target),
            &Credential {
                username: None,
                secret: Secret::new(password),
            },
        )
        .unwrap();
    let repo = Repository::new(
        f.path("drive/repo").to_string_lossy(),
        Secret::new(password),
    )
    .with_cache_dir(f.path("cache"));
    restic.init(&repo).unwrap();

    let report = f.run(&job, Some(&restic));
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    let snapshot = report.targets[0].snapshot_id.clone().unwrap();

    // The staged copy is in the snapshot; the live database and its WAL aren't.
    let nodes = restic.ls(&repo, &snapshot, None).unwrap();
    let has = |p: &Path| nodes.iter().any(|n| n.path == snapshot_path(p));
    assert!(
        has(&f.copy("versioned", 1, "app.db")),
        "the snapshot copy is backed up"
    );
    assert!(has(&f.path("src/notes.txt")));
    assert!(!has(&db), "the live database is left out");
    assert!(!has(&f.path("src/data/app.db-wal")));

    // Verify ran (no schedule: every run) and was recorded.
    assert!(
        f.history
            .last_verify("versioned", &target.display_name())
            .unwrap()
            .is_some()
    );

    // The password is in neither the log nor the history.
    let log = fs::read_to_string(report.log_path.unwrap()).unwrap();
    assert!(!log.contains(password), "{log}");
    assert!(log.contains("snapshot"), "{log}");

    // Without the stored password, the target fails with a clear reason.
    f.secrets.delete(&password_secret_name(&target)).unwrap();
    let report = f.run(&job, Some(&restic));
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report.targets[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("password"),
        "{report:?}"
    );
}

#[test]
fn a_versioned_target_without_restic_fails_with_a_reason() {
    let mut f = Fixture::new();
    f.write("src/a.txt", "a");
    let job = Job {
        name: "versioned".into(),
        sources: vec![Source {
            path: f.path("src"),
            exclude: vec![],
        }],
        targets: vec![drive("BACKUP", "repo")],
        ..Job::default()
    };
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report.targets[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("restic"),
        "{report:?}"
    );
}

#[cfg(windows)]
#[test]
fn a_file_in_use_makes_a_mirror_run_partly_done() {
    use std::os::windows::fs::OpenOptionsExt;

    let mut f = Fixture::new();
    let job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    f.write("src/busy.txt", "in use");
    let busy = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(f.path("src/busy.txt"))
        .unwrap();
    let report = f.run(&job, None);
    drop(busy);
    assert_eq!(report.outcome, Outcome::Partly);
    assert_eq!(report.outcome.exit_code(), 2);
    assert_eq!(report.targets[0].problems.len(), 1, "{report:?}");
    let run = &f.history.recent(Some("job"), 1).unwrap()[0];
    assert_eq!(
        run.targets[0].skipped,
        Some(1),
        "the history keeps the count"
    );
    assert_eq!(run.skipped(), Some(1));
}

/// A WAL database with `rows` rows, whose last rows stay in the WAL while the returned
/// connection is open (no checkpoint), so the database file alone doesn't have them.
fn wal_database(path: &Path, rows: i64) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0; CREATE TABLE t (x INTEGER);",
    )
    .unwrap();
    for i in 0..rows {
        conn.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
    }
    conn
}

fn rows(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_live_database_given_as_a_file_source_is_replaced_by_its_copy() {
    let mut f = Fixture::new();
    let db = f.path("live/app.db");
    let _open = wal_database(&db, 25);
    let job = Job {
        name: "db".into(),
        mode: Mode::Mirror,
        mirror_deletes_confirmed: true,
        sources: vec![Source {
            path: db.clone(),
            exclude: vec![],
        }],
        targets: vec![drive("BACKUP", "mirror")],
        before: vec![BeforeStep::SqliteSnapshot { db: db.clone() }],
        ..Job::default()
    };
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert!(!f.path("drive/mirror/app.db").exists(), "no raw live file");
    assert_eq!(
        rows(&f.path("drive/mirror/sqlite-snapshots/1/app.db")),
        25,
        "the consistent copy, WAL rows included, is mirrored instead"
    );
}

#[test]
fn the_copy_is_backed_up_even_when_the_source_excludes_databases() {
    let mut f = Fixture::new();
    let db = f.path("src/app.db");
    let _open = wal_database(&db, 3);
    f.write("src/keep.txt", "k");
    let job = Job {
        name: "excluded".into(),
        mode: Mode::Mirror,
        mirror_deletes_confirmed: true,
        sources: vec![Source {
            path: f.path("src"),
            exclude: vec!["*.db".into()],
        }],
        targets: vec![drive("BACKUP", "mirror")],
        before: vec![BeforeStep::SqliteSnapshot { db: db.clone() }],
        ..Job::default()
    };
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert!(f.path("drive/mirror/src/keep.txt").exists());
    assert!(!f.path("drive/mirror/src/app.db").exists());
    assert_eq!(rows(&f.path("drive/mirror/sqlite-snapshots/1/app.db")), 3);
}

#[test]
fn two_databases_with_one_name_each_get_their_own_copy() {
    let mut f = Fixture::new();
    let (left, right) = (f.path("src/left/app.db"), f.path("src/right/app.db"));
    let _l = wal_database(&left, 11);
    let _r = wal_database(&right, 22);
    f.write("src/notes.txt", "an ordinary file beside the databases");
    let job = Job {
        name: "two".into(),
        mode: Mode::Mirror,
        mirror_deletes_confirmed: true,
        sources: vec![Source {
            path: f.path("src"),
            exclude: vec![],
        }],
        targets: vec![drive("BACKUP", "mirror")],
        before: vec![
            BeforeStep::SqliteSnapshot { db: left.clone() },
            BeforeStep::SqliteSnapshot { db: right.clone() },
        ],
        ..Job::default()
    };
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert_eq!(rows(&f.copy("two", 1, "app.db")), 11);
    assert_eq!(rows(&f.copy("two", 2, "app.db")), 22);
    assert_eq!(rows(&f.path("drive/mirror/sqlite-snapshots/1/app.db")), 11);
    assert_eq!(rows(&f.path("drive/mirror/sqlite-snapshots/2/app.db")), 22);
    assert!(!f.path("drive/mirror/src/left/app.db").exists());
    assert!(!f.path("drive/mirror/src/right/app.db").exists());
    assert_eq!(
        (rows(&left), rows(&right)),
        (11, 22),
        "the live databases are untouched"
    );

    // A step removed later leaves no stale copy to back up.
    // (Deleting the stale copy from the mirror is 1 of its 3 files: over the default 30%.)
    let one = Job {
        before: vec![BeforeStep::SqliteSnapshot { db: left.clone() }],
        mirror_delete_limit_percent: 100,
        ..job
    };
    let report = f.run(&one, None);
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert!(
        !f.copy("two", 2, "app.db").exists(),
        "the old copy is tidied away"
    );
    assert!(!f.path("drive/mirror/sqlite-snapshots/2").exists());
}

#[test]
fn a_database_anywhere_in_replicas_staging_folder_is_refused_and_untouched() {
    let places = [
        // Where its own copy would go.
        ("odd", "odd/sqlite-snapshots/1/app.db"),
        // Loose in the job's snapshots folder, which a run tidies.
        ("odd", "odd/sqlite-snapshots/live.db"),
        // Beneath a step folder no step uses.
        ("odd", "odd/sqlite-snapshots/7/x.db"),
        // In another job's staging.
        ("odd", "other/sqlite-snapshots/1/y.db"),
        // At the staging folder's root.
        ("odd", "root.db"),
    ];
    for (job_name, rel) in places {
        let mut f = Fixture::new();
        let db = f.staging().join(rel);
        let _open = wal_database(&db, 44);
        f.write("src/a.txt", "a");
        let job = Job {
            name: job_name.into(),
            mode: Mode::Mirror,
            mirror_deletes_confirmed: true,
            sources: vec![Source {
                path: f.path("src"),
                exclude: vec![],
            }],
            targets: vec![drive("BACKUP", "mirror")],
            before: vec![BeforeStep::SqliteSnapshot { db: db.clone() }],
            ..Job::default()
        };
        let report = f.run(&job, None);
        assert_eq!(report.outcome, Outcome::Failed, "{rel}");
        assert!(
            report
                .error
                .as_deref()
                .unwrap()
                .contains("inside Replica's staging folder"),
            "{rel}: {report:?}"
        );
        assert!(db.exists(), "{rel}: the database is still there");
        assert_eq!(rows(&db), 44, "{rel}: and unchanged");
        assert!(!f.path("drive/mirror").exists(), "{rel}: nothing ran");
    }
}

#[test]
fn a_step_whose_database_changes_keeps_only_the_new_copy() {
    let mut f = Fixture::new();
    let (old, new) = (f.path("src/old.db"), f.path("src/new.db"));
    let _o = wal_database(&old, 1);
    let _n = wal_database(&new, 2);
    f.write("src/a.txt", "a");
    let src = f.path("src");
    let job = |db: &Path| Job {
        name: "change".into(),
        mode: Mode::Mirror,
        mirror_deletes_confirmed: true,
        mirror_delete_limit_percent: 100,
        sources: vec![Source {
            path: src.clone(),
            exclude: vec![],
        }],
        targets: vec![drive("BACKUP", "mirror")],
        before: vec![BeforeStep::SqliteSnapshot {
            db: db.to_path_buf(),
        }],
        ..Job::default()
    };
    assert_eq!(f.run(&job(&old), None).outcome, Outcome::Ok);
    assert!(f.copy("change", 1, "old.db").exists());
    let report = f.run(&job(&new), None);
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    let in_staging: Vec<String> = fs::read_dir(f.copy("change", 1, "x").parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        in_staging,
        ["new.db"],
        "only the step's current copy is staged"
    );
    let mirrored: Vec<String> = fs::read_dir(f.path("drive/mirror/sqlite-snapshots/1"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(mirrored, ["new.db"], "and mirrored");
    assert_eq!(rows(&f.path("drive/mirror/sqlite-snapshots/1/new.db")), 2);
}

#[test]
fn an_unresolvable_database_path_fails_the_run_before_anything_runs() {
    let mut f = Fixture::new();
    f.write("src/a.txt", "a");
    let job = Job {
        name: "bad".into(),
        mode: Mode::Mirror,
        mirror_deletes_confirmed: true,
        sources: vec![Source {
            path: f.path("src"),
            exclude: vec![],
        }],
        targets: vec![drive("BACKUP", "mirror")],
        // A NUL in a path can't be resolved on any system.
        before: vec![
            BeforeStep::Command {
                cmd: "echo first".into(),
                timeout_secs: 30,
            },
            BeforeStep::SqliteSnapshot {
                db: PathBuf::from("bad\0name.db"),
            },
        ],
        ..Job::default()
    };
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report
            .error
            .as_deref()
            .unwrap()
            .contains("couldn't be worked out"),
        "{report:?}"
    );
    let log = fs::read_to_string(report.log_path.unwrap()).unwrap();
    assert!(!log.contains("command: echo first"), "nothing ran: {log}");
    assert!(!f.path("drive/mirror").exists());
}

#[test]
fn back_to_back_runs_each_keep_their_own_log() {
    let mut f = Fixture::new();
    let job = mirror_job(&f, vec![drive("BACKUP", "mirror")]);
    let reports: Vec<RunReport> = (0..3).map(|_| f.run(&job, None)).collect();
    let paths: Vec<PathBuf> = reports
        .iter()
        .map(|r| r.log_path.clone().unwrap())
        .collect();
    assert_ne!(paths[0], paths[1]);
    assert_ne!(paths[1], paths[2]);
    assert_ne!(paths[0], paths[2]);
    for p in &paths {
        let log = fs::read_to_string(p).unwrap();
        assert_eq!(log.matches("started").count(), 1, "one run per log: {log}");
    }
    let recorded: Vec<Option<PathBuf>> = f
        .history
        .recent(Some("job"), 3)
        .unwrap()
        .into_iter()
        .map(|r| r.log_path)
        .collect();
    for p in &paths {
        assert!(
            recorded.contains(&Some(p.clone())),
            "each run links its own log"
        );
    }
}

#[test]
fn a_run_whose_targets_were_all_skipped_is_not_a_success_whatever_its_steps_did() {
    let mut f = Fixture::new();
    let mut job = mirror_job(&f, vec![drive("NOT-CONNECTED", "mirror")]);
    job.after.push(AfterStep::Command {
        cmd: "exit 2".into(),
        timeout_secs: 30,
    });
    let report = f.run(&job, None);
    assert_eq!(report.outcome, Outcome::Partly, "the after step's failure");
    assert_eq!(
        f.history.last_success("job").unwrap(),
        None,
        "but nothing was backed up"
    );
    // A run that mirrored is a success.
    let ok = f.run(&mirror_job(&f, vec![drive("BACKUP", "mirror")]), None);
    assert_eq!(
        f.history
            .last_success("job")
            .unwrap()
            .map(|t| t.timestamp()),
        Some(ok.started_at.timestamp())
    );
}

/// A junction (Windows) or symbolic link (elsewhere) at `link`, to the folder `to`.
fn link_dir(link: &Path, to: &Path) {
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link.to_string_lossy().replace('/', "\\"))
        .arg(to.to_string_lossy().replace('/', "\\"))
        .output()
        .is_ok_and(|o| o.status.success());
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(to, link).is_ok();
    assert!(made, "couldn't make a link at {}", link.display());
}

#[test]
fn a_database_named_through_an_alias_is_still_left_out() {
    for alias in ["dots", "junction"] {
        let mut f = Fixture::new();
        let db = f.path("src/app.db");
        let _open = wal_database(&db, 9);
        f.write("src/keep.txt", "k");
        let named = match alias {
            // src/sub/../app.db: the same file, spelled another way.
            "dots" => {
                fs::create_dir_all(f.path("src/sub")).unwrap();
                f.path("src/sub/../app.db")
            }
            // A junction to src, outside it.
            _ => {
                link_dir(&f.path("via"), &f.path("src"));
                f.path("via/app.db")
            }
        };
        let job = Job {
            name: format!("alias-{alias}"),
            mode: Mode::Mirror,
            mirror_deletes_confirmed: true,
            sources: vec![Source {
                path: f.path("src"),
                exclude: vec![],
            }],
            targets: vec![drive("BACKUP", "mirror")],
            before: vec![BeforeStep::SqliteSnapshot { db: named }],
            ..Job::default()
        };
        let report = f.run(&job, None);
        assert_eq!(report.outcome, Outcome::Ok, "{alias}: {report:?}");
        assert!(f.path("drive/mirror/src/keep.txt").exists(), "{alias}");
        assert!(
            !f.path("drive/mirror/src/app.db").exists(),
            "{alias}: the raw live database is left out"
        );
        assert!(
            !f.path("drive/mirror/src/app.db-wal").exists(),
            "{alias}: and its WAL"
        );
        assert_eq!(
            rows(&f.path("drive/mirror/sqlite-snapshots/1/app.db")),
            9,
            "{alias}: the copy is mirrored instead"
        );
    }
}
