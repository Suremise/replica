//! The History tab: every run, newest first, with each target's result (and why it was
//! skipped or failed), its start, duration, files, size and error, and its log; and the test
//! restores, kept apart in the history but shown here with the runs. Filter by job and result
//! (in the history itself, so an old run is found), and "Show older" pages back through the
//! rest.
//!
//! It's a sortable table under a toolbar (job, result, a period and a search, the
//! last two over what's loaded), with a summary line above it and the chosen entry's details
//! below.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, RichText, Ui};
use replica_core::history::{HistoryFilter, RunRecord, TestRestoreRecord};
use replica_core::i18n::tr;
use replica_core::run::Outcome;
use replica_core::tr;

use super::backend::Backend;
use super::icons::Icon;
use super::jobs::outcome_text;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, Sort, TableColumn, TableState, Tone, card_frame, dropdown, dropdown_value,
    empty_state, error_text, icon_button, info_bar, info_row, leaving_room, quiet_text,
    secondary_button, section_card_with_actions, size_text, status_chip, status_chip_width, table,
    toolbar_group_gap,
};
use crate::cli::duration_text;

/// How often it reads the history again while shown.
const RELOAD: Duration = Duration::from_secs(3);

/// The job dropdown's widest, closed: a longer name is cut with "…".
const JOB_WIDTH: f32 = 260.0;
/// The result and period dropdowns' widest, closed.
const FILTER_WIDTH: f32 = 220.0;
/// The search's narrowest beside the dropdowns: narrower, it goes on a row of its own.
const SEARCH_MIN_WIDTH: f32 = 160.0;

/// The Job column's narrowest: it takes what the others leave.
const JOB_LEAST: f32 = 150.0;
/// Every column's least but Started's: the table scrolls sideways only in a window too narrow
/// for them (it fits at the first-run size).
const TABLE_LEAST: f32 = JOB_LEAST + 144.0 + 166.0 + 84.0 + 64.0 + 80.0;

/// A run or a test restore.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Run(RunRecord),
    Test(TestRestoreRecord),
}

impl Entry {
    pub fn started_at(&self) -> DateTime<Utc> {
        match self {
            Entry::Run(r) => r.started_at,
            Entry::Test(t) => t.started_at,
        }
    }

    pub fn job(&self) -> &str {
        match self {
            Entry::Run(r) => &r.job,
            Entry::Test(t) => &t.job,
        }
    }

    pub fn outcome(&self) -> Outcome {
        match self {
            Entry::Run(r) => r.outcome,
            Entry::Test(t) => t.outcome,
        }
    }

    /// Its result in words: a partly-done run says how many files it skipped, when the
    /// history has the count.
    pub fn result_text(&self) -> String {
        match self {
            Entry::Run(r) => result_text(r.outcome, r.skipped()),
            Entry::Test(t) => outcome_text(t.outcome),
        }
    }

    /// Which entry it is, kept while the list is read again or sorted: a test restore's
    /// ids are apart from the runs'.
    pub fn key(&self) -> (bool, i64) {
        match self {
            Entry::Run(r) => (false, r.id),
            Entry::Test(t) => (true, t.id),
        }
    }

    pub fn duration(&self) -> Duration {
        match self {
            Entry::Run(r) => r.duration,
            Entry::Test(t) => t.duration,
        }
    }

    /// The files backed up, or a test restore's files compared.
    pub fn files(&self) -> u64 {
        match self {
            Entry::Run(r) => r.files,
            Entry::Test(t) => t.checked,
        }
    }

    /// The size backed up (a test restore has none).
    pub fn bytes(&self) -> Option<u64> {
        match self {
            Entry::Run(r) => Some(r.bytes),
            Entry::Test(_) => None,
        }
    }

