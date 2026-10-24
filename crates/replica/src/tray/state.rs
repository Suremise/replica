//! The tray's decisions, apart from Windows: which icon, what the tooltip says, which jobs are
//! due, which are overdue, and what a finished run notifies. Time comes in as a parameter, so
//! the tests use a fake clock.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone};
use replica_core::i18n::tr;
use replica_core::job::Job;
use replica_core::platform::{Level, Notification};
use replica_core::run::{Outcome, RunReport};
use replica_core::schedule::DEFAULT_GRACE;
use replica_core::tr;

/// The tray icon's state, most important first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    Running,
    Failed,
    Overdue,
    Idle,
}

/// Running beats a failure, a failure beats overdue, and overdue beats idle.
pub fn icon_state(running: bool, failed: bool, overdue: bool) -> IconState {
    if running {
        IconState::Running
    } else if failed {
        IconState::Failed
    } else if overdue {
        IconState::Overdue
    } else {
        IconState::Idle
    }
}

/// Whether the built-in timer should start `job` now.
///
/// A job is due once its schedule's next time after its last run (any outcome, so a failing
/// job isn't retried in a loop) has come. A job that never ran counts from `since`, when the
/// tray started watching. So a run missed while the PC was off happens as soon as the tray is
/// back, once.
pub fn is_due<Tz: TimeZone>(
    job: &Job,
    last_run: Option<&DateTime<Tz>>,
    since: &DateTime<Tz>,
    now: &DateTime<Tz>,
) -> bool {
    let Some(schedule) = job.schedule.as_ref().filter(|_| job.enabled) else {
        return false;
    };
    let anchor = last_run.unwrap_or(since);
    schedule.next_after(anchor) <= *now
}

/// Whether `job` is overdue: its last success (or, if it never had one, `since`) is older than
/// its next due time plus the grace period.
pub fn is_overdue<Tz: TimeZone>(
    job: &Job,
    last_success: Option<&DateTime<Tz>>,
    since: &DateTime<Tz>,
    now: &DateTime<Tz>,
) -> bool {
    let Some(schedule) = job.schedule.as_ref().filter(|_| job.enabled) else {
        return false;
    };
    schedule.is_overdue(last_success.unwrap_or(since), now, DEFAULT_GRACE)
}

/// When `job` was next due after its last success (or `since`): the "due" of an overdue
/// notice.
pub fn due_since<Tz: TimeZone>(
    job: &Job,
    last_success: Option<&DateTime<Tz>>,
    since: &DateTime<Tz>,
) -> Option<DateTime<Tz>> {
    let schedule = job.schedule.as_ref()?;
    Some(schedule.next_after(last_success.unwrap_or(since)))
}

/// Overdue notices: at most one a day for each job.
#[derive(Debug, Default)]
pub struct OverdueNotices {
    last: HashMap<String, NaiveDate>,
}

impl OverdueNotices {
    /// Whether to notify that `job` is overdue `today`; records it if so.
    pub fn take(&mut self, job: &str, today: NaiveDate) -> bool {
        let key = job.to_lowercase();
        if self.last.get(&key) == Some(&today) {
            return false;
        }
        self.last.insert(key, today);
        true
    }

    /// Forgets `job`'s notice (it succeeded, so a later lapse notifies again).
    pub fn clear(&mut self, job: &str) {
        self.last.remove(&job.to_lowercase());
    }
}

