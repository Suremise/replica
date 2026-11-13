//! The History tab: every run, newest first, with each target's result (and why it was
//! skipped or failed), its start, duration, files, size and error, and its log; and the test
//! restores, kept apart in the history but shown here with the runs.
//!
//! It's a sortable table under a toolbar (job, result, a period and a search), with a summary
//! line above it, a pager under it and the chosen entry's details below. The filters, the
//! search and the order apply to the whole history, and only the page shown is read from it
//! ([`replica_core::history::History::page`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, RichText, Ui};
use replica_core::history::{Counts, HistoryQuery, Record, RunRecord, SortBy, TestRestoreRecord};
use replica_core::i18n::tr;
use replica_core::run::Outcome;
use replica_core::tr;

use super::backend::Backend;
use super::icons::Icon;
use super::jobs::outcome_text;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, Sort, TableColumn, TableState, Tone, cell_status_chip, dropdown, dropdown_value,
    empty_state, error_text, icon_button, info_bar, info_row, leaving_room, page_size, pager,
    quiet_mono, quiet_path, quiet_text, secondary_button, section_card_with_actions, size_text,
    status_chip, status_chip_width, table, table_frame, toolbar_group_gap,
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
const TABLE_LEAST: f32 = JOB_LEAST + 160.0 + 172.0 + 96.0 + 64.0 + 84.0;

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
                    for c in &t.clashes {
                        text.extend([" ", c.as_str()]);
                    }
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
                    lines.extend(t.clashes.iter().cloned());
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

impl From<Record> for Entry {
    fn from(record: Record) -> Self {
        match record {
            Record::Run(r) => Entry::Run(r),
            Record::Test(t) => Entry::Test(t),
        }
    }
}

/// The history's order for the table's `sort` (its columns, by place): newest first without
/// one. The Target column isn't sortable.
fn sort_by(sort: Option<Sort>) -> Option<(SortBy, bool)> {
    let sort = sort?;
    let column = match sort.column {
        0 => SortBy::Started,
        1 => SortBy::Job,
        3 => SortBy::Result,
        4 => SortBy::Duration,
        5 => SortBy::Files,
        6 => SortBy::Size,
        _ => return None,
    };
    Some((column, sort.ascending))
}

/// Whether the search `needle` (trimmed, in lower case) is in `record`'s
/// [`Entry::search_text`]: the words the window shows it in, any case, `%` and `_` as
/// themselves.
pub fn matches_search(record: &Record, needle: &str) -> bool {
    Entry::from(record.clone())
        .search_text()
        .to_lowercase()
        .contains(needle)
}

