//! Jobs that share a repository take turns: a run waits for a repository another run
//! holds, up to a bound, gives way to a job queued behind it that could run, and restic's own
//! lock held by a restic outside Replica is retried with backoff. Every wait is in the report,
//! however it ended.
//!
//! With the pinned restic, in temp folders only (restic's cache too). Nothing here waits for a
//! length of time: each release happens in answer to the run's own "waiting" event, and every
//! outside restic is stopped and reaped however the test ends ([`OutsideRestic`]).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use replica_core::cancel::CancelToken;
use replica_core::history::History;
use replica_core::job::{Job, Source, Target, VolumeRef};
use replica_core::platform::fake::{FakeResolver, MemorySecretStore, PlainTree};
use replica_core::platform::{Credential, Secret, SecretStore};
use replica_core::repo_lock::{self, FileLocks, Turn, Wait};
use replica_core::restic::{Repository, Restic, password_secret_name};
use replica_core::run::{Outcome, RunContext, RunEvent, RunReport, Waits, run_job};

mod common;

const PASSWORD: &str = "repo-lock-test-password-7c2a";

struct Fixture {
    temp: tempfile::TempDir,
    resolver: FakeResolver,
    secrets: MemorySecretStore,
    history: History,
    restic: Restic,
}

impl Fixture {
    /// One drive, BACKUP, at `<temp>/drive`, with a repository at `repo` and another at
    /// `other`, both initialised. restic's cache is `<temp>/restic-cache`.
    fn new() -> Option<Fixture> {
        let restic_path = common::restic()?;
        let temp = tempfile::tempdir().unwrap();
        let restic = Restic::new(restic_path)
            .unwrap()
            .with_cache_dir(temp.path().join("restic-cache"));
        let drive = temp.path().join("drive");
        fs::create_dir_all(&drive).unwrap();
        let f = Fixture {
            resolver: FakeResolver {
                volumes: vec![(None, Some("BACKUP".into()), drive)],
                ..Default::default()
            },
            secrets: MemorySecretStore::default(),
            history: History::open(&temp.path().join("history.db")).unwrap(),
            temp,
            restic,
        };
        fs::create_dir_all(f.path("src")).unwrap();
        fs::write(f.path("src/a.txt"), "a").unwrap();
        for repo in ["repo", "other"] {
            f.secrets
                .set(
                    &password_secret_name(&target(repo)),
                    &Credential {
                        username: None,
                        secret: Secret::new(PASSWORD),
                    },
                )
                .unwrap();
            f.restic.init(&f.repository(repo)).unwrap();
        }
        Some(f)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn repository(&self, repo: &str) -> Repository {
        Repository::new(
            self.path(&format!("drive/{repo}")).to_string_lossy(),
            Secret::new(PASSWORD),
        )
        .with_cache_dir(self.path("restic-cache"))
    }

    fn locks(&self) -> FileLocks {
        FileLocks::new(self.path("locks"))
    }

    fn job(&self, name: &str, repo: &str) -> Job {
        Job {
            name: name.into(),
            sources: vec![Source {
                path: self.path("src"),
                exclude: vec![],
            }],
            targets: vec![target(repo)],
            ..Job::default()
        }
    }

    fn run(&mut self, job: &Job, waits: Waits, on_event: &mut dyn FnMut(&RunEvent)) -> RunReport {
        self.run_with(job, waits, None, &CancelToken::new(), on_event)
    }

    fn run_with(
        &mut self,
        job: &Job,
        waits: Waits,
        give_way: Option<&dyn Fn() -> bool>,
        cancel: &CancelToken,
        on_event: &mut dyn FnMut(&RunEvent),
    ) -> RunReport {
        let (logs, locks, staging) = (self.path("logs"), self.path("locks"), self.path("staging"));
        let repo_locks = self.locks();
        let ctx = RunContext {
            restic: Some(&self.restic),
            resolver: &self.resolver,
            secrets: &self.secrets,
            logs_dir: &logs,
            locks_dir: &locks,
            repo_locks: &repo_locks,
            waits,
            give_way,
            staging_dir: &staging,
            processes: &PlainTree,
            cancel,
        };
        run_job(job, &ctx, &mut self.history, on_event)
    }

    /// Takes `job`'s repositories the way a run does, as another run of it would hold them.
    fn hold(&self, job: &Job) -> repo_lock::Held {
        repo_lock::acquire(
            job,
            &self.locks(),
            &self.path("locks"),
            Turn::alone(Duration::ZERO),
            &CancelToken::new(),
            &mut Vec::new(),
            &mut |_| panic!("free"),
        )
        .unwrap()
    }

    /// `restic backup --stdin` against `repo`, holding its (shared) restic lock until it's
    /// [`OutsideRestic::finish`]ed. Returns once its lock is in the repository.
    fn outside_restic(&self, repo: &str) -> OutsideRestic {
        let child = Command::new(self.restic.path())
            .args(["backup", "--stdin", "--stdin-filename", "outside.txt"])
            .env("RESTIC_REPOSITORY", self.path(&format!("drive/{repo}")))
            .env("RESTIC_PASSWORD", PASSWORD)
            .env("RESTIC_CACHE_DIR", self.path("restic-cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut outside = OutsideRestic(Some(child));
        outside.stdin().unwrap().write_all(b"outside").unwrap();
        let locks = self.path(&format!("drive/{repo}/locks"));
        let start = Instant::now();
        while !has_files(&locks) {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the outside restic never took its lock"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        outside
    }
}

/// A restic started outside Replica. Dropping it kills and reaps it if it's still running, so a
/// failed test never leaves it behind.
struct OutsideRestic(Option<Child>);

impl OutsideRestic {
    fn stdin(&mut self) -> Option<&mut ChildStdin> {
        self.0.as_mut()?.stdin.as_mut()
    }

    /// Lets it finish its backup (its stdin closed): whether it succeeded.
    fn finish(&mut self) -> bool {
        let mut child = self.0.take().expect("not finished yet");
        drop(child.stdin.take());
        child.wait().unwrap().success()
    }

    /// Closes its stdin, so it finishes, without waiting for it (from a run's event callback).
    fn let_go(&mut self) {
        if let Some(child) = self.0.as_mut() {
            drop(child.stdin.take());
        }
    }
}

impl Drop for OutsideRestic {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn has_files(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

fn target(repo: &str) -> Target {
    Target::Drive {
        name: Some(format!("BACKUP {repo}")),
        volume: VolumeRef {
            volume_id: None,
            label: Some("BACKUP".into()),
        },
        path: repo.into(),
    }
}

fn waits(turn: Duration, restic_lock: Duration) -> Waits {
    Waits { turn, restic_lock }
}

/// (kind, job, how it ended) of each of a report's waits.
fn waits_of(report: &RunReport) -> Vec<(&'static str, Option<&str>, &'static str)> {
    report
        .waits
        .iter()
        .map(|w| (w.wait.kind(), w.wait.job(), w.ended))
        .collect()
}

#[test]
fn a_run_waits_for_the_job_using_its_repository_then_succeeds() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let first = f.job("first", "repo");
    let second = f.job("second", "repo");
    let mut held = Some(f.hold(&first));
    let mut events = Vec::new();
    let report = f.run(&second, Waits::default(), &mut |e| {
        if let RunEvent::Waiting(_) = e {
            // The other run finishes: it lets go.
            held = None;
        }
        if matches!(e, RunEvent::Waiting(_) | RunEvent::Waited) {
            events.push(e.clone());
        }
    });
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert_eq!(
        events,
        [
            RunEvent::Waiting(Wait::Turn {
                target: "BACKUP repo".into(),
                job: Some("first".into())
            }),
            RunEvent::Waited
        ]
    );
    assert_eq!(waits_of(&report), [("turn", Some("first"), "ok")]);
    let log = fs::read_to_string(report.log_path.as_ref().unwrap()).unwrap();
    assert!(
        log.contains("Waiting for first to finish with BACKUP repo"),
        "{log}"
    );
    assert!(!log.to_lowercase().contains("already locked"), "{log}");
}

#[test]
fn a_run_that_waits_past_its_bound_fails_clearly_and_is_recorded() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let first = f.job("first", "repo");
    let second = f.job("second", "repo");
    let _held = f.hold(&first);
    let report = f.run(&second, waits(Duration::ZERO, Duration::ZERO), &mut |_| {});
    assert_eq!(report.outcome, Outcome::Failed, "{report:?}");
    let error = report.error.clone().unwrap();
    assert!(
        error.contains("first") && error.contains("BACKUP repo"),
        "it names who and where: {error}"
    );
    assert!(report.targets.is_empty(), "nothing was run");
    assert_eq!(
        waits_of(&report),
        [("turn", Some("first"), "timed-out")],
        "the wait that failed it is reported"
    );
    let recorded = f.history.recent(Some("second"), 1).unwrap();
    assert_eq!(recorded[0].outcome, Outcome::Failed);
    assert_eq!(recorded[0].error.as_deref(), Some(error.as_str()));
    assert_eq!(f.history.last_success("second").unwrap(), None);
}

#[test]
fn a_stop_while_waiting_ends_the_run_and_the_wait_is_reported() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let _held = f.hold(&f.job("first", "repo"));
    let second = f.job("second", "repo");
    let cancel = CancelToken::new();
    let stop = cancel.clone();
    let report = f.run_with(&second, Waits::default(), None, &cancel, &mut |e| {
        if let RunEvent::Waiting(_) = e {
            stop.cancel();
        }
    });
    assert_eq!(report.outcome, Outcome::Failed, "{report:?}");
    assert_eq!(waits_of(&report), [("turn", Some("first"), "cancelled")]);
}

#[test]
fn a_waiting_run_gives_way_to_a_job_behind_it_and_does_nothing() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let _held = f.hold(&f.job("first", "repo"));
    let second = f.job("second", "repo");
    let behind = f.job("behind", "other");
    let locks = f.locks();
    let behind_is_free = || repo_lock::is_free(&behind, &locks);
    let report = f.run_with(
        &second,
        Waits::default(),
        Some(&behind_is_free),
        &CancelToken::new(),
        &mut |e| assert!(!matches!(e, RunEvent::Started { .. }), "{e:?}"),
    );
    assert!(report.gave_way, "{report:?}");
    assert_eq!(report.outcome, Outcome::Skipped);
    assert_eq!(report.id, None, "nothing recorded");
    assert_eq!(report.log_path, None, "nothing logged");
    assert_eq!(waits_of(&report), [("turn", Some("first"), "gave-way")]);
    assert!(f.history.recent(Some("second"), 1).unwrap().is_empty());
}

#[test]
fn a_job_on_another_repository_never_waits() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let _held = f.hold(&f.job("first", "repo"));
    let elsewhere = f.job("elsewhere", "other");
    let report = f.run(
        &elsewhere,
        waits(Duration::ZERO, Duration::ZERO),
        &mut |e| {
            assert!(!matches!(e, RunEvent::Waiting(_)), "{e:?}");
        },
    );
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert!(report.waits.is_empty());
}

#[test]
fn restic_s_own_lock_is_retried_with_backoff_until_it_s_free() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let mut outside = f.outside_restic("repo");
    let job = f.job("hive", "repo");
    let mut events = Vec::new();
    let report = f.run(&job, Waits::default(), &mut |e| {
        if let RunEvent::Waiting(_) = e {
            // The outside restic finishes its backup and lets go of its lock.
            outside.let_go();
        }
        if matches!(e, RunEvent::Waiting(_) | RunEvent::Waited) {
            events.push(e.clone());
        }
    });
    assert!(outside.finish());
    assert_eq!(report.outcome, Outcome::Ok, "{report:?}");
    assert_eq!(
        events,
        [
            RunEvent::Waiting(Wait::ResticLock {
                target: "BACKUP repo".into()
            }),
            RunEvent::Waited
        ],
        "its backup shares the outside lock; its forget waits for it"
    );
    assert_eq!(
        waits_of(&report),
        [("restic-lock", None, "ok")],
        "the wait is reported"
    );
    let log = fs::read_to_string(report.log_path.as_ref().unwrap()).unwrap();
    assert!(
        log.contains("restic unlock"),
        "stale locks are cleared first: {log}"
    );
    assert_eq!(
        f.restic
            .snapshots(&f.repository("repo"), None)
            .unwrap()
            .len(),
        2,
        "both backups were kept: the live lock wasn't removed"
    );
}

