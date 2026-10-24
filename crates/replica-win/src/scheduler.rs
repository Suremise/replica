//! Replica's Windows Task Scheduler tasks: the optional way to run schedules (off by default),
//! and everything a user needs to see and manage them ("nothing hidden").
//!
//! - Every task lives in the `\Replica\` folder, one per scheduled job, named after the job. It
//!   runs `replica-tray.exe --run <job>` as the user, while they're logged on (no stored
//!   password, no admin rights).
//! - [`Scheduler::sync`] makes the folder match the jobs: a task for each enabled job with a
//!   schedule, updated when its schedule changes, and none for a job that's gone, turned off or
//!   unscheduled. A task the user disabled stays disabled.
//! - [`Scheduler::list`] shows each task with its job, trigger, next and last run, result and
//!   whether it's enabled, and flags **orphans**: tasks whose job no longer exists.
//! - [`Scheduler::set_enabled`], [`Scheduler::delete`] and [`Scheduler::delete_all`] manage them,
//!   and [`Scheduler::turn_off`] is what turning the option off does: delete them all (the
//!   default) or disable them.

use std::path::Path;

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Weekday};
use replica_core::job::Job;
use replica_core::platform::PlatformError;
use replica_core::schedule::Schedule;
use replica_core::tr;

use crate::win32::tasks::{TaskFolder, TaskInfo};

/// Replica's Task Scheduler folder.
pub const FOLDER: &str = r"\Replica";

/// One of Replica's tasks, as the window's Scheduled tasks view and `replica tasks list` show it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTask {
    /// The task's name: the job's name.
    pub name: String,
    /// The job it runs, from its definition.
    pub job: String,
    /// Its job no longer exists.
    pub orphan: bool,
    /// Its schedule as Replica wrote it (`daily 02:30`), if it says.
    pub schedule: Option<String>,
    pub enabled: bool,
    pub running: bool,
    pub next_run: Option<NaiveDateTime>,
    pub last_run: Option<NaiveDateTime>,
    /// The last run's exit code (0 ok, 1 failed, 2 partly, 3 skipped), or Task Scheduler's own
    /// code; `None` if it never ran.
    pub last_result: Option<i32>,
}

/// What a sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
}

/// Replica's tasks in one Task Scheduler folder: [`FOLDER`], or a test's own.
#[derive(Debug, Clone)]
pub struct Scheduler {
    folder: String,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

fn failed(e: std::io::Error) -> PlatformError {
    PlatformError(tr!("error-tasks-failed", detail = e.to_string()))
}

impl Scheduler {
    pub fn new() -> Self {
        Self::in_folder(FOLDER)
    }

    /// Tasks in another folder: for tests, which use their own and delete it afterwards.
    pub fn in_folder(folder: &str) -> Self {
        Self {
            folder: folder.to_owned(),
        }
    }

    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// Whether the folder is in Task Scheduler (it's made with the first task and removed with
    /// the last).
    pub fn folder_exists(&self) -> Result<bool, PlatformError> {
        Ok(self.open(false)?.is_some())
    }

    fn open(&self, create: bool) -> Result<Option<TaskFolder>, PlatformError> {
        TaskFolder::open(&self.folder, create).map_err(failed)
    }

    /// Every task in the folder, with orphans flagged against `jobs` (the job names there are).
    pub fn list(&self, jobs: &[&str]) -> Result<Vec<ScheduledTask>, PlatformError> {
        let Some(folder) = self.open(false)? else {
            return Ok(Vec::new());
        };
        let mut tasks: Vec<_> = folder
            .tasks()
            .map_err(failed)?
            .into_iter()
            .map(|t| describe(t, jobs))
            .collect();
        tasks.sort_by_key(|t| t.name.to_lowercase());
        Ok(tasks)
    }

