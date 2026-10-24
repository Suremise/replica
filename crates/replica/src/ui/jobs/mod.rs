//! The Jobs page: a table of every job with whether it's on, how it last ended, when it last
//! and next runs and where it backs up to; run one or all now (with progress, and Stop), turn
//! one on or off, edit, add and delete, and export and import jobs ([`transfer`]).
//!
//! The tray runs the jobs, one at a time ([`UiRequest::Run`]); the window only asks. Every
//! change is written to `settings.toml` at once and the tray told to reload it, so schedules
//! and Task Scheduler follow straight away.

pub mod editor;
pub mod form;
pub mod key_entry;
pub mod schedule;
pub mod setup;
pub mod transfer;

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Datelike, Local, Utc};
use eframe::egui::{self, Id, Modal, RichText, Ui};
use replica_core::i18n::tr;
use replica_core::job::{Job, Mode};
use replica_core::run::Outcome;
use replica_core::settings::{AppSettings, ScheduleMode, Settings};
use replica_core::tr;

use self::editor::{Editor, EditorResult};
use self::transfer::{Closed, ExportDialog, ImportDialog};
use super::backend::{Backend, Status, UiRequest, change_settings};
use super::icons::Icon;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, TableColumn, TableState, Tone, card_frame, danger_button_filled, dialog_frame,
    empty_state, flow, heading, icon_button, info_chip, leaving_room, message_bar, primary_button,
    progress_chip, relative_time, secondary_button, secondary_button_with_icon, status_chip, table,
    tag, tag_width, toggle, toolbar_group_gap,
};

/// How often the tab looks for a changed settings file (an edit by hand, the CLI, the tray).
const RECHECK: Duration = Duration::from_secs(1);

/// Under this width the table leaves out its Targets column.
const NARROW_TABLE: f32 = 780.0;

/// The table's columns, by what they show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    On,
    Job,
    Status,
    Last,
    Next,
    Targets,
    Actions,
}

/// The On and Actions columns' widths: a switch, and three icon buttons.
const ON_WIDTH: f32 = 48.0;
const ACTIONS_WIDTH: f32 = 104.0;
/// The Job column's narrowest when there's room, besides its mode's tag: it takes what the
/// others leave.
const JOB_LEAST: f32 = 200.0;
/// Its narrowest in a window too narrow for that.
const JOB_FLOOR: f32 = 120.0;

impl Col {
    /// Its width when there's room, and the least it's cut to so Job keeps its own (Job's are
    /// none: it takes what's left).
    fn widths(self) -> (f32, f32) {
        match self {
            Col::On => (ON_WIDTH, ON_WIDTH),
            Col::Actions => (ACTIONS_WIDTH, ACTIONS_WIDTH),
            Col::Job => (0.0, 0.0),
            // Room for "Waiting its turn", the widest chip.
            Col::Status => (136.0, 124.0),
            Col::Last => (92.0, 76.0),
            Col::Next => (190.0, 120.0),
            Col::Targets => (150.0, 100.0),
        }
    }

    /// The column, `width` wide (Job takes what's left, at least `job_least`).
    fn column(self, width: f32, job_least: f32) -> TableColumn {
        let key = match self {
            Col::On => "jobs-col-on",
            Col::Job => "jobs-col-job",
            Col::Status => "jobs-col-status",
            Col::Last => "jobs-col-last",
            Col::Next => "jobs-col-next",
            Col::Targets => "jobs-col-targets",
            Col::Actions => "jobs-col-actions",
        };
        let column = match self {
            Col::Job => TableColumn::new(tr(key), None).at_least(job_least),
            _ => TableColumn::new(tr(key), Some(width.floor())),
        };
        match self {
            Col::Job | Col::Status | Col::Last => column.sortable(),
            _ => column,
        }
    }
}

pub struct JobsTab {
    backend: Arc<dyn Backend>,
    settings: Result<Settings, String>,
    stamp: Option<SystemTime>,
    checked_at: Option<Instant>,
    /// The table's sort and selected row.
    table: TableState,
    editor: Option<Editor>,
    /// The job being deleted, and whether it has a scheduled task.
    deleting: Option<(String, bool)>,
    /// A note or an error across the top.
    message: Option<(bool, String)>,
    export: Option<ExportDialog>,
    import: Option<ImportDialog>,
    /// A job to select in the table once it's listed (one just imported).
    select: Option<String>,
}

