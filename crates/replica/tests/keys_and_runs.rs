//! A repository password stored by the key flow (`replica_win::keys`, as `replica init` uses it)
//! is the one a run opens the repository with (`replica_core::run`, through
//! `restic::Repository::from_store`). Temp folders and an in-memory secret store only.
//!
//! It needs the pinned restic (`scripts/fetch-restic.ps1`) or `REPLICA_RESTIC`; without one it
//! skips, unless `REPLICA_REQUIRE_RESTIC=1`.

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};

use replica_core::cancel::CancelToken;
use replica_core::history::History;
use replica_core::job::{Job, Source, Target, VolumeRef};
use replica_core::platform::fake::{FakeResolver, MemorySecretStore};
use replica_core::restic::{Repository, Restic};
use replica_core::run::{Outcome, RunContext, run_job};
use replica_win::jobs::JobObjectTree;
use replica_win::keys;

fn restic() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("REPLICA_RESTIC") {
        return Some(PathBuf::from(path));
    }
    let bundled =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/restic/bin/restic.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    assert!(
        std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
        "restic is required (REPLICA_REQUIRE_RESTIC=1) but wasn't found: run scripts/fetch-restic.ps1"
    );
    eprintln!("skipped: no restic (run scripts/fetch-restic.ps1)");
    None
}

#[test]
fn a_password_stored_by_the_key_flow_opens_the_repository_for_a_run() {
    let Some(restic_path) = restic() else { return };
    let temp = tempfile::tempdir().unwrap();
    // The run opens the repository itself: restic's cache stays in the temp folder too.
    let restic = Restic::new(restic_path)
        .unwrap()
        .with_cache_dir(temp.path().join("cache"));
    let drive = temp.path().join("drive");
    fs::create_dir_all(&drive).unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/a.txt"), "a").unwrap();
    let target = Target::Drive {
        name: Some("External".into()),
        volume: VolumeRef {
            volume_id: None,
            label: Some("BACKUP".into()),
        },
        path: "repo".into(),
    };
    let secrets = MemorySecretStore::default();

    // `replica init`'s flow: generate, the user confirms they've saved it, then restic init.
    let password = keys::generate(&target, &secrets)
        .unwrap()
        .confirm_saved(&secrets)
        .unwrap();
    let location = drive.join("repo");
    let repo = Repository::from_store(&location, &target, &secrets)
        .expect("the run's lookup finds the password the key flow stored")
        .with_cache_dir(temp.path().join("cache"));
    restic.init(&repo).unwrap();
    assert_eq!(
        keys::show(&target, &secrets).unwrap(),
        password,
        "and `replica key show` shows it"
    );

    // A run opens the repository with it.
    let job = Job {
        name: "hive".into(),
        sources: vec![Source {
            path: temp.path().join("src"),
            exclude: vec![],
        }],
        targets: vec![target],
        ..Job::default()
    };
    let resolver = FakeResolver {
        volumes: vec![(None, Some("BACKUP".into()), drive)],
        ..Default::default()
    };
    let (logs, locks, staging) = (
        temp.path().join("logs"),
        temp.path().join("locks"),
        temp.path().join("staging"),
    );
    let ctx = RunContext {
        restic: Some(&restic),
        resolver: &resolver,
        secrets: &secrets,
        logs_dir: &logs,
        locks_dir: &locks,
        repo_locks: &replica_win::repo_locks::NamedMutexLocks::new(),
        waits: replica_core::run::Waits::default(),
        give_way: None,
        staging_dir: &staging,
        processes: &JobObjectTree,
        cancel: &CancelToken::new(),
    };
    let mut history = History::in_memory().unwrap();
    let report = run_job(&job, &ctx, &mut history, &mut |_| {});
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert!(report.targets[0].snapshot_id.is_some());
    assert_eq!(restic.snapshots(&repo, Some("hive")).unwrap().len(), 1);
}
