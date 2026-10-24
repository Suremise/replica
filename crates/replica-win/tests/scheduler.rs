//! The real Windows Task Scheduler, in a test-only folder that each test deletes when it ends,
//! pass or fail. The tasks point at an exe that doesn't exist and are never run.
#![cfg(windows)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use replica_core::job::Job;
use replica_win::scheduler::Scheduler;

/// A scheduler on `\ReplicaTest-<pid>-<time>-<test>`, emptied and removed when dropped.
struct TestFolder(Scheduler);

impl TestFolder {
    fn new(test: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(Scheduler::in_folder(&format!(
            r"\ReplicaTest-{}-{nanos}-{test}",
            std::process::id()
        )))
    }
}

impl Drop for TestFolder {
    fn drop(&mut self) {
        if let Err(error) = self.0.delete_all() {
            eprintln!("test tasks left behind in {}: {error}", self.0.folder());
        }
    }
}

fn exe() -> PathBuf {
    std::env::temp_dir()
        .join("replica-test-not-run")
        .join("replica-tray.exe")
}

fn job(name: &str, schedule: Option<&str>, enabled: bool) -> Job {
    Job {
        name: name.into(),
        enabled,
        schedule: schedule.map(|s| s.parse().unwrap()),
        ..Job::default()
    }
}

fn names(tasks: &[replica_win::scheduler::ScheduledTask]) -> Vec<String> {
    tasks.iter().map(|t| t.name.clone()).collect()
}

#[test]
fn sync_creates_a_task_per_scheduled_enabled_job() {
    let folder = TestFolder::new("sync");
    let s = &folder.0;
    let jobs = [
        job("hive", Some("daily 02:30"), true),
        job("photos", Some("every 6h"), true),
        job("off", Some("daily 03:00"), false),
        job("by-hand", None, true),
    ];
    let report = s.sync(&jobs, &exe()).unwrap();
    assert_eq!(report.created, ["hive", "photos"]);

    let all = ["hive", "photos", "off", "by-hand"];
    let tasks = s.list(&all).unwrap();
    assert_eq!(names(&tasks), ["hive", "photos"]);
    let hive = &tasks[0];
    assert_eq!(hive.job, "hive");
    assert_eq!(hive.schedule.as_deref(), Some("daily 02:30"));
    assert!(hive.enabled && !hive.orphan && !hive.running);
    assert!(hive.next_run.is_some(), "{hive:?}");
    assert_eq!(hive.last_run, None);
    assert_eq!(hive.last_result, None);
    assert_eq!(tasks[1].schedule.as_deref(), Some("every 6h"));

    // Nothing changed: nothing re-registered.
    assert_eq!(s.sync(&jobs, &exe()).unwrap(), Default::default());
}

#[test]
fn a_task_the_user_turned_off_stays_off() {
    let folder = TestFolder::new("disabled");
    let s = &folder.0;
    s.sync(&[job("hive", Some("daily 02:30"), true)], &exe())
        .unwrap();
    s.set_enabled("HIVE", false).unwrap();
    assert!(!s.list(&["hive"]).unwrap()[0].enabled);
    assert!(s.enabled_jobs().unwrap().is_empty());

    // A new schedule updates the task, and it stays off.
    let report = s
        .sync(&[job("hive", Some("weekly mon 09:00"), true)], &exe())
        .unwrap();
    assert_eq!(report.updated, ["hive"]);
    let task = &s.list(&["hive"]).unwrap()[0];
    assert_eq!(task.schedule.as_deref(), Some("weekly mon 09:00"));
    assert!(!task.enabled);

    s.set_enabled("hive", true).unwrap();
    assert_eq!(s.enabled_jobs().unwrap(), ["hive"]);
}

#[test]
fn a_job_that_goes_or_loses_its_schedule_loses_its_task() {
    let folder = TestFolder::new("removed");
    let s = &folder.0;
    let both = [
        job("hive", Some("daily 02:30"), true),
        job("photos", Some("every 6h"), true),
    ];
    s.sync(&both, &exe()).unwrap();
    let report = s
        .sync(&[job("hive", None, true), both[1].clone()], &exe())
        .unwrap();
    assert_eq!(report.removed, ["hive"]);
    let report = s.sync(&[], &exe()).unwrap();
    assert_eq!(report.removed, ["photos"]);
    assert!(s.list(&[]).unwrap().is_empty());
}

#[test]
fn orphans_are_tasks_whose_job_is_gone() {
    let folder = TestFolder::new("orphans");
    let s = &folder.0;
    s.sync(
        &[
            job("hive", Some("daily 02:30"), true),
            job("old", Some("every 1h"), true),
        ],
        &exe(),
    )
    .unwrap();
    let tasks = s.list(&["hive"]).unwrap();
    let orphans: Vec<_> = tasks.iter().filter(|t| t.orphan).map(|t| &t.name).collect();
    assert_eq!(orphans, ["old"]);
}

#[test]
fn delete_one_then_all_then_the_folder_is_gone() {
    let folder = TestFolder::new("delete");
    let s = &folder.0;
    s.sync(
        &[
            job("a", Some("every 1h"), true),
            job("b", Some("every 2h"), true),
            job("c", Some("every 3h"), true),
        ],
        &exe(),
    )
    .unwrap();
    s.delete("B").unwrap();
    assert_eq!(names(&s.list(&[]).unwrap()), ["a", "c"]);
    assert!(s.delete("nope").is_err());
    assert_eq!(s.delete_all().unwrap(), 2);
    assert!(s.list(&[]).unwrap().is_empty());
    assert_eq!(s.delete_all().unwrap(), 0, "the folder is gone too");
}

#[test]
fn turning_the_option_off_deletes_or_disables_them_all() {
    let folder = TestFolder::new("off");
    let s = &folder.0;
    let jobs = [
        job("a", Some("every 1h"), true),
        job("b", Some("every 2h"), true),
    ];
    s.sync(&jobs, &exe()).unwrap();
    assert_eq!(s.turn_off(false).unwrap(), 2);
    assert!(s.list(&[]).unwrap().iter().all(|t| !t.enabled));
    assert_eq!(s.turn_off(true).unwrap(), 2);
    assert!(s.list(&[]).unwrap().is_empty());
}

/// What the tray does when it sees the option turned off: the tasks that are on go, and any
/// the user turned off (kept) stay.
#[test]
fn deleting_the_enabled_tasks_keeps_the_ones_turned_off() {
    let folder = TestFolder::new("enabled");
    let s = &folder.0;
    s.sync(
        &[
            job("on", Some("every 1h"), true),
            job("kept", Some("every 2h"), true),
        ],
        &exe(),
    )
    .unwrap();
    s.set_enabled("kept", false).unwrap();
    assert_eq!(s.delete_enabled().unwrap(), 1);
    assert_eq!(names(&s.list(&[]).unwrap()), ["kept"]);
}