    /// Its targets, by name.
    pub fn target_text(&self) -> String {
        match self {
            Entry::Run(r) => r
                .targets
                .iter()
                .map(|t| t.target.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            Entry::Test(t) => t.target.clone(),
        }
    }

    /// What the search looks through: the job, the targets, the result and the errors.
    fn search_text(&self) -> String {
        let mut text = format!(
            "{} {} {}",
            self.job(),
            self.target_text(),
            self.result_text()
        );
        match self {
            Entry::Run(r) => {
                for t in &r.targets {
                    text.extend([" ", t.reason.as_deref().unwrap_or("")]);
                }
                text.extend([" ", r.error.as_deref().unwrap_or("")]);
            }
            Entry::Test(t) => text.extend([" ", t.detail.as_deref().unwrap_or("")]),
        }
        text
    }

    /// The line it's listed under.
    pub fn summary(&self) -> String {
        let when = local_text(&self.started_at());
        match self {
            Entry::Run(r) => tr!(
                "history-run-line",
                when = when,
                job = r.job.as_str(),
                result = result_text(r.outcome, r.skipped()),
                duration = duration_text(r.duration),
                files = r.files,
                size = size_text(r.bytes)
            ),
            Entry::Test(t) => tr!(
                "history-test-line",
                when = when,
                job = t.job.as_str(),
                result = outcome_text(t.outcome),
                checked = t.checked,
                differing = t.differing
            ),
        }
    }

    /// Everything the details show, as text to paste into a message.
    pub fn details_text(&self) -> String {
        let mut lines = vec![self.summary()];
        match self {
            Entry::Run(r) => {
                for t in &r.targets {
                    let mut line = target_line(t);
                    if let Some(reason) = &t.reason {
                        line = tr!("history-target-why", line = line, reason = reason.as_str());
                    }
                    lines.push(line);
                }
                lines.extend(r.error.clone());
                lines.extend(r.log_path.as_ref().map(|p| p.display().to_string()));
            }
            Entry::Test(t) => {
                lines.push(test_target_line(t));
                lines.extend(t.detail.clone());
            }
        }
        lines.join("\n")
    }
}

/// Runs and test restores together, newest first.
pub fn merge(runs: Vec<RunRecord>, tests: Vec<TestRestoreRecord>) -> Vec<Entry> {
    let mut all: Vec<Entry> = runs
        .into_iter()
        .map(Entry::Run)
        .chain(tests.into_iter().map(Entry::Test))
        .collect();
    all.sort_by_key(|e| std::cmp::Reverse(e.started_at()));
    all
}

/// The table's order: by `sort`'s column (the table's), newest first without one.
fn sort(entries: &mut [Entry], sort: Option<Sort>) {
    let Some(sort) = sort else {
        return;
    };
    entries.sort_by(|a, b| {
        let order = match sort.column {
            0 => a.started_at().cmp(&b.started_at()),
            1 => a.job().to_lowercase().cmp(&b.job().to_lowercase()),
            3 => worse(a.outcome()).cmp(&worse(b.outcome())),
            4 => a.duration().cmp(&b.duration()),
            5 => a.files().cmp(&b.files()),
            _ => a.bytes().cmp(&b.bytes()),
        };
        if sort.ascending {
            order
        } else {
            order.reverse()
        }
    });
}

/// How bad a result is, to sort by: ok, skipped, partly, failed (as core ranks them).
fn worse(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Ok => 0,
        Outcome::Skipped => 1,
        Outcome::Partly => 2,
        Outcome::Failed => 3,
    }
}

/// "12 runs · 2 test restores · 10 OK · 1 failed · 1 skipped": what's listed, counted.
pub fn summary_line(entries: &[Entry]) -> String {
    let runs = entries
        .iter()
        .filter(|e| matches!(e, Entry::Run(_)))
        .count();
    let tests = entries.len() - runs;
    let count = |o: Outcome| entries.iter().filter(|e| e.outcome() == o).count();
    let mut parts = vec![tr!("history-summary-runs", count = runs)];
    if tests > 0 {
        parts.push(tr!("history-summary-tests", count = tests));
    }
    for (outcome, key) in [
        (Outcome::Ok, "history-summary-ok"),
        (Outcome::Partly, "history-summary-partly"),
        (Outcome::Failed, "history-summary-failed"),
        (Outcome::Skipped, "history-summary-skipped"),
    ] {
        let n = count(outcome);
        if n > 0 || outcome == Outcome::Ok {
            parts.push(tr!(key, count = n));
        }
    }
    parts.join("  ·  ")
}