#[test]
fn restic_s_own_lock_held_too_long_is_a_clear_failure() {
    let Some(mut f) = Fixture::new() else {
        return;
    };
    let mut outside = f.outside_restic("repo");
    let job = f.job("hive", "repo");
    let report = f.run(&job, waits(Duration::ZERO, Duration::ZERO), &mut |_| {});
    assert!(outside.finish(), "the outside restic was left alone");
    // The snapshot is made (backup shares the lock); retention couldn't run.
    assert_eq!(report.outcome, Outcome::Partly, "{report:?}");
    let reason = report.targets[0].reason.clone().unwrap();
    assert!(
        reason.contains("stayed locked by another restic"),
        "{reason}"
    );
    assert!(report.targets[0].snapshot_id.is_some());
    assert_eq!(
        waits_of(&report),
        [("restic-lock", None, "timed-out")],
        "the wait that gave up is reported"
    );
}

/// A quick listing outside a run (`snapshots`, `ls`) retries restic's own lock briefly
///: a call that finds the repository locked is tried again (once at once, after
/// `restic unlock`), up to its bound; then the lock's error comes back as it is.
#[test]
fn a_listing_retries_restic_s_lock_briefly() {
    use replica_core::restic::ResticError;
    let Some(f) = Fixture::new() else {
        return;
    };
    let repo = f.repository("repo");
    let locked = || ResticError::Locked {
        location: "repo".into(),
        detail: "held by PID 1".into(),
    };
    let cancel = CancelToken::new();

    let mut calls = 0;
    let listed =
        repo_lock::retry_locked(&f.restic, &repo, Duration::from_secs(60), &cancel, || {
            calls += 1;
            if calls < 3 {
                Err(locked())
            } else {
                f.restic.snapshots(&repo, None)
            }
        });
    assert!(listed.is_ok(), "{listed:?}");
    assert_eq!(calls, 3, "locked twice, then listed");

    let mut calls = 0;
    let given_up = repo_lock::retry_locked(&f.restic, &repo, Duration::ZERO, &cancel, || {
        calls += 1;
        Err::<(), _>(locked())
    });
    assert_eq!(given_up, Err(locked()));
    assert_eq!(calls, 2, "once more after restic unlock, then the bound");

    cancel.cancel();
    let stopped =
        repo_lock::retry_locked(&f.restic, &repo, Duration::from_secs(60), &cancel, || {
            Err::<(), _>(locked())
        });
    assert_eq!(stopped, Err(ResticError::Cancelled));
}
