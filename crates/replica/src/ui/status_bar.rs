//! The window's status bar: a thin strip along the bottom with what's happening now (Ready,
//! or the running job and its progress), the next scheduled run, a count of the jobs whose
//! last run went wrong (a click shows them in History), what runs the schedules, and whether
//! this is the portable copy.
//!
//! It shows only what the window already has: the tray's [`Status`], the settings and
//! [`super::backend::Places`]. It reads them as the window draws, so it follows a run as the
//! tray reports it, at no cost of its own.

use chrono::{DateTime, Utc};
use eframe::egui::{self, Button, RichText, Sense, Stroke, Ui, vec2};
use replica_core::i18n::tr;
use replica_core::job::Job;
use replica_core::run::Outcome;
use replica_core::settings::{AppSettings, ScheduleMode};
use replica_core::tr;

use super::backend::Status;
use super::jobs::{NextRun, next_run};
use super::theme::{self, Palette};

/// The strip's height, in points.
pub const HEIGHT: f32 = 24.0;
/// Its text: smaller than the pages'.
const TEXT_SIZE: f32 = 12.0;
/// The running job's progress bar.
const PROGRESS_WIDTH: f32 = 96.0;
const PROGRESS_HEIGHT: f32 = 6.0;

/// What the status bar says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub activity: Activity,
    /// The next run on a schedule: "Next: Documents, tomorrow 02:30".
    pub next: Option<String>,
    pub problems: Problems,
    pub scheduler: ScheduleMode,
    pub portable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Ready,
    Running {
        job: String,
        percent: u32,
        /// What it's waiting for, while it waits its turn for a repository.
        waiting: Option<String>,
        queued: usize,
    },
}

/// How many of the jobs last ended badly, by how, and how many are overdue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Problems {
    pub failed: usize,
    pub partly: usize,
    pub skipped: usize,
    pub overdue: usize,
    /// The job, when only one has a problem: a click shows its runs alone.
    pub only: Option<String>,
}

impl Problems {
    pub fn any(&self) -> bool {
        self.failed + self.partly + self.skipped + self.overdue > 0
    }
}

impl Facts {
    /// What the status bar says about `jobs`, as the tray's `status` has them `now`.
    pub fn of(
        jobs: &[Job],
        app: &AppSettings,
        status: &Status,
        portable: bool,
        now: DateTime<Utc>,
    ) -> Facts {
        let activity = match &status.running {
            Some((job, percent)) => Activity::Running {
                job: job.clone(),
                percent: (*percent).min(100),
                waiting: status.waiting.clone(),
                queued: status.queued.len(),
            },
            None => Activity::Ready,
        };
        Facts {
            activity,
            next: next_text(jobs, app, status, now),
            problems: problems(jobs, status),
            scheduler: app.schedule_mode,
            portable,
        }
    }
}

/// The soonest scheduled run, or that schedules are paused; none when nothing is scheduled.
fn next_text(
    jobs: &[Job],
    app: &AppSettings,
    status: &Status,
    now: DateTime<Utc>,
) -> Option<String> {
    // A job running or queued now isn't next: it's already on its way.
    let scheduled = jobs.iter().filter(|j| {
        j.enabled
            && j.schedule.is_some()
            && status.progress(&j.name).is_none()
            && !status.is_queued(&j.name)
    });
    if app.pause_schedules {
        return scheduled.count().gt(&0).then(|| tr("status-paused"));
    }
    let soonest = scheduled
        .filter_map(|job| match next_run(job, app, status, now) {
            NextRun::Due => Some((now, job, true)),
            NextRun::At(at) => Some((at, job, false)),
            _ => None,
        })
        .min_by_key(|(at, _, _)| *at)?;
    let (at, job, due) = soonest;
    Some(if due {
        tr!("status-next-due", job = job.name.as_str())
    } else {
        tr!(
            "status-next",
            job = job.name.as_str(),
            when = NextRun::At(at).text(now)
        )
    })
}

