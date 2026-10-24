//! Restoring several items into one folder, and the test restore, against the pinned restic, in
//! temp folders only. It needs the pinned restic: see `common::restic`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use replica_core::cancel::CancelToken;
use replica_core::exclude::ExcludeRules;
use replica_core::history::{History, HistoryFilter};
use replica_core::platform::Secret;
use replica_core::restic::{
    BackupRequest, Repository, Restic, ResticError, RestoreRequest, snapshot_path,
};
use replica_core::run::Outcome;
use replica_core::test_restore::{self, Check, Picker};

mod common;

struct Fixture {
    restic: Restic,
    repo: Repository,
    temp: tempfile::TempDir,
}

impl Fixture {
    /// A repository with one snapshot of `src` (a.txt, dir/b.txt, dir/c.txt), job "hive".
    fn new() -> Option<Self> {
        let restic = Restic::new(common::restic()?).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::new(
            temp.path().join("repo").to_string_lossy(),
            Secret::new("test-restore-fixture"),
        )
        .with_cache_dir(temp.path().join("cache"));
        let f = Fixture { restic, repo, temp };
        f.write("src/a.txt", "a");
        f.write("src/dir/b.txt", "bee");
        f.write("src/dir/c.txt", "sea");
        f.restic.init(&f.repo).unwrap();
        let src = f.path("src");
        let excludes = ExcludeRules::new([(src.as_path(), [] as [&str; 0])], true).unwrap();
        f.restic
            .backup(
                &f.repo,
                &BackupRequest {
                    job: "hive",
                    sources: std::slice::from_ref(&src),
                    excludes: &excludes,
                },
                &mut |_| {},
                &CancelToken::new(),
            )
            .unwrap();
        Some(f)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn item(&self, rel: &str) -> String {
        snapshot_path(&self.path(rel))
    }

    fn request<'a>(&'a self, to: &'a Path, sources: &'a [PathBuf]) -> RestoreRequest<'a> {
        RestoreRequest {
            job: "hive",
            snapshot: "latest",
            item: None,
            target: to,
            sources,
            allow_over_source: false,
        }
    }

    fn test_restore(&self, scratch: &str) -> test_restore::TestRestore {
        test_restore::run(
            &self.restic,
            &self.repo,
            "hive",
            10,
            &self.path(scratch),
            &mut Picker::new(7),
            &CancelToken::new(),
        )
        .unwrap()
    }
}

#[test]
fn several_items_restore_into_one_folder_each_by_its_name() {
    let Some(f) = Fixture::new() else { return };
    let to = f.path("restored");
    let (a, dir) = (f.item("src/a.txt"), f.item("src/dir"));
    let sources = [f.path("src")];
    let done = f
        .restic
        .restore_many(
            &f.repo,
            &f.request(&to, &sources),
            &[&a, &dir],
            &CancelToken::new(),
        )
        .unwrap();
    // restic counts the folder too.
    assert!(done.files_restored >= 3, "{done:?}");
    assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "a");
    assert_eq!(fs::read_to_string(to.join("dir/b.txt")).unwrap(), "bee");
    assert_eq!(fs::read_to_string(to.join("dir/c.txt")).unwrap(), "sea");

    // The folder is checked first: it isn't empty now.
    let again = f.restic.restore_many(
        &f.repo,
        &f.request(&to, &sources),
        &[&a],
        &CancelToken::new(),
    );
    assert!(matches!(
        again,
        Err(ResticError::RestoreTargetNotEmpty { .. })
    ));

    // Two items of one name would land on each other: refused before anything is restored.
    let other = f.path("other");
    let b = f.item("src/dir/b.txt");
    let clash = f.restic.restore_many(
        &f.repo,
        &f.request(&other, &sources),
        &[&b, "/X/b.txt"],
        &CancelToken::new(),
    );
    assert!(matches!(clash, Err(ResticError::RestoreSameName { .. })));
    assert!(!other.exists());

    // Over a source only when allowed, as `restore` does.
    let over = f.restic.restore_many(
        &f.repo,
        &f.request(&f.path("src/dir"), &sources),
        &[&a],
        &CancelToken::new(),
    );
    assert!(over.is_err());
}