/// A result's chip.
pub fn outcome_tone(outcome: Outcome) -> Tone {
    match outcome {
        Outcome::Ok => Tone::Ok,
        Outcome::Partly | Outcome::Skipped => Tone::Warning,
        Outcome::Failed => Tone::Failed,
    }
}

/// How far back the list goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Period {
    #[default]
    Any,
    Day,
    Week,
    Month,
}

impl Period {
    const ALL: [Period; 4] = [Period::Any, Period::Day, Period::Week, Period::Month];

    fn since(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let days = match self {
            Period::Any => return None,
            Period::Day => 1,
            Period::Week => 7,
            Period::Month => 30,
        };
        Some(now - chrono::TimeDelta::days(days))
    }

    fn label(self) -> String {
        tr(match self {
            Period::Any => "history-period-any",
            Period::Day => "history-period-day",
            Period::Week => "history-period-week",
            Period::Month => "history-period-month",
        })
    }
}

fn local_text(at: &DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn local_minute(at: &DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

pub struct HistoryTab {
    backend: Arc<dyn Backend>,
    entries: Result<Vec<Entry>, String>,
    /// Every job in the history, for the job filter.
    jobs: Vec<String>,
    /// How many pages of each kind are loaded ("Show older" adds one).
    pages: usize,
    /// More, older entries match the filters.
    more: bool,
    loaded_at: Option<Instant>,
    /// The filter the entries were loaded for.
    loaded_for: HistoryFilter,
    /// Only this job, if chosen.
    pub job: Option<String>,
    /// Only this result, if chosen.
    pub result: Option<Outcome>,
    /// Only entries this recent (of those loaded).
    pub period: Period,
    /// Only entries whose job, targets, result or errors have this (of those loaded).
    pub search: String,
    /// The Started column's width as the table first shows it.
    pub started_width: f32,
    table: TableState,
    /// The chosen entry, by [`Entry::key`].
    selected: Option<(bool, i64)>,
    message: Option<String>,
}

impl HistoryTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            entries: Ok(Vec::new()),
            jobs: Vec::new(),
            pages: 1,
            more: false,
            loaded_at: None,
            loaded_for: HistoryFilter::default(),
            job: None,
            result: None,
            period: Period::Any,
            search: String::new(),
            started_width: 130.0,
            table: TableState::default(),
            selected: None,
            message: None,
        }
    }

    fn filter(&self) -> HistoryFilter {
        HistoryFilter {
            job: self.job.clone(),
            outcome: self.result,
        }
    }

    /// Reads the history again: the filters are applied there, so every entry, however old,
    /// is reached by filtering or by "Show older".
    pub fn load(&mut self) {
        self.loaded_at = Some(Instant::now());
        let filter = self.filter();
        if filter != self.loaded_for {
            self.pages = 1;
        }
        self.loaded_for = filter.clone();
        let limit = self.pages * PAGE;
        let loaded = self
            .backend
            .runs(&filter, limit)
            .and_then(|runs| Ok((runs, self.backend.test_restores(&filter, limit)?)));
        match loaded {
            Ok((runs, tests)) => {
                let page = paged(runs, tests, limit);
                self.more = page.more;
                self.entries = Ok(page.entries);
            }
            Err(e) => self.entries = Err(e),
        }
        self.jobs = self.backend.history_jobs().unwrap_or_default();
    }

    /// The entries loaded, newest first.
    pub fn shown(&self) -> Vec<&Entry> {
        self.entries
            .as_ref()
            .map(|all| all.iter().collect())
            .unwrap_or_default()
    }

    /// Whether older entries match the filters and aren't loaded yet.
    pub fn has_older(&self) -> bool {
        self.more
    }

    /// Loads the next, older page.
    pub fn show_older(&mut self) {
        self.pages += 1;
        self.load();
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        if self.loaded_at.is_none_or(|t| t.elapsed() >= RELOAD) || self.filter() != self.loaded_for
        {
            self.load();
        }
        ui.ctx().request_repaint_after(RELOAD);
        self.filters(ui);
        ui.add_space(space::M);
        if let Some(message) = &self.message {
            info_bar(ui, InfoKind::Error, None, message);
            ui.add_space(space::S);
        }
        if let Err(e) = &self.entries {
            info_bar(ui, InfoKind::Error, None, e);
            return;
        }
        let mut shown = self.visible();
        if shown.is_empty() {
            if self.filtered() {
                let clear = empty_state(
                    ui,
                    Icon::Empty,
                    &tr("history-no-match-title"),
                    &tr("history-no-match-body"),
                    Some(&tr("history-clear-filters")),
                );
                if clear {
                    self.clear_filters();
                }
            } else {
                empty_state(
                    ui,
                    Icon::History,
                    &tr("history-empty-title"),
                    &tr("history-empty-body"),
                    None,
                );
            }
            if self.more && secondary_button(ui, &tr("history-show-older")).clicked() {
                self.show_older();
            }
            return;
        }
        sort(&mut shown, self.table.sort);
        // The selection follows its entry, not its place, as the list re-reads and re-sorts.
        self.table.selected = self
            .selected
            .and_then(|key| shown.iter().position(|e| e.key() == key));
        if self.table.selected.is_none() {
            self.selected = None;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(RichText::new(summary_line(&shown)).color(theme::of(ui).text_secondary));
                ui.add_space(space::S);
                card_frame(theme::of(ui)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // The columns' room, scrolled sideways in a narrow window, with a solid
                    // scroll bar so it's plain there's more.
                    let width = ui.available_width();
                    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                    egui::ScrollArea::horizontal()
                        .id_salt("history-scroll")
                        .show(ui, |ui| {
                            // At least the columns' least: narrower than that, it scrolls.
                            ui.set_width(width.max(self.started_width + TABLE_LEAST));
                            self.table(ui, &shown);
                        });
                });
                if self.more {
                    ui.add_space(space::S);
                    if secondary_button(ui, &tr("history-show-older")).clicked() {
                        self.show_older();
                    }
                }
                ui.add_space(space::M);
                if let Some(entry) = self.table.selected.and_then(|i| shown.get(i)).cloned() {
                    self.details(ui, &entry);
                }
            });
    }

    /// Whether a filter or the search narrows the list.
    fn filtered(&self) -> bool {
        self.job.is_some()
            || self.result.is_some()
            || self.period != Period::Any
            || !self.search.trim().is_empty()
    }

    fn clear_filters(&mut self) {
        self.job = None;
        self.result = None;
        self.period = Period::Any;
        self.search.clear();
    }

    /// The entries loaded that pass the period and the search (both on what's loaded: "Show
    /// older" reaches further back).
    fn visible(&self) -> Vec<Entry> {
        let since = self.period.since(Utc::now());
        let search = self.search.trim().to_lowercase();
        self.shown()
            .into_iter()
            .filter(|e| since.is_none_or(|s| e.started_at() >= s))
            .filter(|e| search.is_empty() || e.search_text().to_lowercase().contains(&search))
            .cloned()
            .collect()
    }

    fn table(&mut self, ui: &mut Ui, shown: &[Entry]) {
        let columns = [
            TableColumn::new(tr("history-col-started"), Some(self.started_width)).sortable(),
            // The job takes what the others leave.
            TableColumn::new(tr("history-col-job"), None)
                .sortable()
                .at_least(JOB_LEAST),
            TableColumn::new(tr("history-col-target"), Some(144.0)),
            // Room for "Partly done · 12 skipped".
            TableColumn::new(tr("history-col-result"), Some(166.0)).sortable(),
            TableColumn::new(tr("history-col-duration"), Some(84.0)).sortable(),
            TableColumn::new(tr("history-col-files"), Some(64.0))
                .sortable()
                .right_aligned(),
            TableColumn::new(tr("history-col-size"), Some(80.0))
                .sortable()
                .right_aligned(),
        ];
        let response = table(
            ui,
            &tr("tab-history"),
            &columns,
            shown.len(),
            // As tall as its rows, in the page's scroll area: every row is laid out, so a
            // screen reader reaches each one.
            None,
            &mut self.table,
            |i| shown[i].summary(),
            // Never shown: an empty list has its own empty state.
            |_| {},
            |ui, row, col| {
                let entry = &shown[row];
                match col {
                    0 => {
                        // To the minute: the seconds are in its details.
                        quiet_text(ui, &local_minute(&entry.started_at()), None);
                    }
                    1 => {
                        // A long name is cut, the test restore's chip kept after it.
                        let chip = tr("history-test-restore");
                        let room = match entry {
                            Entry::Test(_) => status_chip_width(ui, &chip),
                            Entry::Run(_) => 0.0,
                        };
                        leaving_room(ui, room, |ui| quiet_text(ui, entry.job(), None));
                        if matches!(entry, Entry::Test(_)) {
                            status_chip(ui, Tone::Info, &chip);
                        }
                    }
                    2 => {
                        quiet_text(ui, &entry.target_text(), None);
                    }
                    3 => {
                        status_chip(ui, outcome_tone(entry.outcome()), &entry.result_text());
                    }
                    4 => {
                        quiet_text(ui, &duration_text(entry.duration()), None);
                    }
                    5 => {
                        quiet_text(ui, &entry.files().to_string(), None);
                    }
                    _ => {
                        let size = entry.bytes().map_or_else(|| "—".to_owned(), size_text);
                        quiet_text(ui, &size, None);
                    }
                }
            },
        );
        if response.sort_changed {
            self.table.selected = None;
            self.selected = None;
        }
        if response.selection_changed {
            self.selected = self
                .table
                .selected
                .and_then(|i| shown.get(i))
                .map(Entry::key);
        }
    }

    /// The toolbar, on one row: the job, result and period, each a dropdown carrying its label,
    /// then the search in the rest of the width, and "Look again" at the right end. Too narrow
    /// for the search, the search goes on a second row, never the button.
    fn filters(&mut self, ui: &mut Ui) {
        let before = self.filter();
        let mut refresh = false;
        let mut search_below = false;
        let row = egui::vec2(ui.available_width(), theme::CONTROL_HEIGHT);
        let right = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(row, right, |ui| {
            ui.spacing_mut().item_spacing.x = space::TOOLBAR_GAP;
            refresh = icon_button(ui, Icon::Refresh, &tr("history-refresh")).clicked();
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                self.dropdowns(ui);
                toolbar_group_gap(ui);
                if ui.available_width() >= SEARCH_MIN_WIDTH {
                    self.search_field(ui);
                } else {
                    search_below = true;
                }
            });
        });
        if search_below {
            ui.add_space(space::TOOLBAR_GAP);
            self.search_field(ui);
        }
        if refresh || self.filter() != before {
            self.load();
        }
    }

    fn dropdowns(&mut self, ui: &mut Ui) {
        let jobs = self.jobs.clone();
        let (all_jobs, name) = (tr("history-all-jobs"), tr("history-filter-job"));
        let value = self.job.clone().unwrap_or_else(|| all_jobs.clone());
        dropdown(ui, "history-job", Some(&name), &value, JOB_WIDTH, |ui| {
            dropdown_value(ui, &mut self.job, None, all_jobs);
            for job in jobs {
                dropdown_value(ui, &mut self.job, Some(job.clone()), job);
            }
        })
        .accessible_name(name);

        let (all_results, name) = (tr("history-all-results"), tr("history-filter-result"));
        let value = self
            .result
            .map_or_else(|| all_results.clone(), outcome_text);
        dropdown(
            ui,
            "history-result",
            Some(&name),
            &value,
            FILTER_WIDTH,
            |ui| {
                dropdown_value(ui, &mut self.result, None, all_results);
                for outcome in [
                    Outcome::Ok,
                    Outcome::Partly,
                    Outcome::Skipped,
                    Outcome::Failed,
                ] {
                    dropdown_value(ui, &mut self.result, Some(outcome), outcome_text(outcome));
                }
            },
        )
        .accessible_name(name);

        let name = tr("history-filter-period");
        let value = self.period.label();
        dropdown(
            ui,
            "history-period",
            Some(&name),
            &value,
            FILTER_WIDTH,
            |ui| {
                for period in Period::ALL {
                    dropdown_value(ui, &mut self.period, period, period.label());
                }
            },
        )
        .accessible_name(name);
    }

    /// The search, as wide as what's left of its row.
    fn search_field(&mut self, ui: &mut Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(tr("history-search-hint"))
                .desired_width(f32::INFINITY)
                .min_size(egui::vec2(0.0, theme::CONTROL_HEIGHT))
                .align(egui::Align2::LEFT_CENTER),
        )
        .accessible_name(tr("history-search"));
    }

    /// The chosen entry: each target's result and why, the error, its log, and a copy of it all.
    fn details(&mut self, ui: &mut Ui, entry: &Entry) {
        let title = tr!(
            "history-details-title",
            job = entry.job(),
            when = local_text(&entry.started_at())
        );
        let log = match entry {
            Entry::Run(r) => r.log_path.clone(),
            Entry::Test(_) => None,
        };
        let mut copy = false;
        let mut open_log = false;
        section_card_with_actions(
            ui,
            &title,
            None,
            |ui| {
                copy = secondary_button(ui, &tr("history-copy-details")).clicked();
                if log.is_some() {
                    open_log = secondary_button(ui, &tr("history-open-log")).clicked();
                }
            },
            |ui| match entry {
                Entry::Run(run) => run_details(ui, run),
                Entry::Test(test) => test_details(ui, test),
            },
        );
        if copy {
            ui.ctx().copy_text(entry.details_text());
        }
        if open_log && let Some(log) = log {
            self.message = self
                .backend
                .open_path(&log)
                .err()
                .map(|e| tr!("history-open-log-failed", error = e));
        }
    }
}