fn problems(jobs: &[Job], status: &Status) -> Problems {
    let mut problems = Problems::default();
    let mut with: Vec<&str> = Vec::new();
    for job in jobs {
        let mut bad = true;
        match status.last(&job.name).map(|l| l.outcome) {
            Some(Outcome::Failed) => problems.failed += 1,
            Some(Outcome::Partly) => problems.partly += 1,
            Some(Outcome::Skipped) => problems.skipped += 1,
            _ => bad = false,
        }
        if status.overdue(&job.name).is_some() {
            problems.overdue += 1;
            bad = true;
        }
        if bad {
            with.push(&job.name);
        }
    }
    if let [only] = with[..] {
        problems.only = Some(only.to_owned());
    }
    problems
}

/// The status bar, in the panel given it. True when its problems were clicked.
///
/// Its segments share the width it has: the problems first (a way to History, so never
/// dropped), then the next run, what runs the schedules and "Portable"; what's happening now
/// takes the rest. A name that doesn't fit is cut short with "…" (its whole text in its hover
/// text, and for a screen reader); a segment with no room even for that is left out.
pub fn show(ui: &mut Ui, facts: &Facts) -> bool {
    let p = theme::of(ui);
    let mut clicked = false;
    ui.horizontal_centered(|ui| {
        text_size(ui);
        let plan = Plan::new(ui, facts, ui.available_width());
        let height = ui.available_height();
        ui.allocate_ui_with_layout(
            vec2(plan.left, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| activity(ui, p, &facts.activity, plan.left),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (i, (segment, width)) in plan.right.iter().enumerate() {
                if i > 0 {
                    divider(ui, p);
                }
                match segment {
                    Segment::Portable => text(
                        ui,
                        RichText::new(tr("status-portable")).color(p.text_secondary),
                        *width,
                    ),
                    Segment::Scheduler(name) => {
                        text(ui, RichText::new(name).color(p.text_secondary), *width)
                    }
                    Segment::Problems => {
                        clicked = problems_button(ui, p, &facts.problems).clicked();
                    }
                    Segment::Next(next) => {
                        text(ui, RichText::new(next).color(p.text_secondary), *width)
                    }
                }
            }
        });
    });
    clicked
}

/// The narrowest a cut-short name is shown; with less room its segment is left out.
const LEAST_TEXT: f32 = 72.0;
/// The space between a segment and its divider, each side.
const GAP: f32 = 8.0;
/// What a segment costs besides its text: its divider and the gaps round it.
const SEGMENT_COST: f32 = 1.0 + 2.0 * GAP;

/// A segment on the right.
#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Portable,
    Scheduler(String),
    Problems,
    Next(String),
}

/// How the bar's width is shared: the right's segments as they're shown from the right edge,
/// each with its width, and what's left for the activity on the left.
struct Plan {
    right: Vec<(Segment, f32)>,
    left: f32,
}

impl Plan {
    fn new(ui: &Ui, facts: &Facts, total: f32) -> Plan {
        let least_left = match &facts.activity {
            Activity::Ready => text_width(ui, &tr("status-ready")),
            Activity::Running { .. } => LEAST_TEXT,
        };
        let budget = (total - least_left - GAP).max(0.0);
        let scheduler = tr(match facts.scheduler {
            ScheduleMode::Timer => "status-scheduler-timer",
            ScheduleMode::TaskScheduler => "status-scheduler-tasks",
        });
        // By priority: whichever has room, in this order, is shown.
        let mut wanted: Vec<(Segment, f32, bool)> = Vec::new();
        if facts.problems.any() {
            wanted.push((
                Segment::Problems,
                problems_width(ui, &facts.problems),
                false,
            ));
        }
        if let Some(next) = &facts.next {
            wanted.push((Segment::Next(next.clone()), text_width(ui, next), true));
        }
        wanted.push((
            Segment::Scheduler(scheduler.clone()),
            text_width(ui, &scheduler),
            false,
        ));
        if facts.portable {
            wanted.push((
                Segment::Portable,
                text_width(ui, &tr("status-portable")),
                false,
            ));
        }
        let mut used = 0.0;
        let mut chosen: Vec<(Segment, f32)> = Vec::new();
        for (segment, width, cut) in wanted {
            let room = budget - used - SEGMENT_COST;
            let width = if width <= room || segment == Segment::Problems {
                width
            } else if cut && room >= LEAST_TEXT {
                room
            } else {
                continue;
            };
            used += width + SEGMENT_COST;
            chosen.push((segment, width));
        }
        // Shown from the right edge: Portable, what runs the schedules, problems, next.
        let rank = |s: &Segment| match s {
            Segment::Portable => 0,
            Segment::Scheduler(_) => 1,
            Segment::Problems => 2,
            Segment::Next(_) => 3,
        };
        chosen.sort_by_key(|(s, _)| rank(s));
        Plan {
            right: chosen,
            left: (total - used - GAP).max(0.0),
        }
    }
}