#[test]
fn a_test_restore_matches_its_sources() {
    let Some(f) = Fixture::new() else { return };
    let report = f.test_restore("scratch");
    assert_eq!(
        report.files.len(),
        3,
        "fewer files than asked for: all of them"
    );
    assert_eq!(report.count(Check::Same), 3);
    assert_eq!(report.outcome(), Outcome::Ok);
    assert!(report.files.iter().all(|c| c.source.exists()));
}

#[test]
fn a_test_restore_finds_a_file_whose_bytes_changed_but_not_its_size_or_time() {
    let Some(f) = Fixture::new() else { return };
    // Damage b.txt the way a failing disk would: same size, same modified time.
    let b = f.path("src/dir/b.txt");
    let time = fs::metadata(&b).unwrap().modified().unwrap();
    fs::write(&b, "BEE").unwrap();
    fs::File::options()
        .write(true)
        .open(&b)
        .unwrap()
        .set_modified(time)
        .unwrap();
    // And edit c.txt normally: a newer time, so it's not compared.
    std::thread::sleep(Duration::from_millis(10));
    f.write("src/dir/c.txt", "sea, edited");

    let report = f.test_restore("scratch");
    let check_of = |name: &str| {
        report
            .files
            .iter()
            .find(|c| c.item.ends_with(name))
            .unwrap()
            .check
    };
    assert_eq!(check_of("b.txt"), Check::Differs);
    assert_eq!(check_of("c.txt"), Check::ChangedSince);
    assert_eq!(check_of("a.txt"), Check::Same);
    assert_eq!(report.outcome(), Outcome::Failed);

    // It's kept in the history apart from runs.
    let history = History::in_memory().unwrap();
    let record = report.record("hive", "repo", Utc::now(), Duration::from_secs(2));
    history.record_test_restore(&record).unwrap();
    let kept = history
        .test_restores(
            &HistoryFilter {
                job: Some("HIVE".into()),
                outcome: None,
            },
            10,
        )
        .unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].outcome, Outcome::Failed);
    assert_eq!((kept[0].checked, kept[0].differing), (2, 1));
    assert!(kept[0].detail.as_deref().unwrap().ends_with("b.txt"));
    assert!(
        history.recent(Some("hive"), 10).unwrap().is_empty(),
        "not a run"
    );
}

#[test]
fn a_test_restore_of_a_job_without_snapshots_says_so() {
    let Some(f) = Fixture::new() else { return };
    let err = test_restore::run(
        &f.restic,
        &f.repo,
        "other-job",
        5,
        &f.path("scratch"),
        &mut Picker::new(1),
        &CancelToken::new(),
    )
    .unwrap_err();
    assert!(matches!(err, ResticError::SnapshotNotFound { .. }));
}

#[test]
fn the_history_filters_before_it_limits_and_lists_every_job() {
    use replica_core::run::RunReport;
    let mut history = History::in_memory().unwrap();
    let start = Utc::now() - chrono::TimeDelta::days(30);
    // One old failure of "archive", then 600 newer runs of "docs".
    let report = |job: &str, at, outcome| RunReport {
        id: None,
        job: job.into(),
        started_at: at,
        duration: Duration::from_secs(1),
        outcome,
        error: None,
        targets: Vec::new(),
        log_path: None,
        waits: Vec::new(),
        gave_way: false,
    };
    history
        .record(&report("archive", start, Outcome::Failed))
        .unwrap();
    for i in 1..=600 {
        let at = start + chrono::TimeDelta::minutes(i);
        history.record(&report("docs", at, Outcome::Ok)).unwrap();
    }
    let all = HistoryFilter::default();
    assert_eq!(history.runs(&all, 500).unwrap().len(), 500);
    assert!(
        history
            .runs(&all, 500)
            .unwrap()
            .iter()
            .all(|r| r.job == "docs")
    );
    let archive = HistoryFilter {
        job: Some("Archive".into()),
        outcome: None,
    };
    assert_eq!(history.runs(&archive, 500).unwrap().len(), 1);
    let failed = HistoryFilter {
        job: None,
        outcome: Some(Outcome::Failed),
    };
    let found = history.runs(&failed, 500).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].job, "archive");
    assert_eq!(history.runs(&all, 700).unwrap().len(), 601);
    assert_eq!(history.job_names().unwrap(), ["archive", "docs"]);
}
