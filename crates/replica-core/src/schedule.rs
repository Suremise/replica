//! Schedules: a small text syntax, the next run time and "overdue".
//!
//! # Syntax
//!
//! Case-insensitive, one of:
//!
//! | Form | Example | Meaning |
//! |---|---|---|
//! | `daily HH:MM` | `daily 02:30` | every day at that local time |
//! | `weekly DAYS HH:MM` | `weekly mon,thu 21:00` | on those days (`mon` … `sun`, comma-separated) at that local time |
//! | `every Nh` or `every N hours` | `every 6h` | N hours after the last run |
//! | `every Nm` or `every N minutes` | `every 90m` | N minutes after the last run |
//!
//! Times are 24-hour local wall-clock times. An interval is counted from the last run, so a job
//! that has never run is due at once; that is the caller's call, as [`Schedule::next_after`]
//! needs a time to count from.
//!
//! Across a daylight-saving change, a daily or weekly time that doesn't exist that day (the clock
//! jumps over it) runs at the first valid minute after it, and one that happens twice runs at
//! the first.

use std::fmt;
use std::str::FromStr;

use chrono::{
    DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeDelta, TimeZone, Timelike, Weekday,
};
use serde::{Deserialize, Serialize};

use crate::i18n::tr;
use crate::tr;

/// How late a run may be, after it was due, before it counts as overdue.
pub const DEFAULT_GRACE: TimeDelta = TimeDelta::hours(1);

/// When a job runs. Stored in settings as its text form (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Schedule {
    /// Every day at a local time.
    Daily { at: NaiveTime },
    /// On some days of the week at a local time. `days` is sorted Monday first, without repeats.
    Weekly { days: Vec<Weekday>, at: NaiveTime },
    /// A fixed interval after the last run, of at least a minute.
    Every { interval: TimeDelta },
}

/// A schedule's text that doesn't parse. Its text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleError {
    pub text: String,
    pub reason: ScheduleProblem,
}

/// Which part of a schedule's text is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleProblem {
    Time,
    Days,
    Interval,
    Form,
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            ScheduleProblem::Time => tr("error-schedule-time"),
            ScheduleProblem::Days => tr("error-schedule-days"),
            ScheduleProblem::Interval => tr("error-schedule-interval"),
            ScheduleProblem::Form => tr("error-schedule-form"),
        };
        f.write_str(&tr!(
            "error-schedule-invalid",
            text = self.text.as_str(),
            reason = reason
        ))
    }
}

impl std::error::Error for ScheduleError {}

impl Schedule {
    /// The first time strictly after `after` at which the job is due.
    ///
    /// For [`Schedule::Every`], `after` is the last run. For daily and weekly schedules, it's
    /// usually now or the last run.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> DateTime<Tz> {
        match self {
            Schedule::Every { interval } => after.clone() + *interval,
            Schedule::Daily { at } => next_wall_time(after, *at, |_| true),
            Schedule::Weekly { days, at } => {
                next_wall_time(after, *at, |d| days.contains(&d.weekday()))
            }
        }
    }

    /// Whether a job with this schedule is overdue: its last success is older than the time it
    /// was next due plus `grace`.
    pub fn is_overdue<Tz: TimeZone>(
        &self,
        last_success: &DateTime<Tz>,
        now: &DateTime<Tz>,
        grace: TimeDelta,
    ) -> bool {
        *now > self.next_after(last_success) + grace
    }
}

/// The first `at` on a day that `day_ok` accepts, strictly after `after`.
fn next_wall_time<Tz: TimeZone>(
    after: &DateTime<Tz>,
    at: NaiveTime,
    day_ok: impl Fn(NaiveDate) -> bool,
) -> DateTime<Tz> {
    let tz = after.timezone();
    let start = after.naive_local().date();
    // Eight days covers every weekday, with today's time possibly already gone.
    for offset in 0..=8 {
        let day = start + Days::new(offset);
        if !day_ok(day) {
            continue;
        }
        if let Some(candidate) = resolve_local(&tz, day, at)
            && candidate > *after
        {
            return candidate;
        }
    }
    unreachable!("a weekly schedule has at least one day, so one falls within eight days")
}

/// A local wall-clock time as a real instant: the earlier one if it happens twice, or the first
/// minute after a gap if it doesn't exist.
fn resolve_local<Tz: TimeZone>(tz: &Tz, day: NaiveDate, at: NaiveTime) -> Option<DateTime<Tz>> {
    let mut local = day.and_time(at);
    // Daylight-saving gaps are at most a few hours; step a minute at a time past one.
    for _ in 0..=(4 * 60) {
        if let Some(t) = tz.from_local_datetime(&local).earliest() {
            return Some(t);
        }
        local += TimeDelta::minutes(1);
    }
    None
}