/// How many runs (and test restores) a page holds.
pub const PAGE: usize = 200;

/// A page of the history: runs and test restores (each asked for with `limit`) merged, newest
/// first, cut where one kind may have older entries not loaded yet, so nothing is missing
/// from what's shown.
pub struct Page {
    pub entries: Vec<Entry>,
    pub more: bool,
}

pub fn paged(runs: Vec<RunRecord>, tests: Vec<TestRestoreRecord>, limit: usize) -> Page {
    let more_runs = runs.len() >= limit;
    let more_tests = tests.len() >= limit;
    // The oldest entry of each kind that may go on: below the later of those, something of
    // that kind may be missing.
    let cutoff = [
        more_runs
            .then(|| runs.last().map(|r| r.started_at))
            .flatten(),
        more_tests
            .then(|| tests.last().map(|t| t.started_at))
            .flatten(),
    ]
    .into_iter()
    .flatten()
    .max();
    let mut entries = merge(runs, tests);
    if let Some(cutoff) = cutoff {
        entries.retain(|e| e.started_at() >= cutoff);
    }
    Page {
        entries,
        more: more_runs || more_tests,
    }
}

/// A result in words, a partly-done one with how many files were skipped ("Partly done · 3
/// skipped") when that's known and not none: rows recorded before the history kept the count
/// say "Partly done" alone.
pub fn result_text(outcome: Outcome, skipped: Option<u64>) -> String {
    match skipped {
        Some(count) if outcome == Outcome::Partly && count > 0 => tr!(
            "history-partly-skipped",
            result = outcome_text(outcome),
            count = count
        ),
        _ => outcome_text(outcome),
    }
}