/// What a finished run notifies: a failure always, a partly done run always (some of it
/// failed), a success only if the job asks for it, and a skipped run never (an overdue notice
/// covers a target that stays away).
pub fn notification_for(report: &RunReport, job: Option<&Job>) -> Option<Notification> {
    let name = report.job.as_str();
    let ok = report
        .targets
        .iter()
        .filter(|t| t.outcome == Outcome::Ok)
        .count();
    let note = |level, title: String, body: String| Notification {
        level,
        job: Some(report.job.clone()),
        title,
        body,
    };
    match report.outcome {
        Outcome::Failed => {
            let blamed = report.targets.iter().find(|t| t.outcome == Outcome::Failed);
            let body = match (blamed, &report.error) {
                (Some(t), _) => tr!(
                    "notify-run-failed-body",
                    target = t.target.as_str(),
                    reason = t.reason.clone().unwrap_or_default()
                ),
                (None, Some(error)) => tr!("notify-run-failed-reason", reason = error.as_str()),
                (None, None) => String::new(),
            };
            Some(note(
                Level::Error,
                tr!("notify-run-failed-title", job = name),
                body,
            ))
        }
        Outcome::Partly => Some(note(
            Level::Warning,
            tr!("notify-run-partly-title", job = name),
            tr!(
                "notify-run-partly-body",
                ok = ok,
                total = report.targets.len()
            ),
        )),
        Outcome::Ok if job.is_some_and(|j| j.notify_on_success) => Some(note(
            Level::Success,
            tr!("notify-run-ok-title", job = name),
            tr!("notify-run-ok-body", count = ok),
        )),
        Outcome::Ok | Outcome::Skipped => None,
    }
}

/// What the tooltip says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tooltip<'a> {
    Running {
        job: &'a str,
        percent: u32,
        queued: usize,
    },
    /// Running, but waiting its turn for a repository another run is using.
    Waiting {
        job: &'a str,
        /// The wait's own text: "Waiting for hive to finish with External HDD".
        wait: &'a str,
        queued: usize,
    },
    Failed {
        job: &'a str,
    },
    Overdue {
        count: usize,
    },
    Paused,
    Idle,
}