/// Ready, or the running job with its progress bar, what it waits for and how many are queued
/// after it: as much of that as fits in `width`, the job's name cut short first.
fn activity(ui: &mut Ui, p: &Palette, activity: &Activity, width: f32) {
    let Activity::Running {
        job,
        percent,
        waiting,
        queued,
    } = activity
    else {
        text(
            ui,
            RichText::new(tr("status-ready")).color(p.text_secondary),
            width,
        );
        return;
    };
    let running = tr!("status-running", job = job.as_str(), percent = *percent);
    let shown = text_width(ui, &running).min(width);
    text(ui, RichText::new(running).color(p.text), shown);
    let mut room = width - shown - GAP;
    if room >= PROGRESS_WIDTH {
        let (rect, _) =
            ui.allocate_exact_size(vec2(PROGRESS_WIDTH, PROGRESS_HEIGHT), Sense::hover());
        let painter = ui.painter();
        let radius = egui::CornerRadius::same(2);
        painter.rect_filled(rect, radius, p.neutral_fill);
        let mut done = rect;
        done.set_width(rect.width() * (*percent as f32 / 100.0));
        painter.rect_filled(done, radius, p.accent);
        room -= PROGRESS_WIDTH + GAP;
    }
    if let Some(waiting) = waiting {
        let wanted = text_width(ui, waiting);
        let fits = (room - SEGMENT_COST).min(wanted);
        if fits >= LEAST_TEXT.min(wanted) {
            divider(ui, p);
            text(ui, RichText::new(waiting).color(p.warning), fits);
            room -= fits + SEGMENT_COST;
        }
    }
    if *queued > 0 {
        let queue = tr!("status-queued", count = *queued);
        if text_width(ui, &queue) + SEGMENT_COST <= room {
            divider(ui, p);
            ui.label(RichText::new(queue).color(p.text_secondary));
        }
    }
}

/// `text` in at most `width`, cut short with "…" if it needs more.
fn text(ui: &mut Ui, text: RichText, width: f32) {
    let height = ui.available_height();
    ui.allocate_ui_with_layout(
        // A point to spare, so a text that fits isn't cut by rounding.
        vec2(width + 1.0, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| ui.add(egui::Label::new(text).truncate()),
    );
}

/// How wide `text` is in the bar's font.
fn text_width(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(TEXT_SIZE),
            egui::Color32::PLACEHOLDER,
        )
        .size()
        .x
}

/// The bar's text size, for it and what's in it.
fn text_size(ui: &mut Ui) {
    for text in [egui::TextStyle::Body, egui::TextStyle::Button] {
        ui.style_mut()
            .text_styles
            .insert(text, egui::FontId::proportional(TEXT_SIZE));
    }
    ui.spacing_mut().item_spacing.x = GAP;
}

/// The problems' parts, each in its status's colour: "1 failed,", "1 skipped".
fn problem_parts(p: &Palette, problems: &Problems) -> Vec<(String, egui::Color32)> {
    let mut parts: Vec<(String, egui::Color32)> = Vec::new();
    if problems.failed > 0 {
        parts.push((tr!("status-failed", count = problems.failed), p.failed));
    }
    if problems.partly > 0 {
        parts.push((tr!("status-partly", count = problems.partly), p.warning));
    }
    if problems.skipped > 0 {
        parts.push((tr!("status-skipped", count = problems.skipped), p.warning));
    }
    if problems.overdue > 0 {
        parts.push((tr!("status-overdue", count = problems.overdue), p.warning));
    }
    let last = parts.len().saturating_sub(1);
    for (text, _) in &mut parts[..last] {
        text.push(',');
    }
    parts
}

/// The problems' button's padding, each side.
const PROBLEMS_PADDING: f32 = 4.0;