/// A target's mode, as the history keeps it, in words.
fn mode_label(mode: &str) -> String {
    match mode {
        "versioned" => tr("mode-versioned"),
        "mirror" => tr("mode-mirror"),
        other => other.to_owned(),
    }
}

/// A target's result as a line: its name and mode, result, files and size.
fn target_line(t: &replica_core::history::TargetRecord) -> String {
    tr!(
        "history-target",
        target = t.target.as_str(),
        mode = mode_label(&t.mode),
        result = result_text(t.outcome, t.skipped),
        files = t.files,
        size = size_text(t.bytes)
    )
}

/// A run's targets, each with its result and why it was skipped or failed, then its error.
fn run_details(ui: &mut Ui, run: &RunRecord) {
    for t in &run.targets {
        let name = tr!(
            "history-target-name",
            target = t.target.as_str(),
            mode = mode_label(&t.mode)
        );
        let amount = tr!(
            "history-target-amount",
            files = t.files,
            size = size_text(t.bytes)
        );
        let why = t.reason.clone().unwrap_or(amount);
        info_row(ui, &name, Some(&why), |ui| {
            status_chip(
                ui,
                outcome_tone(t.outcome),
                &result_text(t.outcome, t.skipped),
            )
        });
    }
    if let Some(error) = &run.error {
        ui.add_space(space::S);
        info_bar(ui, InfoKind::Error, None, error);
    }
}