    /// Makes the folder match `jobs`: a task running `exe --run <job>` for each enabled job
    /// with a schedule, and no other task. A task that's already right is left as it is; one
    /// the user disabled stays disabled.
    pub fn sync(&self, jobs: &[Job], exe: &Path) -> Result<SyncReport, PlatformError> {
        let wanted: Vec<&Job> = jobs
            .iter()
            .filter(|j| j.enabled && j.schedule.is_some())
            .collect();
        let folder = match self.open(!wanted.is_empty())? {
            Some(folder) => folder,
            None => return Ok(SyncReport::default()),
        };
        let existing = folder.tasks().map_err(failed)?;
        let mut report = SyncReport::default();
        for job in &wanted {
            let schedule = job.schedule.as_ref().expect("filtered on schedule");
            let current = existing
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(&job.name));
            let enabled = current.is_none_or(|t| t.enabled);
            let marker = data_text(&job.name, schedule, exe);
            if current.is_some_and(|t| data_of(&t.xml).as_deref() == Some(marker.as_str())) {
                continue;
            }
            let xml = task_xml(
                &job.name,
                schedule,
                exe,
                enabled,
                Local::now().naive_local(),
            );
            folder.register(&job.name, &xml).map_err(failed)?;
            match current {
                Some(_) => report.updated.push(job.name.clone()),
                None => report.created.push(job.name.clone()),
            }
        }
        for task in &existing {
            if !wanted
                .iter()
                .any(|j| j.name.eq_ignore_ascii_case(&task.name))
            {
                folder.delete(&task.name).map_err(failed)?;
                report.removed.push(task.name.clone());
            }
        }
        drop(folder);
        if wanted.is_empty() {
            self.remove_folder_if_empty()?;
        }
        Ok(report)
    }

    /// Enables or disables the task `name`.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), PlatformError> {
        let folder = self.require(name)?;
        folder
            .set_enabled(&self.exact(&folder, name)?, enabled)
            .map_err(failed)
    }

    /// Starts the task `name` now, as Task Scheduler would at its time.
    pub fn run_now(&self, name: &str) -> Result<(), PlatformError> {
        let folder = self.require(name)?;
        folder.run(&self.exact(&folder, name)?).map_err(failed)
    }

    /// Deletes the task `name`.
    pub fn delete(&self, name: &str) -> Result<(), PlatformError> {
        let folder = self.require(name)?;
        folder.delete(&self.exact(&folder, name)?).map_err(failed)?;
        drop(folder);
        self.remove_folder_if_empty()
    }

    /// Deletes every task in the folder, and the folder. Returns how many there were.
    pub fn delete_all(&self) -> Result<usize, PlatformError> {
        let Some(folder) = self.open(false)? else {
            return Ok(0);
        };
        let tasks = folder.tasks().map_err(failed)?;
        for task in &tasks {
            folder.delete(&task.name).map_err(failed)?;
        }
        drop(folder);
        self.remove_folder_if_empty()?;
        Ok(tasks.len())
    }

    /// Turning the Task Scheduler option off: deletes Replica's tasks (`delete`, the default
    /// the window offers), or disables them all. Tasks are never left running silently.
    pub fn turn_off(&self, delete: bool) -> Result<usize, PlatformError> {
        if delete {
            return self.delete_all();
        }
        let Some(folder) = self.open(false)? else {
            return Ok(0);
        };
        let tasks = folder.tasks().map_err(failed)?;
        for task in &tasks {
            folder.set_enabled(&task.name, false).map_err(failed)?;
        }
        Ok(tasks.len())
    }

    /// Deletes the tasks that are on, leaving any the user turned off. Returns how many went.
    /// The tray does this when it sees the option turned off without the window (which asks
    /// first, and turns them off if the user keeps them).
    pub fn delete_enabled(&self) -> Result<usize, PlatformError> {
        let Some(folder) = self.open(false)? else {
            return Ok(0);
        };
        let enabled: Vec<_> = folder
            .tasks()
            .map_err(failed)?
            .into_iter()
            .filter(|t| t.enabled)
            .collect();
        for task in &enabled {
            folder.delete(&task.name).map_err(failed)?;
        }
        drop(folder);
        self.remove_folder_if_empty()?;
        Ok(enabled.len())
    }

    /// The names of the jobs with an enabled task.
    pub fn enabled_jobs(&self) -> Result<Vec<String>, PlatformError> {
        Ok(self
            .list(&[])?
            .into_iter()
            .filter(|t| t.enabled)
            .map(|t| t.job)
            .collect())
    }

    fn require(&self, name: &str) -> Result<TaskFolder, PlatformError> {
        self.open(false)?
            .ok_or_else(|| PlatformError(tr!("error-task-not-found", task = name)))
    }

    /// The task's real name, matched ignoring case as job names are.
    fn exact(&self, folder: &TaskFolder, name: &str) -> Result<String, PlatformError> {
        folder
            .tasks()
            .map_err(failed)?
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .map(|t| t.name)
            .ok_or_else(|| PlatformError(tr!("error-task-not-found", task = name)))
    }

    fn remove_folder_if_empty(&self) -> Result<(), PlatformError> {
        if let Some(folder) = self.open(false)?
            && folder.tasks().map_err(failed)?.is_empty()
        {
            drop(folder);
            TaskFolder::delete_folder(&self.folder).map_err(failed)?;
        }
        Ok(())
    }
}