/// How wide the problems' button is.
fn problems_width(ui: &Ui, problems: &Problems) -> f32 {
    let parts = problem_parts(theme::of(ui), problems);
    let gaps = ui.spacing().icon_spacing * parts.len().saturating_sub(1) as f32;
    let text: f32 = parts.iter().map(|(t, _)| text_width(ui, t)).sum();
    text + gaps + 2.0 * PROBLEMS_PADDING
}

/// "1 failed, 1 skipped", each count in its status's colour: a button to History.
fn problems_button(ui: &mut Ui, p: &Palette, problems: &Problems) -> egui::Response {
    let atoms: egui::Atoms<'_> = problem_parts(p, problems)
        .into_iter()
        .map(|(text, colour)| egui::Atom::from(RichText::new(text).color(colour)))
        .collect();
    ui.spacing_mut().button_padding = vec2(PROBLEMS_PADDING, 1.0);
    let response = ui.add(Button::new(atoms).frame(false));
    response.on_hover_text(tr("status-problems-hint"))
}

/// A thin upright line between segments.
fn divider(ui: &mut Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 12.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, separator(p)),
    );
}

/// The dividers' colour: the hairlines', a little stronger so they show on the bar.
pub fn separator(p: &Palette) -> egui::Color32 {
    p.text_secondary.gamma_multiply(0.35)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::backend::{LastRun, Overdue};

    fn job(name: &str, schedule: Option<&str>) -> Job {
        Job {
            name: name.into(),
            schedule: schedule.map(|s| s.parse().unwrap()),
            ..Job::default()
        }
    }

    fn last(outcome: Outcome, at: DateTime<Utc>) -> LastRun {
        LastRun {
            at,
            outcome,
            reason: None,
        }
    }

    #[test]
    fn ready_with_nothing_running_and_the_running_job_with_its_progress() {
        let app = AppSettings::default();
        let now = Utc::now();
        let mut status = Status::default();
        let facts = Facts::of(&[], &app, &status, false, now);
        assert_eq!(facts.activity, Activity::Ready);
        assert_eq!(facts.next, None);
        assert!(!facts.problems.any());

        status.running = Some(("Documents".into(), 42));
        status.queued = vec!["Photos".into()];
        let facts = Facts::of(&[], &app, &status, true, now);
        assert_eq!(
            facts.activity,
            Activity::Running {
                job: "Documents".into(),
                percent: 42,
                waiting: None,
                queued: 1
            }
        );
        assert!(facts.portable);
    }

    #[test]
    fn the_next_run_is_the_soonest_of_the_jobs_that_are_on() {
        let app = AppSettings::default();
        let now = Utc::now();
        let status = Status {
            since: Some(now),
            ..Status::default()
        };
        let mut off = job("Off", Some("every 1h"));
        off.enabled = false;
        let jobs = [
            job("Daily", Some("every 24h")),
            job("Often", Some("every 2h")),
            off,
            job("Manual", None),
        ];
        let next = Facts::of(&jobs, &app, &status, false, now).next.unwrap();
        assert!(next.contains("Often"), "{next}");

        let paused = AppSettings {
            pause_schedules: true,
            ..app
        };
        assert_eq!(
            Facts::of(&jobs, &paused, &status, false, now).next,
            Some(tr("status-paused"))
        );
        assert_eq!(
            Facts::of(&[job("Manual", None)], &paused, &status, false, now).next,
            None
        );
    }

    #[test]
    fn problems_are_counted_by_how_the_last_run_ended() {
        let app = AppSettings::default();
        let now = Utc::now();
        let mut status = Status::default();
        status.last.insert("a".into(), last(Outcome::Failed, now));
        status.last.insert("b".into(), last(Outcome::Skipped, now));
        status.last.insert("c".into(), last(Outcome::Ok, now));
        let jobs = [job("A", None), job("B", None), job("C", None)];
        let facts = Facts::of(&jobs, &app, &status, false, now);
        assert_eq!(
            facts.problems,
            Problems {
                failed: 1,
                skipped: 1,
                ..Problems::default()
            }
        );

        // One job alone: a click shows its runs.
        status.last.remove("b");
        status.overdue.insert(
            "a".into(),
            Overdue {
                due: now,
                last_success: None,
            },
        );
        let facts = Facts::of(&jobs, &app, &status, false, now);
        assert_eq!(facts.problems.overdue, 1);
        assert_eq!(facts.problems.only.as_deref(), Some("A"));
    }
}
