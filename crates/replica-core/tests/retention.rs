//! Keeping things: a job that keeps everything never forgets or prunes, and a pinned
//! snapshot survives any retention until it's unpinned. With the pinned restic, in temp folders
//! (restic's cache too).

use std::fs;
use std::path::PathBuf;

use replica_core::cancel::CancelToken;
use replica_core::history::History;
use replica_core::job::{Job, Retention, Source, Target, VolumeRef};
use replica_core::platform::fake::{FakeResolver, MemorySecretStore, PlainTree};
use replica_core::platform::{Credential, Secret, SecretStore};
use replica_core::repo_lock::FileLocks;
use replica_core::restic::{Repository, Restic, Snapshot, password_secret_name};
use replica_core::run::{Outcome, RunContext, RunReport, Waits, run_job};

mod common;

const PASSWORD: &str = "retention-test-password-3b9e";

struct Fixture {
    temp: tempfile::TempDir,
    resolver: FakeResolver,
    secrets: MemorySecretStore,
    history: History,
    restic: Restic,
}

impl Fixture {
    /// A drive, BACKUP, at `<temp>/drive`, with an initialised repository at `repo`.
    fn new() -> Option<Fixture> {
        let restic_path = common::restic()?;
        let temp = tempfile::tempdir().unwrap();
        let restic = Restic::new(restic_path)
            .unwrap()
            .with_cache_dir(temp.path().join("restic-cache"));
        fs::create_dir_all(temp.path().join("drive")).unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        let f = Fixture {
            resolver: FakeResolver {
                volumes: vec![(None, Some("BACKUP".into()), temp.path().join("drive"))],
                ..Default::default()
            },
            secrets: MemorySecretStore::default(),
            history: History::open(&temp.path().join("history.db")).unwrap(),
            temp,
            restic,
        };
        f.secrets
            .set(
                &password_secret_name(&target()),
                &Credential {
                    username: None,
                    secret: Secret::new(PASSWORD),
                },
            )
            .unwrap();
        f.restic.init(&f.repository()).unwrap();
        Some(f)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn repository(&self) -> Repository {
        Repository::new(
            self.path("drive/repo").to_string_lossy(),
            Secret::new(PASSWORD),
        )
        .with_cache_dir(self.path("restic-cache"))
    }

    fn job(&self, retention: Retention) -> Job {
        Job {
            name: "hive".into(),
            sources: vec![Source {
                path: self.path("src"),
                exclude: vec![],
            }],
            targets: vec![target()],
            retention,
            ..Job::default()
        }
    }

    /// A run, after changing the source so each backs up something new.
    fn run(&mut self, job: &Job, n: usize) -> RunReport {
        fs::write(self.path("src/a.txt"), format!("version {n}")).unwrap();
        let (logs, locks, staging) = (self.path("logs"), self.path("locks"), self.path("staging"));
        let repo_locks = FileLocks::new(&locks);
        let ctx = RunContext {
            restic: Some(&self.restic),
            resolver: &self.resolver,
            secrets: &self.secrets,
            logs_dir: &logs,
            locks_dir: &locks,
            repo_locks: &repo_locks,
            waits: Waits::default(),
            give_way: None,
            staging_dir: &staging,
            processes: &PlainTree,
            cancel: &CancelToken::new(),
        };
        let report = run_job(job, &ctx, &mut self.history, &mut |_| {});
        assert_eq!(report.outcome, Outcome::Ok, "run {n}: {report:?}");
        report
    }

    fn snapshots(&self) -> Vec<Snapshot> {
        let mut list = self
            .restic
            .snapshots(&self.repository(), Some("hive"))
            .unwrap();
        list.sort_by_key(|s| s.time);
        list
    }
}

fn target() -> Target {
    Target::Drive {
        name: None,
        volume: VolumeRef {
            volume_id: None,
            label: Some("BACKUP".into()),
        },
        path: "repo".into(),
    }
}

/// Keeps one snapshot: every run forgets all but the newest.
fn keep_one() -> Retention {
    Retention {
        daily: 1,
        weekly: 0,
        monthly: 0,
        keep_forever: false,
    }
}

#[test]
fn a_job_that_keeps_everything_never_forgets_or_prunes() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let job = f.job(Retention {
        keep_forever: true,
        ..keep_one()
    });
    for n in 1..=4 {
        let report = f.run(&job, n);
        let log = fs::read_to_string(report.log_path.as_ref().unwrap()).unwrap();
        assert!(log.contains("keep forever: no forget or prune"), "{log}");
        assert!(!log.contains("forgot"), "forget never ran: {log}");
    }
    assert_eq!(f.snapshots().len(), 4, "nothing was removed");
}

#[test]
fn a_pinned_snapshot_survives_retention_until_it_s_unpinned() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let job = f.job(keep_one());
    let repo = f.repository();
    f.run(&job, 1);
    let first = f.snapshots()[0].id.clone();
    let first = f.restic.set_pinned(&repo, &first, true).unwrap();
    f.run(&job, 2);
    let second = f
        .snapshots()
        .iter()
        .find(|s| !s.is_pinned())
        .unwrap()
        .id
        .clone();
    let second = f.restic.set_pinned(&repo, &second, true).unwrap();
    for n in 3..=5 {
        f.run(&job, n);
    }
    let kept = f.snapshots();
    let pinned: Vec<&str> = kept
        .iter()
        .filter(|s| s.is_pinned())
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(
        pinned,
        [first.as_str(), second.as_str()],
        "both pinned snapshots survive keeping one: {kept:?}"
    );
    assert_eq!(kept.len(), 3, "the two pinned and the newest: {kept:?}");
    assert_eq!(
        f.restic.set_pinned(&repo, &first, true).unwrap(),
        first,
        "pinning again changes nothing and keeps its ID"
    );

    // Unpinned, the next run's retention removes it.
    let unpinned = f.restic.set_pinned(&repo, &first, false).unwrap();
    assert_ne!(unpinned, first, "restic rewrote it under a new ID");
    f.run(&job, 6);
    let after = f.snapshots();
    assert!(
        after.iter().all(|s| s.id != unpinned),
        "the unpinned one went: {after:?}"
    );
    assert!(after.iter().any(|s| s.id == second), "the other pin stays");
    assert_eq!(after.len(), 2);
}

/// Round 1's finding: a job could be named like the pin's old tag (`replica-pinned`). Its
/// snapshots aren't pinned until pinned, and pinning and unpinning never touch its own tag.
#[test]
fn a_job_named_like_the_old_pin_tag_keeps_its_snapshots_and_its_tag() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let job = Job {
        name: "replica-pinned".into(),
        ..f.job(keep_one())
    };
    assert!(replica_core::job::validate_jobs(std::slice::from_ref(&job)).is_empty());
    let repo = f.repository();
    f.run(&job, 1);
    let listed = |f: &Fixture| f.restic.snapshots(&repo, Some("replica-pinned")).unwrap();
    let mine = listed(&f);
    assert_eq!(mine.len(), 1);
    assert!(
        !mine[0].is_pinned(),
        "not pinned until pinned: {:?}",
        mine[0].tags
    );

    let pinned = f.restic.set_pinned(&repo, &mine[0].id, true).unwrap();
    let unpinned = f.restic.set_pinned(&repo, &pinned, false).unwrap();
    let mine = listed(&f);
    assert_eq!(mine.len(), 1, "still the job's own after unpinning");
    assert_eq!(mine[0].id, unpinned);
    assert_eq!(mine[0].tags, ["replica-pinned"]);
}