fn describe(task: TaskInfo, jobs: &[&str]) -> ScheduledTask {
    let data = data_of(&task.xml);
    let field = |key: &str| {
        data.as_deref().and_then(|d| {
            d.split(';')
                .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
                .map(str::to_owned)
        })
    };
    let job = field("job").unwrap_or_else(|| task.name.clone());
    ScheduledTask {
        orphan: !jobs.iter().any(|j| j.eq_ignore_ascii_case(&job)),
        schedule: field("schedule"),
        enabled: task.enabled,
        running: task.running,
        next_run: task.next_run.and_then(from_ole_date),
        last_run: task.last_run.and_then(from_ole_date),
        // 0x41303 "has not yet run".
        last_result: task
            .last_run
            .map(|_| task.last_result)
            .filter(|r| *r != 0x41303),
        name: task.name,
        job,
    }
}

/// An OLE automation date (days since 1899-12-30, local time) as a local date and time.
pub fn from_ole_date(days: f64) -> Option<NaiveDateTime> {
    let base = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let millis = (days * 86_400_000.0).round() as i64;
    base.checked_add_signed(TimeDelta::milliseconds(millis))
}

/// What Replica records in a task's `<Data>`: the job, its schedule and the exe, so a task is
/// re-registered exactly when one of them changes, and the list can show them.
fn data_text(job: &str, schedule: &Schedule, exe: &Path) -> String {
    format!(
        "replica;job={job};schedule={schedule};exe={}",
        exe.display()
    )
}