impl JobsTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let mut tab = Self {
            backend,
            settings: Ok(Settings::default()),
            stamp: None,
            checked_at: None,
            table: TableState::default(),
            editor: None,
            deleting: None,
            message: None,
            export: None,
            import: None,
            select: None,
        };
        tab.load();
        tab
    }

    pub fn jobs(&self) -> &[Job] {
        self.settings.as_ref().map_or(&[], |s| &s.jobs)
    }

    /// Whether the editor is open (its changes would be lost by closing the window).
    pub fn editing(&self) -> bool {
        self.editor.is_some()
    }

    /// Opens the editor on `job`, or on a new one.
    pub fn edit(&mut self, job: Option<Job>) {
        self.message = None;
        self.editor = Some(Editor::new(Arc::clone(&self.backend), job));
    }

    /// The open editor, if any.
    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        self.editor.as_mut()
    }

    fn load(&mut self) {
        let path = self.backend.settings_file();
        self.stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        self.checked_at = Some(Instant::now());
        self.settings = Settings::load_or_default(&path)
            .map(|l| l.settings)
            .map_err(|e| e.to_string());
    }

    fn reload_if_changed(&mut self) {
        if self.checked_at.is_some_and(|t| t.elapsed() < RECHECK) {
            return;
        }
        let path = self.backend.settings_file();
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if stamp != self.stamp {
            self.load();
        } else {
            self.checked_at = Some(Instant::now());
        }
    }

    /// Changes the settings on disk (as they are now, not as last read), checks them, saves
    /// them and tells the tray.
    fn change(&mut self, edit: impl FnOnce(&mut Settings)) -> Result<(), String> {
        change_settings(self.backend.as_ref(), edit)?;
        self.load();
        Ok(())
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        self.reload_if_changed();
        if let Some(editor) = &mut self.editor {
            match editor.ui(ui) {
                EditorResult::Open => {}
                EditorResult::Cancelled => self.editor = None,
                EditorResult::Saved(name) => {
                    self.editor = None;
                    self.message = Some((false, tr!("jobs-saved", job = name.as_str())));
                    self.load();
                }
            }
            return;
        }
        let writable = self.backend.writable();
        let mut bars = Vec::new();
        if !writable {
            bars.push((
                true,
                tr!(
                    "window-read-only",
                    path = self.backend.settings_file().display().to_string()
                ),
            ));
        }
        if let Err(e) = &self.settings {
            bars.push((
                true,
                tr!(
                    "window-settings-error",
                    error = e.as_str(),
                    path = self.backend.settings_file().display().to_string()
                ),
            ));
        }
        bars.extend(self.message.clone());
        for (error, text) in bars {
            let kind = if error {
                InfoKind::Error
            } else {
                InfoKind::Info
            };
            message_bar(ui, kind, &flow(&text));
            ui.add_space(space::S);
        }
        let editable = writable && self.settings.is_ok();
        let status = self.backend.status();
        self.command_bar(ui, editable);
        ui.add_space(space::M);
        if self.settings.is_ok() && self.jobs().is_empty() {
            card_frame(theme::of(ui)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if ui
                    .add_enabled_ui(editable, |ui| {
                        empty_state(
                            ui,
                            Icon::Empty,
                            &tr("jobs-empty-title"),
                            &tr("jobs-empty-body"),
                            Some(&tr("jobs-new")),
                        )
                    })
                    .inner
                {
                    self.edit(None);
                }
            });
        } else if self.settings.is_ok() {
            self.table(ui, editable, &status);
        }
        self.delete_window(ui.ctx());
        self.transfer_windows(ui.ctx(), editable);
    }

    /// Opens Export... with `only` chosen, else the job selected in the table, else every job.
    fn open_export(&mut self, only: Option<&str>) {
        let chosen = only.map(str::to_owned).or_else(|| self.selected_name());
        self.export = Some(ExportDialog::new(self.jobs(), |j| {
            chosen
                .as_deref()
                .is_none_or(|c| j.name.eq_ignore_ascii_case(c))
        }));
    }

    /// Asks for a jobs file and opens its preview, or says why it can't be imported.
    fn open_import(&mut self) {
        let Some(path) = self.backend.open_jobs_file() else {
            return;
        };
        match ImportDialog::open(self.backend.as_ref(), &path) {
            Ok(dialog) => {
                self.message = None;
                self.import = Some(dialog);
            }
            Err(e) => self.message = Some((true, e)),
        }
    }

    /// The selected row's job, in the table's order.
    fn selected_name(&self) -> Option<String> {
        let row = self.table.selected?;
        self.sorted_jobs(&self.backend.status())
            .get(row)
            .map(|j| j.name.clone())
    }

    fn transfer_windows(&mut self, ctx: &egui::Context, editable: bool) {
        let closed = if let Some(export) = &mut self.export {
            export.ui(ctx, self.backend.as_ref())
        } else if let Some(import) = &mut self.import {
            import.ui(ctx, self.backend.as_ref(), editable)
        } else {
            return;
        };
        if closed == Closed::Open {
            return;
        }
        self.export = None;
        self.import = None;
        if let Closed::Done { message, select } = closed {
            self.message = Some((false, message));
            self.select = select;
            self.load();
        }
    }

    /// The page's actions: a new job, and running every job that's on.
    fn command_bar(&mut self, ui: &mut Ui, editable: bool) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled_ui(editable, |ui| {
                    ui.add_space(0.0);
                    primary_button(ui, &tr("jobs-new"))
                })
                .inner
                .clicked()
            {
                self.edit(None);
            }
            let enabled: Vec<String> = self
                .jobs()
                .iter()
                .filter(|j| j.enabled)
                .map(|j| j.name.clone())
                .collect();
            if ui
                .add_enabled_ui(!enabled.is_empty(), |ui| {
                    secondary_button_with_icon(ui, Icon::Play, &tr("jobs-run-all"))
                })
                .inner
                .clicked()
            {
                self.backend.send(UiRequest::Run(enabled));
            }
            toolbar_group_gap(ui);
            let any = !self.jobs().is_empty();
            if ui
                .add_enabled_ui(any, |ui| secondary_button(ui, &tr("jobs-export")))
                .inner
                .clicked()
            {
                self.open_export(None);
            }
            if ui
                .add_enabled_ui(editable, |ui| secondary_button(ui, &tr("jobs-import")))
                .inner
                .clicked()
            {
                self.open_import();
            }
        });
    }

    /// The jobs in the table's order.
    fn sorted_jobs(&self, status: &Status) -> Vec<Job> {
        let mut jobs = self.jobs().to_vec();
        if let Some(sort) = self.table.sort {
            jobs.sort_by(|a, b| {
                let order = match sort.column {
                    1 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    2 => status_rank(a, status).cmp(&status_rank(b, status)),
                    3 => status
                        .last(&a.name)
                        .map(|l| l.at)
                        .cmp(&status.last(&b.name).map(|l| l.at)),
                    _ => Ordering::Equal,
                };
                if sort.ascending {
                    order
                } else {
                    order.reverse()
                }
            });
        }
        jobs
    }

    fn table(&mut self, ui: &mut Ui, editable: bool, status: &Status) {
        let Ok(settings) = &self.settings else {
            return;
        };
        let app = settings.app.clone();
        let now = Utc::now();
        let jobs = self.sorted_jobs(status);
        if let Some(name) = self.select.take() {
            self.table.selected = jobs.iter().position(|j| j.name.eq_ignore_ascii_case(&name));
        }
        let cols: Vec<Col> = [
            Col::On,
            Col::Job,
            Col::Status,
            Col::Last,
            Col::Next,
            Col::Targets,
            Col::Actions,
        ]
        .into_iter()
        .filter(|c| *c != Col::Targets || ui.available_width() >= NARROW_TABLE)
        .collect();
        // Job takes what the others leave, at least JOB_LEAST beside its tag: short of that,
        // the others are cut towards their least, together.
        let room = ui.available_width() - 2.0 * space::S - 2.0;
        let tag_room = [
            (Mode::Versioned, false),
            (Mode::Versioned, true),
            (Mode::Mirror, false),
        ]
        .into_iter()
        .map(|(m, forever)| tag_width(ui, &mode_text(m, forever)))
        .fold(0.0, f32::max);
        let job_least = JOB_LEAST + tag_room + 2.0 * space::S;
        let (preferred, least) = cols
            .iter()
            .map(|c| c.widths())
            .fold((0.0, 0.0), |(p, l), (cp, cl)| (p + cp, l + cl));
        let cut = if preferred > least {
            ((preferred + job_least - room) / (preferred - least)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let job_floor = job_least.min((room - least).max(JOB_FLOOR));
        let columns: Vec<TableColumn> = cols
            .iter()
            .map(|c| {
                let (p, l) = c.widths();
                c.column(p - (p - l) * cut, job_floor)
            })
            .collect();

        let mut toggled = None;
        let mut edit = None;
        let mut delete = None;
        let mut export = None;
        let p = theme::of(ui);
        let height = ui.available_height() - 2.0 * space::M - 2.0;
        let response = card_frame(p)
            .inner_margin(egui::Margin::symmetric(space::S as i8, space::S as i8))
            .show(ui, |ui| {
                // The wide and the narrow table keep their column widths apart.
                ui.push_id(cols.len(), |ui| {
                    table(
                        ui,
                        &tr("jobs-table"),
                        &columns,
                        jobs.len(),
                        Some(height.max(120.0)),
                        &mut self.table,
                        |i| jobs[i].name.clone(),
                        |_| {},
                        |ui, row, col| {
                            let job = &jobs[row];
                            match cols[col] {
                                Col::On => {
                                    let mut on = job.enabled;
                                    let response = ui
                                        .add_enabled_ui(editable, |ui| toggle(ui, &mut on))
                                        .inner
                                        .accessible_name(tr!(
                                            "jobs-enabled-for",
                                            job = job.name.as_str()
                                        ));
                                    if response.changed() {
                                        toggled = Some((job.name.clone(), on));
                                    }
                                }
                                Col::Job => {
                                    // A long name is cut, its mode's tag kept after it.
                                    let forever =
                                        job.mode == Mode::Versioned && job.retention.keep_forever;
                                    let mode = mode_text(job.mode, forever);
                                    leaving_room(ui, tag_width(ui, &mode), |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&job.name).color(p.text),
                                            )
                                            .truncate(),
                                        )
                                    });
                                    let tag = tag(ui, &mode);
                                    if forever {
                                        tag.on_hover_text(tr("jobs-keep-forever-hint"));
                                    }
                                }
                                Col::Status => status_cell(ui, job, status),
                                Col::Last => last_cell(ui, job, status, now),
                                Col::Next => next_cell(ui, job, &app, status, now),
                                Col::Targets => {
                                    let text = targets_text(job);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&text).color(p.text_secondary),
                                        )
                                        .truncate(),
                                    );
                                }
                                Col::Actions => {
                                    if let Some(action) = row_actions(ui, job, editable, status) {
                                        match action {
                                            RowAction::Run => self
                                                .backend
                                                .send(UiRequest::Run(vec![job.name.clone()])),
                                            RowAction::Stop => self.backend.send(UiRequest::Stop),
                                            RowAction::Edit => edit = Some(job.clone()),
                                            RowAction::Delete => delete = Some(job.name.clone()),
                                            RowAction::Export => export = Some(job.name.clone()),
                                        }
                                    }
                                }
                            }
                        },
                    )
                })
                .inner
            })
            .inner;
        if let Some(row) = response.activated
            && editable
        {
            edit = jobs.get(row).cloned();
        }
        if let Some(job) = edit {
            self.edit(Some(job));
        }
        if let Some(name) = export {
            self.open_export(Some(&name));
        }
        if let Some(name) = delete {
            let has_task = self.backend.has_task(&name);
            self.deleting = Some((name, has_task));
        }
        if let Some((name, on)) = toggled {
            let result = self.change(|s| {
                if let Some(j) = s
                    .jobs
                    .iter_mut()
                    .find(|j| j.name.eq_ignore_ascii_case(&name))
                {
                    j.enabled = on;
                }
            });
            self.message = result.err().map(|e| (true, e));
        }
    }

    fn delete_window(&mut self, ctx: &egui::Context) {
        let Some((name, has_task)) = self.deleting.clone() else {
            return;
        };
        let mut answer = None;
        let modal = dialog(ctx, "delete-job").show(ctx, |ui| {
            ui.set_max_width(440.0);
            heading(ui, &tr!("jobs-delete-title", job = name.as_str()));
            ui.add_space(space::S);
            ui.label(flow(&tr("jobs-delete-body")));
            if has_task {
                ui.label(tr("jobs-delete-task"));
            }
            ui.add_space(space::L);
            ui.horizontal(|ui| {
                if danger_button_filled(ui, &tr("jobs-delete-confirm")).clicked() {
                    answer = Some(true);
                }
                if secondary_button(ui, &tr("button-cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(false);
        }
        match answer {
            Some(true) => {
                self.deleting = None;
                self.delete(&name, has_task);
            }
            Some(false) => self.deleting = None,
            None => {}
        }
    }

    fn delete(&mut self, name: &str, has_task: bool) {
        if let Err(e) = self.change(|s| s.jobs.retain(|j| !j.name.eq_ignore_ascii_case(name))) {
            self.message = Some((true, e));
            return;
        }
        self.table.selected = None;
        self.message = Some((false, tr!("jobs-deleted", job = name)));
        if has_task && let Err(e) = self.backend.delete_task(name) {
            self.message = Some((true, tr!("jobs-delete-task-failed", error = e)));
        }
    }
}

/// A dialog of the Jobs page: a modal in the app's dialog frame.
pub fn dialog(ctx: &egui::Context, id: &str) -> Modal {
    Modal::new(Id::unique(id)).frame(dialog_frame(ctx))
}

enum RowAction {
    Run,
    Stop,
    Edit,
    Export,
    Delete,
}

/// A row's buttons: Run now (Stop while it runs), Edit, and a menu with Export... and Delete.
fn row_actions(ui: &mut Ui, job: &Job, editable: bool, status: &Status) -> Option<RowAction> {
    let name = job.name.as_str();
    let mut action = None;
    if status.progress(name).is_some() {
        if icon_button(ui, Icon::Stop, &tr!("jobs-stop-for", job = name)).clicked() {
            action = Some(RowAction::Stop);
        }
    } else {
        let busy = status.is_queued(name);
        let run = ui
            .add_enabled_ui(!busy, |ui| {
                icon_button(ui, Icon::Play, &tr!("jobs-run-now-for", job = name))
            })
            .inner;
        if run.clicked() {
            action = Some(RowAction::Run);
        }
    }
    let edit = ui
        .add_enabled_ui(editable, |ui| {
            icon_button(ui, Icon::Edit, &tr!("jobs-edit-for", job = name))
        })
        .inner;
    if edit.clicked() {
        action = Some(RowAction::Edit);
    }
    // Export... reads only, so the menu is there when settings can't be saved too.
    let more = icon_button(ui, Icon::More, &tr!("jobs-more-for", job = name));
    egui::Popup::menu(&more).show(|ui| {
        let p = theme::of(ui);
        let export = ui
            .add(egui::Button::new(tr("jobs-export")).frame_when_inactive(false))
            .accessible_name(tr!("jobs-export-for", job = name));
        if export.clicked() {
            action = Some(RowAction::Export);
            ui.close();
        }
        if !editable {
            return;
        }
        let delete = ui
            .add(
                egui::Button::new(RichText::new(tr("jobs-delete")).color(p.failed))
                    .frame_when_inactive(false),
            )
            .accessible_name(tr!("jobs-delete-for", job = name));
        if delete.clicked() {
            action = Some(RowAction::Delete);
            ui.close();
        }
    });
    action
}

/// How a job's last result sorts: the worst first.
fn status_rank(job: &Job, status: &Status) -> u8 {
    if status.progress(&job.name).is_some() {
        return 0;
    }
    if status.overdue(&job.name).is_some() {
        return 1;
    }
    match status.last(&job.name).map(|l| l.outcome) {
        Some(Outcome::Failed) => 1,
        Some(Outcome::Partly) => 2,
        Some(Outcome::Skipped) => 3,
        Some(Outcome::Ok) => 4,
        None => 5,
    }
}

pub fn tone(outcome: Outcome) -> Tone {
    match outcome {
        Outcome::Ok => Tone::Ok,
        Outcome::Failed => Tone::Failed,
        Outcome::Partly | Outcome::Skipped => Tone::Warning,
    }
}

/// The last result as a chip, with why in its hover text; progress while it runs.
fn status_cell(ui: &mut Ui, job: &Job, status: &Status) {
    if let Some(wait) = status.waiting(&job.name) {
        info_chip(ui, Icon::History, &tr("jobs-queued")).on_hover_text(wait);
    } else if let Some(percent) = status.progress(&job.name) {
        progress_chip(
            ui,
            percent as f32 / 100.0,
            &tr!("jobs-running", percent = percent),
        );
    } else if status.is_queued(&job.name) {
        info_chip(ui, Icon::History, &tr("jobs-queued"));
    } else if let Some(overdue) = status.overdue(&job.name) {
        // A backup that silently stopped is the worst outcome: Overdue comes before
        // the last result, which its hover text still gives.
        let due = local_text(&overdue.due);
        let mut hint = match &overdue.last_success {
            Some(at) => tr!(
                "jobs-overdue-hint",
                due = due.as_str(),
                since = local_text(at).as_str()
            ),
            None => tr!("jobs-overdue-hint-never", due = due.as_str()),
        };
        if let Some(last) = status.last(&job.name) {
            hint.push('\n');
            hint.push_str(&tr!(
                "jobs-overdue-last",
                result = outcome_text(last.outcome).as_str(),
                reason = last.reason.as_deref().unwrap_or("")
            ));
        }
        status_chip(ui, Tone::Warning, &tr("jobs-overdue")).on_hover_text(hint.trim_end());
    } else {
        match status.last(&job.name) {
            Some(last) => {
                let chip = status_chip(ui, tone(last.outcome), &outcome_text(last.outcome));
                if let Some(reason) = &last.reason
                    && last.outcome != Outcome::Ok
                {
                    chip.on_hover_text(reason);
                }
            }
            None => {
                status_chip(ui, Tone::Neutral, &tr("result-never-run"))
                    .on_hover_text(tr("jobs-never-run-hint"));
            }
        }
    }
}

/// How long ago it last ran, with the exact time in the hover text.
fn last_cell(ui: &mut Ui, job: &Job, status: &Status, now: DateTime<Utc>) {
    let p = theme::of(ui);
    match status.last(&job.name) {
        Some(last) => {
            ui.label(relative_time(local(&last.at), local(&now)))
                .on_hover_text(local_text(&last.at));
        }
        None => {
            ui.label(RichText::new("–").color(p.text_secondary));
        }
    }
}

/// When it next runs, and under it how it's scheduled and what runs it.
fn next_cell(ui: &mut Ui, job: &Job, app: &AppSettings, status: &Status, now: DateTime<Utc>) {
    let p = theme::of(ui);
    let next = next_run(job, app, status, now);
    if next == NextRun::Manual {
        ui.label(RichText::new(tr("jobs-schedule-none")).color(p.text_secondary))
            .on_hover_text(next.text(now));
        return;
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let label = ui.add(egui::Label::new(next.text(now)).truncate());
        if let NextRun::At(at) = next {
            label.on_hover_text(local_text(&at));
        }
        ui.add(
            egui::Label::new(
                RichText::new(schedule_text(job, app))
                    .small()
                    .color(p.text_secondary),
            )
            .truncate(),
        );
    });
}

/// The job's targets, short: their names, or "BACKUP:Replica" and `\\nas\share` paths.
pub fn targets_text(job: &Job) -> String {
    if job.targets.is_empty() {
        return tr("jobs-targets-none");
    }
    job.targets
        .iter()
        .map(|t| t.display_name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A job's mode, as its tag says it: "Versioned · Forever" when it keeps everything.
fn mode_text(mode: Mode, forever: bool) -> String {
    match mode {
        Mode::Versioned if forever => tr("mode-versioned-forever"),
        Mode::Versioned => tr("mode-versioned"),
        Mode::Mirror => tr("mode-mirror"),
    }
}

pub fn outcome_text(outcome: Outcome) -> String {
    tr(match outcome {
        Outcome::Ok => "result-ok",
        Outcome::Partly => "result-partly",
        Outcome::Skipped => "result-skipped",
        Outcome::Failed => "result-failed",
    })
}

fn local_text(at: &DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// `at` as the local time on the clock.
fn local(at: &DateTime<Utc>) -> chrono::NaiveDateTime {
    at.with_timezone(&Local).naive_local()
}

/// How a job is scheduled: not at all, or its schedule and what runs it.
pub fn schedule_text(job: &Job, app: &AppSettings) -> String {
    let Some(schedule) = &job.schedule else {
        return tr("jobs-schedule-none");
    };
    let schedule = schedule.to_string();
    match app.schedule_mode {
        ScheduleMode::Timer => tr!("jobs-schedule-timer", schedule = schedule),
        ScheduleMode::TaskScheduler => tr!("jobs-schedule-tasks", schedule = schedule),
    }
}

/// When a job next runs on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextRun {
    /// It has no schedule.
    Manual,
    Off,
    Paused,
    /// It's due already: the timer runs it on its next round.
    Due,
    At(DateTime<Utc>),
}

impl NextRun {
    /// As people say it: "in 9 h" within 12 hours, else "Tomorrow 02:30", "Sat 10:00" within
    /// the week, or the date and time.
    pub fn text(self, now: DateTime<Utc>) -> String {
        let at = match self {
            NextRun::Manual => return tr("jobs-next-manual"),
            NextRun::Off => return tr("jobs-next-off"),
            NextRun::Paused => return tr("jobs-next-paused"),
            NextRun::Due => return tr("jobs-next-due"),
            NextRun::At(at) => at,
        };
        let (at_l, now_l) = (local(&at), local(&now));
        if at - now <= chrono::TimeDelta::hours(12) {
            return relative_time(at_l, now_l);
        }
        let time = at_l.format("%H:%M").to_string();
        match (at_l.date() - now_l.date()).num_days() {
            0 => tr!("jobs-next-today", time = time),
            1 => tr!("jobs-next-tomorrow", time = time),
            2..=6 => {
                let day = tr(match at_l.weekday() {
                    chrono::Weekday::Mon => "day-mon",
                    chrono::Weekday::Tue => "day-tue",
                    chrono::Weekday::Wed => "day-wed",
                    chrono::Weekday::Thu => "day-thu",
                    chrono::Weekday::Fri => "day-fri",
                    chrono::Weekday::Sat => "day-sat",
                    chrono::Weekday::Sun => "day-sun",
                });
                tr!("jobs-next-on", day = day, time = time)
            }
            _ => local_text(&at),
        }
    }
}

/// When a job next runs on its own, as the built-in timer counts it: from its last run, else
/// from when the tray started.
pub fn next_run(job: &Job, app: &AppSettings, status: &Status, now: DateTime<Utc>) -> NextRun {
    let Some(schedule) = &job.schedule else {
        return NextRun::Manual;
    };
    if !job.enabled {
        return NextRun::Off;
    }
    if app.pause_schedules {
        return NextRun::Paused;
    }
    let anchor = status
        .last(&job.name)
        .map(|l| l.at)
        .or(status.since)
        .unwrap_or(now)
        .with_timezone(&Local);
    let next = schedule.next_after(&anchor).with_timezone(&Utc);
    if next <= now {
        NextRun::Due
    } else {
        NextRun::At(next)
    }
}

/// [`next_run`], as [`NextRun::text`] says it.
pub fn next_run_text(job: &Job, app: &AppSettings, status: &Status, now: DateTime<Utc>) -> String {
    next_run(job, app, status, now).text(now)
}

pub fn last_text(job: &Job, status: &Status) -> String {
    match status.last(&job.name) {
        Some(last) => tr!(
            "jobs-last",
            result = outcome_text(last.outcome),
            when = local_text(&last.at)
        ),
        None => tr("result-never-run"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::backend::LastRun;
    use chrono::TimeZone;

    fn job(schedule: Option<&str>) -> Job {
        Job {
            name: "Hive".into(),
            schedule: schedule.map(|s| s.parse().unwrap()),
            ..Job::default()
        }
    }

    #[test]
    fn the_schedule_says_what_runs_it() {
        let mut app = AppSettings::default();
        assert_eq!(schedule_text(&job(None), &app), tr("jobs-schedule-none"));
        let text = schedule_text(&job(Some("every 6h")), &app);
        assert!(text.contains("every 6h"), "{text}");
        assert_eq!(
            text,
            tr!("jobs-schedule-timer", schedule = "every 6h".to_string())
        );
        app.schedule_mode = ScheduleMode::TaskScheduler;
        assert_eq!(
            schedule_text(&job(Some("every 6h")), &app),
            tr!("jobs-schedule-tasks", schedule = "every 6h".to_string())
        );
    }

    #[test]
    fn the_next_run_counts_from_the_last_one() {
        let app = AppSettings::default();
        let now = Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap();
        let mut status = Status::default();
        let every = job(Some("every 6h"));
        // Never run: counted from when the tray started, as its timer does.
        status.since = Some(now - chrono::TimeDelta::hours(7));
        assert_eq!(
            next_run_text(&every, &app, &status, now),
            tr("jobs-next-due")
        );
        status.since = Some(now - chrono::TimeDelta::hours(1));
        assert_eq!(
            next_run(&every, &app, &status, now),
            NextRun::At(now + chrono::TimeDelta::hours(5))
        );
        assert_eq!(next_run_text(&every, &app, &status, now), "in 5 h");
        status.last.insert(
            "hive".into(),
            LastRun {
                at: now - chrono::TimeDelta::hours(1),
                outcome: Outcome::Ok,
                reason: None,
            },
        );
        assert_eq!(
            next_run(&every, &app, &status, now),
            NextRun::At(now + chrono::TimeDelta::hours(5))
        );
        assert_eq!(
            next_run_text(&job(None), &app, &status, now),
            tr("jobs-next-manual")
        );
        let off = Job {
            enabled: false,
            ..every.clone()
        };
        assert_eq!(next_run_text(&off, &app, &status, now), tr("jobs-next-off"));
        let paused = AppSettings {
            pause_schedules: true,
            ..app
        };
        assert_eq!(
            next_run_text(&every, &paused, &status, now),
            tr("jobs-next-paused")
        );
    }

    #[test]
    fn the_last_run_says_how_it_ended() {
        let mut status = Status::default();
        assert_eq!(last_text(&job(None), &status), tr("result-never-run"));
        let at = Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap();
        status.last.insert(
            "hive".into(),
            LastRun {
                at,
                outcome: Outcome::Failed,
                reason: None,
            },
        );
        let text = last_text(&job(None), &status);
        assert!(text.contains(&tr("result-failed")), "{text}");
    }

    #[test]
    fn the_next_run_reads_as_people_say_it() {
        // From local noon on a Wednesday in July, away from a clock change, so the days
        // named don't depend on the tests' time zone.
        let now = Local
            .with_ymd_and_hms(2026, 7, 1, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let at = |hours| NextRun::At(now + chrono::TimeDelta::hours(hours)).text(now);
        assert_eq!(at(9), "in 9 h");
        assert_eq!(at(14), "Tomorrow 02:00");
        assert_eq!(at(46), "Fri 10:00");
        assert!(at(24 * 9).starts_with("2026-07-10"), "{}", at(24 * 9));
        assert_eq!(NextRun::Due.text(now), tr("jobs-next-due"));
    }

    #[test]
    fn a_failed_run_s_reason_is_its_own_error_else_a_target_s() {
        assert_eq!(
            LastRun::reason_of(Some("locked"), [("BACKUP", Some("not connected"))]),
            Some("locked".into())
        );
        assert_eq!(
            LastRun::reason_of(None, [("A", None), ("BACKUP", Some("not connected"))]),
            Some("BACKUP: not connected".into())
        );
        assert_eq!(LastRun::reason_of(None, [("A", None)]), None);
    }
}