fn test_target_line(test: &TestRestoreRecord) -> String {
    tr!(
        "history-test-target",
        target = test.target.as_str(),
        snapshot = test
            .snapshot_id
            .as_deref()
            .map(|s| &s[..s.len().min(8)])
            .unwrap_or("-")
    )
}

fn test_details(ui: &mut Ui, test: &TestRestoreRecord) {
    info_row(ui, &test_target_line(test), None, |ui| {
        status_chip(ui, outcome_tone(test.outcome), &outcome_text(test.outcome))
    });
    // One that couldn't get as far as comparing says why.
    if test.checked == 0
        && let Some(detail) = &test.detail
    {
        error_text(ui, detail);
        return;
    }
    match test.outcome {
        Outcome::Ok => {
            ui.label(tr!("test-restore-same", checked = test.checked));
        }
        Outcome::Skipped => {
            ui.label(tr("test-restore-nothing-compared"));
        }
        _ => {
            error_text(ui, &tr!("test-restore-differs", differing = test.differing));
            if let Some(detail) = &test.detail {
                for line in detail.lines() {
                    error_text(ui, line);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn run(job: &str, hour: u32, outcome: Outcome) -> RunRecord {
        RunRecord {
            id: hour as i64,
            job: job.into(),
            started_at: Utc.with_ymd_and_hms(2026, 10, 23, hour, 0, 0).unwrap(),
            duration: Duration::from_secs(5),
            outcome,
            exit_code: outcome.exit_code(),
            files: 3,
            bytes: 1000,
            error: None,
            log_path: None,
            targets: Vec::new(),
        }
    }

    fn test(job: &str, hour: u32) -> TestRestoreRecord {
        TestRestoreRecord {
            id: hour as i64,
            job: job.into(),
            target: "t".into(),
            snapshot_id: None,
            started_at: Utc.with_ymd_and_hms(2026, 10, 23, hour, 0, 0).unwrap(),
            duration: Duration::from_secs(1),
            outcome: Outcome::Ok,
            checked: 5,
            differing: 0,
            detail: None,
        }
    }

    #[test]
    fn a_page_stops_where_a_kind_may_have_more() {
        // Runs at 9..=6 (a full page of 4, so maybe more) and a test restore at 3.
        let runs: Vec<RunRecord> = (6..=9).rev().map(|h| run("a", h, Outcome::Ok)).collect();
        let page = paged(runs, vec![test("a", 3)], 4);
        assert!(page.more);
        // The test restore at 3 waits: runs between 6 and 3 may not be loaded yet.
        let hours: Vec<u32> = page
            .entries
            .iter()
            .map(|e| chrono::Timelike::hour(&e.started_at()))
            .collect();
        assert_eq!(hours, [9, 8, 7, 6]);

        let all = paged(vec![run("a", 9, Outcome::Ok)], vec![test("a", 3)], 4);
        assert!(!all.more);
        assert_eq!(all.entries.len(), 2);
    }

    #[test]
    fn modes_are_in_words() {
        assert_eq!(mode_label("versioned"), tr("mode-versioned"));
        assert_eq!(mode_label("mirror"), tr("mode-mirror"));
        assert_eq!(mode_label("other"), "other");
    }

    #[test]
    fn runs_and_test_restores_are_newest_first() {
        let all = merge(
            vec![run("a", 9, Outcome::Ok), run("a", 7, Outcome::Failed)],
            vec![test("a", 8)],
        );
        let hours: Vec<u32> = all
            .iter()
            .map(|e| chrono::Timelike::hour(&e.started_at()))
            .collect();
        assert_eq!(hours, [9, 8, 7]);
        assert!(matches!(all[1], Entry::Test(_)));
    }
}