/// The text of a task's `<Data>` element: unescaped, or as it is inside the CDATA section
/// Task Scheduler wraps it in when it hands the XML back.
fn data_of(xml: &str) -> Option<String> {
    let start = xml.find("<Data>")? + "<Data>".len();
    let end = xml[start..].find("</Data>")? + start;
    let text = xml[start..end].trim();
    Some(
        match text
            .strip_prefix("<![CDATA[")
            .and_then(|t| t.strip_suffix("]]>"))
        {
            Some(raw) => raw.to_owned(),
            None => unescape(text),
        },
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The trigger for a schedule. Daily and weekly ones fire at the local wall-clock time (Task
/// Scheduler follows daylight saving). `every` repeats from `now`, so it's the interval
/// between runs rather than after the last one, as the timer does.
fn trigger_xml(schedule: &Schedule, now: NaiveDateTime) -> String {
    // Today at that time: a start in the past is fine, as it only anchors the pattern.
    let at = |time: NaiveTime| {
        now.date()
            .and_time(time)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    };
    match schedule {
        Schedule::Daily { at: time } => format!(
            "<CalendarTrigger><StartBoundary>{}</StartBoundary><Enabled>true</Enabled>\
             <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger>",
            at(*time)
        ),
        Schedule::Weekly { days, at: time } => {
            let days: String = days
                .iter()
                .map(|d| format!("<{}/>", weekday_name(*d)))
                .collect();
            format!(
                "<CalendarTrigger><StartBoundary>{}</StartBoundary><Enabled>true</Enabled>\
                 <ScheduleByWeek><DaysOfWeek>{days}</DaysOfWeek><WeeksInterval>1</WeeksInterval>\
                 </ScheduleByWeek></CalendarTrigger>",
                at(*time)
            )
        }
        Schedule::Every { interval } => {
            let start = now + *interval;
            format!(
                "<TimeTrigger><StartBoundary>{}</StartBoundary><Enabled>true</Enabled>\
                 <Repetition><Interval>{}</Interval><StopAtDurationEnd>false</StopAtDurationEnd>\
                 </Repetition></TimeTrigger>",
                start.format("%Y-%m-%dT%H:%M:%S"),
                iso_duration(*interval)
            )
        }
    }
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// `PT6H`, `PT1H30M`, `PT90M`… Task Scheduler takes hours and minutes.
fn iso_duration(interval: TimeDelta) -> String {
    let minutes = interval.num_minutes().max(1);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("PT{m}M"),
        (h, 0) => format!("PT{h}H"),
        (h, m) => format!("PT{h}H{m}M"),
    }
}

/// The task definition: run `exe --run <job>` on `schedule`, as the user while they're logged
/// on, once at a time, catching up a missed start, on battery too, with no time limit.
pub fn task_xml(
    job: &str,
    schedule: &Schedule,
    exe: &Path,
    enabled: bool,
    now: NaiveDateTime,
) -> String {
    let description = tr!(
        "tasks-description",
        job = job,
        schedule = schedule.to_string()
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Source>Replica</Source><Description>{description}</Description></RegistrationInfo>
  <Triggers>{trigger}</Triggers>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <Enabled>{enabled}</Enabled>
  </Settings>
  <Actions Context="Author"><Exec><Command>"{exe}"</Command><Arguments>--run "{job}"</Arguments></Exec></Actions>
  <Data>{data}</Data>
</Task>
"#,
        description = escape(&description),
        trigger = trigger_xml(schedule, now),
        enabled = enabled,
        exe = escape(&exe.display().to_string()),
        job = escape(job),
        data = escape(&data_text(job, schedule, exe)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn daily_weekly_and_every_become_triggers() {
        let now = at("2026-10-21 18:00");
        let daily: Schedule = "daily 02:30".parse().unwrap();
        let xml = trigger_xml(&daily, now);
        assert!(
            xml.contains("<StartBoundary>2026-10-21T02:30:00</StartBoundary>"),
            "{xml}"
        );
        assert!(xml.contains("<DaysInterval>1</DaysInterval>"), "{xml}");

        let later_today: Schedule = "daily 20:00".parse().unwrap();
        assert!(trigger_xml(&later_today, now).contains("2026-10-21T20:00:00"));

        let weekly: Schedule = "weekly mon,thu 09:15".parse().unwrap();
        let xml = trigger_xml(&weekly, now);
        assert!(
            xml.contains("<DaysOfWeek><Monday/><Thursday/></DaysOfWeek>"),
            "{xml}"
        );

        let every: Schedule = "every 6h".parse().unwrap();
        let xml = trigger_xml(&every, now);
        assert!(xml.contains("<Interval>PT6H</Interval>"), "{xml}");
        assert!(
            xml.contains("<StartBoundary>2026-10-22T00:00:00</StartBoundary>"),
            "{xml}"
        );
    }

    #[test]
    fn intervals_are_iso_durations() {
        assert_eq!(iso_duration(TimeDelta::minutes(90)), "PT1H30M");
        assert_eq!(iso_duration(TimeDelta::minutes(45)), "PT45M");
        assert_eq!(iso_duration(TimeDelta::hours(24)), "PT24H");
    }

    #[test]
    fn the_definition_runs_the_job_and_escapes_text() {
        let schedule: Schedule = "daily 02:30".parse().unwrap();
        let exe = Path::new(r"C:\Program & Co\Replica\replica-tray.exe");
        let xml = task_xml("hive", &schedule, exe, false, at("2026-10-21 18:00"));
        assert!(
            xml.contains(r#"<Command>"C:\Program &amp; Co\Replica\replica-tray.exe"</Command>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<Arguments>--run "hive"</Arguments>"#),
            "{xml}"
        );
        assert!(xml.contains("<Enabled>false</Enabled>"), "{xml}");
        assert!(
            xml.contains("<LogonType>InteractiveToken</LogonType>"),
            "{xml}"
        );
        assert!(!xml.contains("<UserId>"), "runs as whoever registers it");
        assert_eq!(
            data_of(&xml).unwrap(),
            r"replica;job=hive;schedule=daily 02:30;exe=C:\Program & Co\Replica\replica-tray.exe"
        );
    }

    #[test]
    fn data_reads_back_escaped_or_as_cdata() {
        assert_eq!(
            data_of("<Task><Data>a &amp; b</Data></Task>").as_deref(),
            Some("a & b")
        );
        assert_eq!(
            data_of("<Data>\n  <![CDATA[a & b;exe=C:\\x]]>\n</Data>").as_deref(),
            Some("a & b;exe=C:\\x")
        );
        assert_eq!(data_of("<Task/>"), None);
    }

    #[test]
    fn ole_dates_convert() {
        // 2026-10-21 18:00 is 46316.75 days after 1899-12-30.
        assert_eq!(from_ole_date(46316.75), Some(at("2026-10-21 18:00")));
        assert_eq!(from_ole_date(2.5), Some(at("1900-01-01 12:00")));
    }
}