impl Tooltip<'_> {
    pub fn text(&self) -> String {
        match self {
            Tooltip::Running {
                job,
                percent,
                queued,
            } => {
                let mut text = tr!("tray-tooltip-running", job = *job, percent = *percent);
                if *queued > 0 {
                    text.push_str(&tr!("tray-tooltip-queued", count = *queued));
                }
                text
            }
            Tooltip::Waiting { job, wait, queued } => {
                let mut text = tr!("tray-tooltip-waiting", job = *job, wait = *wait);
                if *queued > 0 {
                    text.push_str(&tr!("tray-tooltip-queued", count = *queued));
                }
                text
            }
            Tooltip::Failed { job } => tr!("tray-tooltip-failed", job = *job),
            Tooltip::Overdue { count } => tr!("tray-tooltip-overdue", count = *count),
            Tooltip::Paused => tr("tray-tooltip-paused"),
            Tooltip::Idle => tr("tray-tooltip-idle"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeDelta};
    use replica_core::run::TargetReport;

    fn clock(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn job(schedule: &str) -> Job {
        Job {
            name: "hive".into(),
            schedule: Some(schedule.parse().unwrap()),
            ..Job::default()
        }
    }

    #[test]
    fn the_icon_shows_the_most_important_state() {
        assert_eq!(icon_state(true, true, true), IconState::Running);
        assert_eq!(icon_state(false, true, true), IconState::Failed);
        assert_eq!(icon_state(false, false, true), IconState::Overdue);
        assert_eq!(icon_state(false, false, false), IconState::Idle);
    }

    #[test]
    fn a_daily_job_is_due_at_its_time_after_its_last_run() {
        let job = job("daily 02:30");
        let started = clock("2026-10-20T09:00:00+01:00");
        let last = clock("2026-10-20T02:30:00+01:00");
        assert!(!is_due(
            &job,
            Some(&last),
            &started,
            &clock("2026-10-21T02:29:00+01:00")
        ));
        assert!(is_due(
            &job,
            Some(&last),
            &started,
            &clock("2026-10-21T02:30:00+01:00")
        ));
        // It ran at 02:30: not due again until tomorrow.
        let ran = clock("2026-10-21T02:30:05+01:00");
        assert!(!is_due(
            &job,
            Some(&ran),
            &started,
            &clock("2026-10-21T23:00:00+01:00")
        ));
    }

    #[test]
    fn a_run_missed_while_the_pc_was_off_happens_once_when_the_tray_is_back() {
        let job = job("daily 02:30");
        let last = clock("2026-10-19T02:30:00+01:00");
        let back = clock("2026-10-21T08:00:00+01:00");
        assert!(is_due(&job, Some(&last), &back, &back));
        let ran = clock("2026-10-21T08:00:01+01:00");
        assert!(!is_due(
            &job,
            Some(&ran),
            &back,
            &clock("2026-10-21T20:00:00+01:00")
        ));
    }

    #[test]
    fn a_job_that_never_ran_counts_from_when_the_tray_started() {
        let job = job("every 6h");
        let started = clock("2026-10-21T08:00:00Z");
        assert!(!is_due(
            &job,
            None,
            &started,
            &clock("2026-10-21T13:59:00Z")
        ));
        assert!(is_due(&job, None, &started, &clock("2026-10-21T14:00:00Z")));
    }

    #[test]
    fn a_failing_job_waits_for_its_next_time_rather_than_retrying_at_once() {
        let job = job("every 1h");
        let failed_at = clock("2026-10-21T10:00:00Z");
        let started = clock("2026-10-21T08:00:00Z");
        assert!(!is_due(
            &job,
            Some(&failed_at),
            &started,
            &clock("2026-10-21T10:30:00Z")
        ));
        assert!(is_due(
            &job,
            Some(&failed_at),
            &started,
            &clock("2026-10-21T11:00:00Z")
        ));
    }

    #[test]
    fn off_or_unscheduled_jobs_are_never_due_or_overdue() {
        let now = clock("2026-10-21T10:00:00Z");
        let long_ago = clock("2020-01-01T00:00:00Z");
        let mut off = job("every 1h");
        off.enabled = false;
        let by_hand = Job {
            name: "x".into(),
            ..Job::default()
        };
        for job in [&off, &by_hand] {
            assert!(!is_due(job, Some(&long_ago), &long_ago, &now));
            assert!(!is_overdue(job, Some(&long_ago), &long_ago, &now));
        }
    }

    /// A job whose runs keep failing (waiting too long for a shared repository, here) still
    /// shows Overdue: what counts is its last success, read from the history as the tray does.
    #[test]
    fn a_job_whose_runs_keep_failing_becomes_overdue() {
        use replica_core::history::History;
        let job = job("every 1h");
        let mut history = History::in_memory().unwrap();
        let ok_at = chrono::Utc::now() - TimeDelta::hours(6);
        let mut good = report(Outcome::Ok, &[(Outcome::Ok, "")]);
        good.started_at = ok_at;
        history.record(&good).unwrap();
        for hours_ago in [5, 4, 3, 2, 1] {
            let mut failed = report(Outcome::Failed, &[]);
            failed.started_at = chrono::Utc::now() - TimeDelta::hours(hours_ago);
            failed.error = Some(tr!(
                "error-wait-timed-out",
                job = "other",
                target = "nas",
                minutes = 120
            ));
            history.record(&failed).unwrap();
        }
        let last_success = history.last_success("hive").unwrap().unwrap();
        assert_eq!(last_success.timestamp(), ok_at.timestamp());
        let now = chrono::Utc::now();
        assert!(
            is_overdue(&job, Some(&last_success), &ok_at, &now),
            "five failed runs since don't hide it"
        );
    }

    #[test]
    fn a_run_that_waited_too_long_for_its_repository_notifies() {
        let mut timed_out = report(Outcome::Failed, &[]);
        let error = tr!(
            "error-wait-timed-out",
            job = "docs",
            target = "nas",
            minutes = 120
        );
        timed_out.error = Some(error.clone());
        let note = notification_for(&timed_out, Some(&job("daily 02:30"))).unwrap();
        assert_eq!(note.level, Level::Error);
        assert_eq!(note.body, error);
        assert!(note.body.contains("docs") && note.body.contains("nas"));
    }

    #[test]
    fn the_tooltip_says_what_a_waiting_job_waits_for() {
        let text = Tooltip::Waiting {
            job: "photos",
            wait: "Waiting for docs to finish with nas",
            queued: 1,
        }
        .text();
        assert_eq!(
            text,
            "Replica: photos, Waiting for docs to finish with nas, 1 more waiting"
        );
    }

    #[test]
    fn overdue_is_the_next_due_time_plus_an_hour_s_grace() {
        let job = job("daily 02:30");
        let success = clock("2026-10-20T02:30:00Z");
        let started = clock("2026-10-20T00:00:00Z");
        let due = clock("2026-10-21T02:30:00Z");
        assert_eq!(due_since(&job, Some(&success), &started), Some(due));
        let grace = due + TimeDelta::hours(1);
        assert!(!is_overdue(&job, Some(&success), &started, &grace));
        assert!(is_overdue(
            &job,
            Some(&success),
            &started,
            &(grace + TimeDelta::minutes(1))
        ));
    }

    #[test]
    fn an_overdue_job_notifies_once_a_day() {
        let mut notices = OverdueNotices::default();
        let day = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        assert!(notices.take("hive", day));
        assert!(!notices.take("HIVE", day));
        assert!(notices.take("hive", day.succ_opt().unwrap()));
        notices.clear("hive");
        assert!(notices.take("hive", day.succ_opt().unwrap()));
    }

    fn report(outcome: Outcome, targets: &[(Outcome, &str)]) -> RunReport {
        RunReport {
            id: None,
            job: "hive".into(),
            started_at: chrono::Utc::now(),
            duration: std::time::Duration::from_secs(1),
            outcome,
            error: None,
            targets: targets
                .iter()
                .map(|(o, why)| TargetReport {
                    target: "nas".into(),
                    mode: "versioned",
                    outcome: *o,
                    reason: Some((*why).into()),
                    files: 0,
                    bytes: 0,
                    snapshot_id: None,
                    problems: Vec::new(),
                })
                .collect(),
            log_path: None,
            waits: Vec::new(),
            gave_way: false,
        }
    }

    #[test]
    fn a_failure_always_notifies_and_a_success_only_if_asked() {
        let quiet = job("daily 02:30");
        let mut chatty = quiet.clone();
        chatty.notify_on_success = true;

        let failed = notification_for(
            &report(Outcome::Failed, &[(Outcome::Failed, "wrong password")]),
            Some(&quiet),
        )
        .unwrap();
        assert_eq!(failed.level, Level::Error);
        assert_eq!(failed.title, tr!("notify-run-failed-title", job = "hive"));
        assert_eq!(failed.body, "nas: wrong password");

        let mut before = report(Outcome::Failed, &[]);
        before.error = Some("the before step failed".into());
        assert_eq!(
            notification_for(&before, Some(&quiet)).unwrap().body,
            "the before step failed"
        );

        let partly = notification_for(
            &report(
                Outcome::Partly,
                &[(Outcome::Ok, ""), (Outcome::Failed, "x")],
            ),
            Some(&quiet),
        )
        .unwrap();
        assert_eq!(partly.level, Level::Warning);

        let ok = report(Outcome::Ok, &[(Outcome::Ok, "")]);
        assert!(notification_for(&ok, Some(&quiet)).is_none());
        assert_eq!(
            notification_for(&ok, Some(&chatty)).unwrap().level,
            Level::Success
        );
        let skipped = report(Outcome::Skipped, &[(Outcome::Skipped, "not connected")]);
        assert!(notification_for(&skipped, Some(&chatty)).is_none());
    }

    #[test]
    fn the_tooltip_says_what_runs_and_what_waits() {
        let running = Tooltip::Running {
            job: "hive",
            percent: 42,
            queued: 2,
        }
        .text();
        assert_eq!(running, "Replica: running hive (42%), 2 more waiting");
        assert_eq!(
            Tooltip::Overdue { count: 1 }.text(),
            "Replica: 1 job is overdue"
        );
        assert_eq!(Tooltip::Idle.text(), "Replica");
    }
}