/// "12 runs · 2 test restores · 10 OK · 1 failed · 1 skipped": what's listed (every page),
/// counted.
pub fn summary_line(counts: &Counts) -> String {
    let mut parts = vec![tr!("history-summary-runs", count = counts.runs)];
    if counts.tests > 0 {
        parts.push(tr!("history-summary-tests", count = counts.tests));
    }
    for (outcome, key) in [
        (Outcome::Ok, "history-summary-ok"),
        (Outcome::Partly, "history-summary-partly"),
        (Outcome::Failed, "history-summary-failed"),
        (Outcome::Skipped, "history-summary-skipped"),
    ] {
        let n = counts.of(outcome);
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

/// What the list shows: its filters, search, order and page. When it differs from what's
/// loaded, the history is read again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Want {
    job: Option<String>,
    result: Option<Outcome>,
    period: Period,
    /// The search, trimmed and in lower case.
    search: String,
    sort: Option<Sort>,
    /// The page's first entry in the whole list, and how many a page has.
    offset: usize,
    size: usize,
}

impl Want {
    /// Whether `other` lists the same entries (whatever the order or page).
    fn same_list(&self, other: &Want) -> bool {
        (&self.job, self.result, self.period, &self.search)
            == (&other.job, other.result, other.period, &other.search)
    }
}

/// A page of the history as read, and what for.
struct Loaded {
    want: Want,
    counts: Counts,
    entries: Vec<Entry>,
}

pub struct HistoryTab {
    backend: Arc<dyn Backend>,
    loaded: Option<Loaded>,
    /// Why the history couldn't be read, if it couldn't.
    error: Option<String>,
    /// Every job in the history, for the job filter.
    jobs: Vec<String>,
    loaded_at: Option<Instant>,
    /// Only this job, if chosen.
    pub job: Option<String>,
    /// Only this result, if chosen.
    pub result: Option<Outcome>,
    /// Only entries this recent.
    pub period: Period,
    /// Only entries whose job, targets, result or errors have this.
    pub search: String,
    /// The Started column's width as the table first shows it.
    pub started_width: f32,
    table: TableState,
    /// The chosen entry, by [`Entry::key`].
    selected: Option<(bool, i64)>,
    /// The chosen entry as last read: its details stay below while the list pages away.
    chosen: Option<Entry>,
    /// A row chosen, by its place in the whole list, on a page not read yet (Page Down): it's
    /// chosen once its page is.
    pending: Option<usize>,
    message: Option<String>,
}

impl HistoryTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            loaded: None,
            error: None,
            jobs: Vec::new(),
            loaded_at: None,
            job: None,
            result: None,
            period: Period::Any,
            search: String::new(),
            started_width: 124.0,
            table: TableState::default(),
            selected: None,
            chosen: None,
            pending: None,
            message: None,
        }
    }

    fn want(&self, size: usize) -> Want {
        Want {
            job: self.job.clone(),
            result: self.result,
            period: self.period,
            search: self.search.trim().to_lowercase(),
            sort: self.table.sort,
            offset: self.table.page * size,
            size,
        }
    }

    /// Reads the history again on the next frame (a run finished, the filters were set).
    pub fn load(&mut self) {
        self.loaded_at = None;
    }

    /// Reads the page the list wants, `size` a page. The filters, the period and the search
    /// apply to the whole history (the search entry by entry, in the words the window shows
    /// them in: [`Entry::search_text`]), then the order, then the page: only the page is
    /// read. A new filter or search starts at the first page, and lets go of a chosen entry
    /// it leaves out; a page past the end (the list shrank) goes to the last.
    fn read(&mut self, size: usize) {
        self.loaded_at = Some(Instant::now());
        let new_list = self
            .loaded
            .as_ref()
            .is_some_and(|l| !l.want.same_list(&self.want(size)));
        if new_list {
            self.table.page = 0;
            self.pending = None;
        }
        for _ in 0..2 {
            let want = self.want(size);
            let query = HistoryQuery {
                job: want.job.clone(),
                outcome: want.result,
                since: want.period.since(Utc::now()),
                sort: sort_by(want.sort),
            };
            let needle = want.search.clone();
            let mut search = |r: &Record| matches_search(r, &needle);
            let matches: Option<&mut dyn FnMut(&Record) -> bool> = if needle.is_empty() {
                None
            } else {
                Some(&mut search)
            };
            match self
                .backend
                .history_page(&query, want.offset, size, matches)
            {
                Ok(page) => {
                    let total = page.counts.total();
                    // Past the end: the last page, read again once.
                    if page.records.is_empty() && want.offset > 0 && total > 0 {
                        self.table.page = (total - 1) / size;
                        continue;
                    }
                    self.loaded = Some(Loaded {
                        want,
                        counts: page.counts,
                        entries: page.records.into_iter().map(Entry::from).collect(),
                    });
                    self.error = None;
                }
                Err(e) => {
                    self.loaded = None;
                    self.error = Some(e);
                }
            }
            break;
        }
        // A new list keeps the chosen entry only if it's on the page it starts at.
        if new_list
            && let Some(key) = self.selected
            && !self.shown().iter().any(|e| e.key() == key)
        {
            self.selected = None;
            self.chosen = None;
        }
        self.jobs = self.backend.history_jobs().unwrap_or_default();
    }

    /// The entries on the page read, in the list's order.
    pub fn shown(&self) -> Vec<&Entry> {
        self.loaded
            .as_ref()
            .map(|l| l.entries.iter().collect())
            .unwrap_or_default()
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        self.filters(ui);
        ui.add_space(space::M);
        // The page as the table will show it: a new page size keeps its first row in view.
        let size = page_size(ui.ctx());
        if let Some(loaded) = &self.loaded
            && loaded.want.same_list(&self.want(size))
        {
            let total = loaded.counts.total();
            self.table.page_rows(ui.ctx(), total);
        }
        let stale = self.loaded_at.is_none_or(|t| t.elapsed() >= RELOAD);
        if stale
            || self
                .loaded
                .as_ref()
                .is_none_or(|l| l.want != self.want(size))
        {
            self.read(size);
        }
        ui.ctx().request_repaint_after(RELOAD);
        if let Some(message) = &self.message {
            info_bar(ui, InfoKind::Error, None, message);
            ui.add_space(space::S);
        }
        if let Some(e) = &self.error {
            info_bar(ui, InfoKind::Error, None, e);
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        let (counts, offset) = (loaded.counts, loaded.want.offset);
        let entries = loaded.entries.clone();
        if counts.total() == 0 {
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
            // Under every table, its pager: "0 of 0" here.
            pager(ui, &mut self.table, 0);
            return;
        }
        // The row chosen on a page that wasn't read yet, now it is.
        if let Some(i) = self.pending
            && let Some(e) = i.checked_sub(offset).and_then(|i| entries.get(i))
        {
            self.selected = Some(e.key());
            self.pending = None;
        }
        // The selection follows its entry, not its place, as the list re-reads; chosen on
        // another page, no row here is selected, and its details stay below.
        self.table.selected = self
            .selected
            .and_then(|key| entries.iter().position(|e| e.key() == key))
            .map(|i| offset + i);
        if let Some(key) = self.selected
            && let Some(e) = entries.iter().find(|e| e.key() == key)
        {
            self.chosen = Some(e.clone());
        }
        self.table.loaded = Some(offset..offset + entries.len());
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(RichText::new(summary_line(&counts)).color(theme::of(ui).text_secondary));
                ui.add_space(space::S);
                table_frame(theme::of(ui)).show(ui, |ui| {
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
                            self.table(ui, &entries, offset, counts.total());
                        });
                });
                // Outside the sideways scrolling, so its controls stay in reach.
                pager(ui, &mut self.table, counts.total());
                ui.add_space(space::M);
                if let Some(entry) = self.chosen.clone() {
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

    /// The table of the page read: `entries`, the whole list's from `offset`, of `total`.
    fn table(&mut self, ui: &mut Ui, entries: &[Entry], offset: usize, total: usize) {
        let columns = [
            TableColumn::new(tr("history-col-started"), Some(self.started_width)).sortable(),
            // The job takes what the others leave.
            TableColumn::new(tr("history-col-job"), None)
                .sortable()
                .at_least(JOB_LEAST),
            TableColumn::new(tr("history-col-target"), Some(160.0)),
            // Room for "Partly done · 12 skipped".
            TableColumn::new(tr("history-col-result"), Some(172.0)).sortable(),
            // Room for "1 min 58 s" in monospace.
            TableColumn::new(tr("history-col-duration"), Some(96.0))
                .sortable()
                .right_aligned(),
            TableColumn::new(tr("history-col-files"), Some(64.0))
                .sortable()
                .right_aligned(),
            TableColumn::new(tr("history-col-size"), Some(84.0))
                .sortable()
                .right_aligned(),
        ];
        // The table draws only the page read (`TableState::loaded`): a row's place in the whole
        // list, less `offset`, is its place here.
        let at = |row: usize| &entries[row - offset];
        let response = table(
            ui,
            &tr("tab-history"),
            &columns,
            total,
            // As tall as its rows, in the page's scroll area: every row is laid out, so a
            // screen reader reaches each one.
            None,
            &mut self.table,
            |i| at(i).summary(),
            // Never shown: an empty list has its own empty state.
            |_| {},
            |ui, row, col| {
                let entry = at(row);
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
                        quiet_path(ui, &entry.target_text(), None);
                    }
                    3 => {
                        cell_status_chip(ui, outcome_tone(entry.outcome()), &entry.result_text());
                    }
                    4 => {
                        quiet_mono(ui, &duration_text(entry.duration()), None);
                    }
                    5 => {
                        quiet_mono(ui, &entry.files().to_string(), None);
                    }
                    _ => {
                        let size = entry.bytes().map_or_else(|| "—".to_owned(), size_text);
                        quiet_mono(ui, &size, None);
                    }
                }
            },
        );
        if response.sort_changed {
            self.table.selected = None;
            self.selected = None;
            self.chosen = None;
            self.pending = None;
        }
        if response.selection_changed {
            let page = offset..offset + entries.len();
            match self.table.selected {
                Some(i) if page.contains(&i) => {
                    let entry = &entries[i - offset];
                    self.selected = Some(entry.key());
                    self.chosen = Some(entry.clone());
                    self.pending = None;
                }
                // On a page not read yet: chosen once it is.
                Some(i) => self.pending = Some(i),
                None => {
                    self.selected = None;
                    self.chosen = None;
                }
            }
        }
    }

    /// The toolbar, on one row: the job, result and period, each a dropdown carrying its label,
    /// then the search in the rest of the width, and "Look again" at the right end. Too narrow
    /// for the search, the search goes on a second row, never the button.
    fn filters(&mut self, ui: &mut Ui) {
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
        // A changed filter or search is read by `ui`, which sees the list it wants change.
        if refresh {
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

/// A run's targets, each with its result and why it was skipped or failed, and a mirror's
/// clashes, each saying what to do; then the run's error.
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
        for clash in &t.clashes {
            ui.label(clash);
        }
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

    /// `run` with one target, `skipped` files it couldn't back up, and `reason`.
    fn run_to(
        job: &str,
        hour: u32,
        outcome: Outcome,
        target: &str,
        skipped: Option<u64>,
        reason: &str,
    ) -> Record {
        Record::Run(RunRecord {
            targets: vec![replica_core::history::TargetRecord {
                target: target.into(),
                mode: "versioned".into(),
                outcome,
                files: 3,
                bytes: 1000,
                reason: Some(reason.into()),
                snapshot_id: None,
                skipped,
                clashes: vec!["a clash to resolve".into()],
            }],
            ..run(job, hour, outcome)
        })
    }

    /// The search as the window does it: trimmed and in lower case.
    fn found(record: &Record, search: &str) -> bool {
        matches_search(record, &search.trim().to_lowercase())
    }

    #[test]
    fn the_search_reads_entries_as_the_window_words_them() {
        let partly = run_to("docs", 9, Outcome::Partly, "Диск", Some(3), "Ошибка чтения");
        let other = run_to("docs", 8, Outcome::Partly, "nas", Some(2), "plain");
        let odd = run_to("a_b%c", 7, Outcome::Ok, "nas", None, "fine");
        let abc = run_to("abc", 6, Outcome::Ok, "nas", None, "fine");
        let failed = Record::Run(run("music", 5, Outcome::Failed));
        let failed_test = Record::Test(TestRestoreRecord {
            outcome: Outcome::Failed,
            detail: Some("Report.docx differs".into()),
            ..test("music", 4)
        });

        // Its result as shown, with how many files were skipped.
        assert!(found(&partly, "3 skipped"));
        assert!(!found(&other, "3 skipped"));
        // Any case, beyond ASCII too, in a target's name and its reason.
        assert!(found(&partly, "диск"));
        assert!(found(&partly, "  ОШИБКА  "));
        // `%` and `_` are themselves, not patterns.
        assert!(found(&odd, "_b%"));
        assert!(!found(&abc, "a%"));
        assert!(!found(&abc, "a_c"));
        // A result in its words, for runs and test restores alike.
        let word = outcome_text(Outcome::Failed);
        assert!(found(&failed, &word));
        assert!(found(&failed_test, &word));
        assert!(!found(&abc, &word));
        // A clash, and a test restore's detail.
        assert!(found(&partly, "clash to resolve"));
        assert!(found(&failed_test, "report.docx"));
    }

    #[test]
    fn modes_are_in_words() {
        assert_eq!(mode_label("versioned"), tr("mode-versioned"));
        assert_eq!(mode_label("mirror"), tr("mode-mirror"));
        assert_eq!(mode_label("other"), "other");
    }
}