impl FromStr for Schedule {
    type Err = ScheduleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let err = |reason| ScheduleError {
            text: text.to_string(),
            reason,
        };
        let lower = text.trim().to_ascii_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        match words.as_slice() {
            ["daily", at] => Ok(Schedule::Daily {
                at: parse_time(at).ok_or(err(ScheduleProblem::Time))?,
            }),
            ["weekly", days, at] => {
                let at = parse_time(at).ok_or(err(ScheduleProblem::Time))?;
                let mut parsed = Vec::new();
                for day in days.split(',') {
                    parsed.push(parse_day(day).ok_or(err(ScheduleProblem::Days))?);
                }
                parsed.sort_by_key(|d| d.num_days_from_monday());
                parsed.dedup();
                Ok(Schedule::Weekly { days: parsed, at })
            }
            ["every", amount] => parse_interval(amount, None).ok_or(err(ScheduleProblem::Interval)),
            ["every", amount, unit] => {
                parse_interval(amount, Some(unit)).ok_or(err(ScheduleProblem::Interval))
            }
            _ => Err(err(ScheduleProblem::Form)),
        }
    }
}

fn parse_time(text: &str) -> Option<NaiveTime> {
    let (h, m) = text.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn parse_day(text: &str) -> Option<Weekday> {
    Some(match text {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_interval(amount: &str, unit: Option<&str>) -> Option<Schedule> {
    let (digits, unit) = match unit {
        Some(unit) => (amount, unit),
        None => amount.split_at(amount.find(|c: char| !c.is_ascii_digit())?),
    };
    let n: i64 = digits.parse().ok().filter(|n| *n >= 1)?;
    let interval = match unit {
        "h" | "hour" | "hours" => TimeDelta::try_hours(n)?,
        "m" | "minute" | "minutes" => TimeDelta::try_minutes(n)?,
        _ => return None,
    };
    Some(Schedule::Every { interval })
}

fn day_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let time = |t: &NaiveTime| format!("{:02}:{:02}", t.hour(), t.minute());
        match self {
            Schedule::Daily { at } => write!(f, "daily {}", time(at)),
            Schedule::Weekly { days, at } => {
                let days: Vec<_> = days.iter().map(|d| day_name(*d)).collect();
                write!(f, "weekly {} {}", days.join(","), time(at))
            }
            Schedule::Every { interval } => {
                let minutes = interval.num_minutes();
                if minutes % 60 == 0 {
                    write!(f, "every {}h", minutes / 60)
                } else {
                    write!(f, "every {minutes}m")
                }
            }
        }
    }
}

impl TryFrom<String> for Schedule {
    type Error = ScheduleError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<Schedule> for String {
    fn from(schedule: Schedule) -> Self {
        schedule.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_and_prints_each_form() {
        for (text, canonical) in [
            ("daily 02:30", "daily 02:30"),
            ("Daily 2:30", "daily 02:30"),
            ("weekly thu,mon,thu 21:00", "weekly mon,thu 21:00"),
            ("every 6h", "every 6h"),
            ("every 6 hours", "every 6h"),
            ("every 90m", "every 90m"),
            ("every 120 minutes", "every 2h"),
        ] {
            let s: Schedule = text.parse().unwrap();
            assert_eq!(s.to_string(), canonical, "{text}");
            assert_eq!(canonical.parse::<Schedule>().unwrap(), s);
        }
    }

    #[test]
    fn rejects_bad_text() {
        for text in [
            "",
            "daily",
            "daily 25:00",
            "daily 2:3",
            "weekly 21:00",
            "weekly mon,xyz 21:00",
            "every 0h",
            "every 5d",
            "every h",
            "hourly",
        ] {
            assert!(text.parse::<Schedule>().is_err(), "{text}");
        }
    }

    #[test]
    fn daily_next_is_today_or_tomorrow() {
        let s: Schedule = "daily 02:30".parse().unwrap();
        assert_eq!(
            s.next_after(&utc("2026-10-21T01:00:00Z")),
            utc("2026-10-21T02:30:00Z")
        );
        assert_eq!(
            s.next_after(&utc("2026-10-21T02:30:00Z")),
            utc("2026-10-22T02:30:00Z")
        );
        assert_eq!(
            s.next_after(&utc("2026-10-21T23:00:00Z")),
            utc("2026-10-22T02:30:00Z")
        );
    }

    #[test]
    fn weekly_next_skips_to_a_listed_day() {
        // 21 Oct 2026 is a Wednesday.
        let s: Schedule = "weekly mon,thu 21:00".parse().unwrap();
        assert_eq!(
            s.next_after(&utc("2026-10-21T12:00:00Z")),
            utc("2026-10-22T21:00:00Z")
        );
        assert_eq!(
            s.next_after(&utc("2026-10-22T21:00:00Z")),
            utc("2026-10-26T21:00:00Z")
        );
        let sunday: Schedule = "weekly sun 08:00".parse().unwrap();
        assert_eq!(
            sunday.next_after(&utc("2026-10-25T09:00:00Z")),
            utc("2026-11-01T08:00:00Z")
        );
    }

    #[test]
    fn every_counts_from_the_last_run() {
        let s: Schedule = "every 6h".parse().unwrap();
        assert_eq!(
            s.next_after(&utc("2026-10-21T22:00:00Z")),
            utc("2026-10-22T04:00:00Z")
        );
    }

    #[test]
    fn local_times_use_the_given_zone() {
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let s: Schedule = "daily 02:30".parse().unwrap();
        let after = tz.with_ymd_and_hms(2026, 10, 21, 3, 0, 0).unwrap();
        assert_eq!(
            s.next_after(&after),
            tz.with_ymd_and_hms(2026, 10, 22, 2, 30, 0).unwrap()
        );
    }

    /// A zone like Europe/London in 2026: GMT, then BST (UTC+1) from 01:00 UTC on 29 March (local
    /// 01:00–02:00 doesn't exist) until 01:00 UTC on 25 October (local 01:00–02:00 happens twice).
    #[derive(Clone, Copy, Debug)]
    struct London;

    impl London {
        const SPRING: (i32, u32, u32) = (2026, 3, 29);
        const AUTUMN: (i32, u32, u32) = (2026, 10, 25);

        fn at(day: (i32, u32, u32), h: u32) -> chrono::NaiveDateTime {
            NaiveDate::from_ymd_opt(day.0, day.1, day.2)
                .unwrap()
                .and_hms_opt(h, 0, 0)
                .unwrap()
        }
    }

    impl TimeZone for London {
        type Offset = FixedOffset;

        fn from_offset(_: &FixedOffset) -> Self {
            London
        }

        fn offset_from_local_date(&self, local: &NaiveDate) -> chrono::LocalResult<FixedOffset> {
            self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> chrono::LocalResult<FixedOffset> {
            use chrono::LocalResult::{Ambiguous, None, Single};
            let (gmt, bst) = (
                FixedOffset::east_opt(0).unwrap(),
                FixedOffset::east_opt(3600).unwrap(),
            );
            let (spring, autumn) = (London::at(London::SPRING, 1), London::at(London::AUTUMN, 1));
            if *local < spring {
                Single(gmt)
            } else if *local < spring + TimeDelta::hours(1) {
                None
            } else if *local < autumn {
                Single(bst)
            } else if *local < autumn + TimeDelta::hours(1) {
                Ambiguous(bst, gmt)
            } else {
                Single(gmt)
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
            self.offset_from_utc_datetime(&utc.and_hms_opt(12, 0, 0).unwrap())
        }

        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> FixedOffset {
            let summer =
                *utc >= London::at(London::SPRING, 1) && *utc < London::at(London::AUTUMN, 1);
            FixedOffset::east_opt(if summer { 3600 } else { 0 }).unwrap()
        }
    }

    #[test]
    fn daylight_saving_gaps_and_repeats() {
        let s: Schedule = "daily 01:30".parse().unwrap();
        // Spring: 01:30 doesn't exist on 29 March, so it runs at 02:00 BST (01:00 UTC).
        let before_spring = London.with_ymd_and_hms(2026, 3, 28, 12, 0, 0).unwrap();
        let next = s.next_after(&before_spring);
        assert_eq!(next.with_timezone(&Utc), utc("2026-03-29T01:00:00Z"));
        // And the day after, back to 01:30 BST.
        assert_eq!(
            s.next_after(&next).with_timezone(&Utc),
            utc("2026-03-30T00:30:00Z")
        );
        // Autumn: 01:30 happens twice on 25 October; it runs once, at the first (BST, 00:30 UTC).
        let before_autumn = London.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap();
        let first = s.next_after(&before_autumn);
        assert_eq!(first.with_timezone(&Utc), utc("2026-10-25T00:30:00Z"));
        assert_eq!(
            s.next_after(&first).with_timezone(&Utc),
            utc("2026-10-26T01:30:00Z")
        );
    }

    #[test]
    fn overdue_after_the_due_time_plus_grace() {
        let s: Schedule = "daily 02:00".parse().unwrap();
        let last = utc("2026-10-20T02:05:00Z");
        // Next due 21 Oct 02:00; overdue after 03:00 with an hour's grace.
        assert!(!s.is_overdue(&last, &utc("2026-10-21T02:59:00Z"), DEFAULT_GRACE));
        assert!(s.is_overdue(&last, &utc("2026-10-21T03:01:00Z"), DEFAULT_GRACE));
        let every: Schedule = "every 6h".parse().unwrap();
        assert!(!every.is_overdue(&last, &utc("2026-10-20T09:00:00Z"), DEFAULT_GRACE));
        assert!(every.is_overdue(&last, &utc("2026-10-20T09:06:00Z"), DEFAULT_GRACE));
    }

    #[test]
    fn serialises_as_text() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap {
            schedule: Schedule,
        }
        let w = Wrap {
            schedule: "weekly fri 18:15".parse().unwrap(),
        };
        let text = toml::to_string(&w).unwrap();
        assert_eq!(text.trim(), "schedule = \"weekly fri 18:15\"");
        assert_eq!(toml::from_str::<Wrap>(&text).unwrap(), w);
        assert!(toml::from_str::<Wrap>("schedule = \"sometimes\"").is_err());
    }
}
