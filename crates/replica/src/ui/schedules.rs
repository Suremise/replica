//! The Schedules page, in the sidebar:
//! - **A job's schedule**, chosen as Restore chooses a backup, and changed with the job editor's
//!   own picker ([`SchedulePicker`]): its own, and its integrity checks' where it has them, with
//!   when it next runs and what runs it. Save writes it as the editor's Save does
//!   ([`save_job`]); everything else about the job is the editor's ("Edit job…").
//! - **Replica's tasks in Windows Task Scheduler** (nothing hidden, by design, so they're
//!   listed whatever runs the schedules): every task in its "Replica" folder, orphans flagged,
//!   each turned on or off or deleted, all removed, and Windows' own Task Scheduler a click
//!   away.
//!
//! What runs the schedules, and pausing them, are options: they stay in Settings, on its
//! Scheduling tab, which links here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Ui};
use replica_core::i18n::tr;
use replica_core::job::Job;
use replica_core::run::Outcome;
use replica_core::settings::{ScheduleMode, Settings};
use replica_core::tr;
use replica_win::scheduler::ScheduledTask;

use super::backend::{Backend, Places};
use super::icons::Icon;
use super::jobs::editor::save_job;
use super::jobs::form::{AfterForm, JobForm};
use super::jobs::schedule::SchedulePicker;
use super::jobs::{next_run_text, outcome_text};
use super::settings::ask;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, TableColumn, TableState, Tone, danger_button, dropdown, dropdown_choice, empty_state,
    flow, heading, icon_button, info_bar, info_row, message_bar, pager, path_label, primary_button,
    relative_time, secondary_button, section_card_with_actions, small_toggle, status_chip, table,
    table_frame,
};

/// How often it reads the settings and the tasks again while shown.
const RELOAD: Duration = Duration::from_secs(3);

/// A question the user has to answer first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    DeleteTask(String),
    DeleteAll,
    /// Discard the schedule's changes, then do this.
    Discard(Then),
}

/// What waits on "Discard changes?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    /// Show this job instead.
    Choose(String),
    /// Open the job editor on it.
    EditJob,
}

/// What the page asks the frame to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The editor on this job, as it's saved.
    EditJob(Job),
    /// The editor on a new job.
    NewJob,
    /// A schedule was saved: read the settings again now (the status bar, the Jobs table).
    Saved,
}

/// The chosen job's schedules being changed: the form Save writes, with a picker for its own
/// schedule and one for each integrity check.
struct Draft {
    /// The job as saved when it was chosen (or saved here).
    saved: Job,
    form: JobForm,
    picker: SchedulePicker,
    checks: Vec<SchedulePicker>,
}

impl Draft {
    fn of(job: &Job) -> Draft {
        let form = JobForm::from_job(job);
        let checks = verify_schedules(&form)
            .map(|text| {
                SchedulePicker::from_text(text).with_names(
                    "schedules-check-runs",
                    "schedules-check-every-backup",
                    "schedules-check-hint",
                )
            })
            .collect();
        Draft {
            saved: job.clone(),
            picker: SchedulePicker::from_text(&form.schedule),
            checks,
            form,
        }
    }

    fn changed(&self) -> bool {
        self.form != JobForm::from_job(&self.saved)
    }
}

/// The integrity checks' schedules, as the form holds them.
fn verify_schedules(form: &JobForm) -> impl Iterator<Item = &String> {
    form.after.iter().filter_map(|step| match step {
        AfterForm::Verify { schedule, .. } => Some(schedule),
        AfterForm::Command { .. } => None,
    })
}

fn verify_schedules_mut(form: &mut JobForm) -> impl Iterator<Item = &mut String> {
    form.after.iter_mut().filter_map(|step| match step {
        AfterForm::Verify { schedule, .. } => Some(schedule),
        AfterForm::Command { .. } => None,
    })
}

pub struct SchedulesTab {
    backend: Arc<dyn Backend>,
    places: Places,
    pub settings: Result<Settings, String>,
    pub tasks: Result<Vec<ScheduledTask>, String>,
    loaded_at: Option<Instant>,
    pub confirm: Option<Confirm>,
    /// What the last change did, or why it failed (`true`).
    message: Option<(bool, String)>,
    task_table: TableState,
    /// The job whose schedule is shown, by name: kept while the app runs.
    pub job: Option<String>,
    draft: Option<Draft>,
    /// Why Save didn't save, in the editor's words.
    errors: Vec<String>,
    /// For the frame: open the editor.
    pub request: Option<Request>,
}

impl SchedulesTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let places = backend.places();
        Self {
            backend,
            places,
            settings: Ok(Settings::default()),
            tasks: Ok(Vec::new()),
            loaded_at: None,
            confirm: None,
            message: None,
            task_table: TableState::default(),
            job: None,
            draft: None,
            errors: Vec::new(),
            request: None,
        }
    }

    /// Shows `name`'s schedule (a link from elsewhere): at once, or after "Discard changes?"
    /// if the one shown has some.
    pub fn show_job(&mut self, name: &str) {
        self.choose(name.to_owned());
    }

    /// Whether the schedule shown has changes that aren't saved.
    pub fn changed(&self) -> bool {
        self.draft.as_ref().is_some_and(Draft::changed)
    }

    /// Reads the settings (for the jobs' names, to flag orphans) and the tasks again.
    pub fn load(&mut self) {
        self.loaded_at = Some(Instant::now());
        self.settings = Settings::load_or_default(&self.backend.settings_file())
            .map(|l| l.settings)
            .map_err(|e| e.to_string());
        let jobs: Vec<String> = self
            .settings
            .as_ref()
            .map(|s| s.jobs.iter().map(|j| j.name.clone()).collect())
            .unwrap_or_default();
        let names: Vec<&str> = jobs.iter().map(String::as_str).collect();
        self.tasks = self.backend.tasks(&names);
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        if self.loaded_at.is_none_or(|t| t.elapsed() >= RELOAD) {
            self.load();
        }
        ui.ctx().request_repaint_after(RELOAD);
        if let Some((error, text)) = &self.message {
            let kind = if *error {
                InfoKind::Error
            } else {
                InfoKind::Info
            };
            info_bar(ui, kind, None, text);
            ui.add_space(space::M);
        }
        // Settings that can't be read: without the jobs, every task would look an orphan,
        // so nothing more is shown until they're fixed (as Settings does).
        let editable = self.places.writable;
        if let Err(e) = &self.settings {
            info_bar(
                ui,
                InfoKind::Error,
                None,
                &flow(&tr!(
                    "window-settings-error",
                    error = e.as_str(),
                    path = self.places.settings.display().to_string()
                )),
            );
            return;
        }
        if !editable {
            info_bar(
                ui,
                InfoKind::Warning,
                None,
                &tr!(
                    "window-read-only",
                    path = self.places.settings.display().to_string()
                ),
            );
            ui.add_space(space::M);
        }
        egui::ScrollArea::vertical()
            .id_salt("schedules-page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.schedule_section(ui, editable);
                self.tasks_section(ui, editable);
            });
        self.confirm_window(ui.ctx());
    }

    // --- A job's schedule -----------------------------------------------------------------

    fn jobs(&self) -> &[Job] {
        self.settings.as_ref().map_or(&[], |s| &s.jobs)
    }

    /// The job chosen, as saved: the one asked for if it's still there, else the first.
    fn chosen(&self) -> Option<Job> {
        let jobs = self.jobs();
        self.job
            .as_ref()
            .and_then(|name| jobs.iter().find(|j| j.name.eq_ignore_ascii_case(name)))
            .or_else(|| jobs.first())
            .cloned()
    }

    /// Keeps the draft on the chosen job: a fresh one when the job changes, or when its saved
    /// form changes (in the editor, say) and nothing here is unsaved.
    fn sync_draft(&mut self) {
        let Some(job) = self.chosen() else {
            self.draft = None;
            return;
        };
        self.job = Some(job.name.clone());
        let fresh = match &self.draft {
            None => true,
            Some(d) if !d.saved.name.eq_ignore_ascii_case(&job.name) => true,
            Some(d) => d.saved != job && !d.changed(),
        };
        if fresh {
            self.draft = Some(Draft::of(&job));
            self.errors.clear();
        }
    }

    /// Shows `name`: at once if nothing's unsaved, else after "Discard changes?".
    fn choose(&mut self, name: String) {
        if self.changed() {
            self.confirm = Some(Confirm::Discard(Then::Choose(name)));
        } else {
            self.job = Some(name);
            self.draft = None;
            self.errors.clear();
        }
    }

    fn schedule_section(&mut self, ui: &mut Ui, editable: bool) {
        self.sync_draft();
        let mut edit = false;
        let has_job = self.draft.is_some();
        section_card_with_actions(
            ui,
            &tr("schedules-job-title"),
            Some(&tr("schedules-job-help")),
            |ui| {
                if has_job {
                    edit = secondary_button(ui, &tr("schedules-edit-job")).clicked();
                }
            },
            |ui| {
                if !has_job {
                    if empty_state(
                        ui,
                        Icon::Jobs,
                        &tr("schedules-no-jobs-title"),
                        &tr("schedules-no-jobs"),
                        editable.then(|| tr("jobs-new")).as_deref(),
                    ) {
                        self.request = Some(Request::NewJob);
                    }
                    return;
                }
                self.job_picker(ui);
                ui.add_space(space::S);
                ui.add_enabled_ui(editable, |ui| self.schedules(ui));
                self.what_it_means(ui);
                self.save_row(ui, editable);
            },
        );
        if edit {
            if self.changed() {
                self.confirm = Some(Confirm::Discard(Then::EditJob));
            } else if let Some(d) = &self.draft {
                self.request = Some(Request::EditJob(d.saved.clone()));
            }
        }
    }

    /// The job, as Restore's job picker: every job, one that's off saying so.
    fn job_picker(&mut self, ui: &mut Ui) {
        let jobs: Vec<Job> = self.jobs().to_vec();
        let shown = |job: &Job| {
            if job.enabled {
                job.name.clone()
            } else {
                tr!("schedules-job-off", job = job.name.as_str())
            }
        };
        let current = self.draft.as_ref().map(|d| d.saved.name.clone());
        let value = jobs
            .iter()
            .find(|j| Some(&j.name) == current.as_ref())
            .map(shown)
            .unwrap_or_default();
        let mut picked = None;
        ui.horizontal_wrapped(|ui| {
            let l = ui.label(tr("schedules-job"));
            dropdown(ui, "schedules-job", None, &value, 320.0, |ui| {
                for job in &jobs {
                    if dropdown_choice(ui, Some(&job.name) == current.as_ref(), shown(job))
                        .clicked()
                    {
                        picked = Some(job.name.clone());
                    }
                }
            })
            .labelled_by(l.id);
        });
        if let Some(name) = picked
            && Some(&name) != current.as_ref()
        {
            self.choose(name);
        }
    }

    /// The job's schedule, and each integrity check's, with the editor's picker.
    fn schedules(&mut self, ui: &mut Ui) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        let before = draft.form.clone();
        ui.push_id("schedule", |ui| {
            draft.picker.ui(ui, &mut draft.form.schedule)
        });
        let count = draft.checks.len();
        let pickers = &mut draft.checks;
        for (i, text) in verify_schedules_mut(&mut draft.form).enumerate() {
            let Some(picker) = pickers.get_mut(i) else {
                break;
            };
            ui.add_space(space::M);
            let title = if count > 1 {
                tr!("schedules-check-numbered", n = i + 1)
            } else {
                tr("schedules-check")
            };
            heading(ui, &title);
            ui.label(
                egui::RichText::new(tr("schedules-check-help")).color(theme::of(ui).text_secondary),
            );
            ui.push_id(("check", i), |ui| picker.ui(ui, text));
        }
        if draft.form != before {
            self.errors.clear();
            self.message = None;
        }
    }

    /// When the saved job next runs, as the Jobs table says it, and what runs it.
    fn what_it_means(&mut self, ui: &mut Ui) {
        let Some(draft) = &self.draft else {
            return;
        };
        let app = self
            .settings
            .as_ref()
            .map(|s| s.app.clone())
            .unwrap_or_default();
        ui.add_space(space::S);
        let next = next_run_text(
            &draft.saved,
            &app,
            &self.backend.status(),
            chrono::Utc::now(),
        );
        let note = draft.changed().then(|| tr("schedules-save-to-see"));
        info_row(ui, &tr("schedules-next"), note.as_deref(), |ui| {
            ui.label(next)
        });
        let runs_by = tr(match app.schedule_mode {
            ScheduleMode::Timer => "settings-mode-timer",
            ScheduleMode::TaskScheduler => "settings-mode-tasks",
        });
        info_row(ui, &tr("schedules-runs-by"), None, |ui| ui.label(runs_by));
    }

    /// Why Save didn't save, then Save and Cancel, on only while something's changed.
    fn save_row(&mut self, ui: &mut Ui, editable: bool) {
        for error in &self.errors {
            ui.add_space(space::XS);
            message_bar(ui, InfoKind::Error, error);
        }
        let changed = self.changed();
        ui.add_space(space::S);
        let (mut save, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            save = ui
                .add_enabled_ui(editable && changed, |ui| {
                    primary_button(ui, &tr("button-save"))
                })
                .inner
                .clicked();
            cancel = ui
                .add_enabled_ui(changed, |ui| secondary_button(ui, &tr("button-cancel")))
                .inner
                .clicked();
        });
        if cancel && let Some(d) = &self.draft {
            self.draft = Some(Draft::of(&d.saved.clone()));
            self.errors.clear();
        }
        if save {
            self.save();
        }
    }

    /// Writes the schedules as the editor's Save would, onto the job as it's saved now: only
    /// its schedules change, whatever else was changed elsewhere meanwhile (turned off, its
    /// targets edited). If it's gone, or its after steps changed at all (a check added,
    /// removed, reordered or replaced: which check a schedule was for can't be known), nothing
    /// is saved and it's shown as saved now, with why.
    fn save(&mut self) {
        let Some(draft) = &self.draft else {
            return;
        };
        let now = Settings::load_or_default(&self.backend.settings_file())
            .map_err(|e| e.to_string())
            .and_then(|l| {
                l.settings
                    .jobs
                    .into_iter()
                    .find(|j| j.name.eq_ignore_ascii_case(&draft.saved.name))
                    .ok_or_else(|| tr!("schedules-job-gone", job = draft.saved.name.as_str()))
            });
        let current = match now {
            Ok(job) => job,
            Err(e) => {
                // Above the page: the job shown next is another one.
                self.errors.clear();
                self.message = Some((true, e));
                // The jobs as they are now: the page doesn't show one that's gone.
                self.load();
                return;
            }
        };
        let mut form = JobForm::from_job(&current);
        form.schedule = draft.form.schedule.clone();
        let checks: Vec<String> = verify_schedules(&draft.form).cloned().collect();
        if current.after != draft.saved.after {
            let name = current.name.clone();
            self.errors.clear();
            self.message = Some((true, tr!("schedules-checks-changed", job = name.as_str())));
            self.draft = Some(Draft::of(&current));
            // Read again, so the next frame doesn't take the copy it had for newer.
            self.load();
            return;
        }
        for (text, check) in verify_schedules_mut(&mut form).zip(checks) {
            *text = check;
        }
        match save_job(self.backend.as_ref(), &current, &form) {
            Ok(name) => {
                self.errors.clear();
                self.message = Some((false, tr!("schedules-saved", job = name.as_str())));
                self.request = Some(Request::Saved);
                self.load();
                self.draft = None;
                self.sync_draft();
            }
            Err(errors) => self.errors = errors,
        }
    }

    fn tasks_section(&mut self, ui: &mut Ui, editable: bool) {
        let tasks = match &self.tasks {
            Ok(tasks) => Ok(tasks.clone()),
            Err(e) => Err(e.clone()),
        };
        let none = tasks.as_ref().is_ok_and(Vec::is_empty);
        let mut remove_all = false;
        let mut open_scheduler = false;
        section_card_with_actions(
            ui,
            &tr("tasks-title"),
            Some(&flow(&tr("tasks-help"))),
            |ui| {
                open_scheduler = secondary_button(ui, &tr("tasks-open-scheduler")).clicked();
                remove_all = ui
                    .add_enabled_ui(editable && !none, |ui| {
                        danger_button(ui, None, &tr("tasks-remove-all"))
                    })
                    .inner
                    .clicked();
            },
            |ui| {
                ui.add_space(space::S);
                let tasks = match tasks {
                    Ok(tasks) => tasks,
                    Err(e) => {
                        info_bar(ui, InfoKind::Error, None, &e);
                        Vec::new()
                    }
                };
                self.tasks_table(ui, &tasks, editable);
                if tasks.iter().any(|t| t.orphan) {
                    ui.add_space(space::S);
                    info_bar(ui, InfoKind::Warning, None, &tr("tasks-orphan-help"));
                }
            },
        );
        if remove_all {
            self.confirm = Some(Confirm::DeleteAll);
        }
        if open_scheduler && let Err(e) = self.backend.open_task_scheduler() {
            self.message = Some((true, e));
        }
    }

    fn tasks_table(&mut self, ui: &mut Ui, tasks: &[ScheduledTask], editable: bool) {
        if tasks.is_empty() {
            empty_state(
                ui,
                Icon::Empty,
                &tr("tasks-none-title"),
                &tr("tasks-none"),
                None,
            );
            // Under every table, its pager: "0 of 0" here.
            pager(ui, &mut self.task_table, 0);
            return;
        }
        let columns = [
            // The task's name takes what the others leave; a name that's cut shows whole on
            // hover.
            TableColumn::new(tr("tasks-col-task"), None).at_least(120.0),
            TableColumn::new(tr("tasks-col-job"), Some(130.0)),
            TableColumn::new(tr("tasks-col-trigger"), Some(140.0)),
            TableColumn::new(tr("tasks-col-next"), Some(84.0)),
            TableColumn::new(tr("tasks-col-last"), Some(172.0)),
            TableColumn::new(tr("tasks-col-on"), Some(44.0)),
            TableColumn::new(tr("tasks-col-actions"), Some(64.0)),
        ];
        // The columns' room, scrolled sideways in a narrow window, with a solid scroll bar so
        // it's plain there's more (nothing hidden).
        let width = ui.available_width();
        let mut action = None;
        ui.scope(|ui| {
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::horizontal()
                .id_salt("tasks-scroll")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                .show(ui, |ui| {
                    // At least the columns' width: wider than the window, it scrolls.
                    ui.set_width(width.max(760.0));
                    table_frame(theme::of(ui)).show(ui, |ui| {
                        table(
                            ui,
                            &tr("tasks-title"),
                            &columns,
                            tasks.len(),
                            None,
                            &mut self.task_table,
                            |i| tr!("tasks-row", task = tasks[i].name.as_str()),
                            |_| {},
                            |ui, row, col| {
                                if let Some(a) = task_cell(ui, &tasks[row], col, editable) {
                                    action = Some(a);
                                }
                            },
                        );
                    });
                });
        });
        // Outside the sideways scrolling, so its controls stay in reach in a narrow window.
        pager(ui, &mut self.task_table, tasks.len());
        match action {
            Some(TaskAction::Enable(name, on)) => {
                let result = self.backend.set_task_enabled(&name, on);
                self.message = result.err().map(|e| (true, e));
                self.load();
            }
            Some(TaskAction::Delete(name)) => self.confirm = Some(Confirm::DeleteTask(name)),
            None => {}
        }
    }

    fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let (title, body, choices) = match &confirm {
            Confirm::DeleteTask(name) => (
                tr!("tasks-delete-title", task = name.as_str()),
                tr("tasks-delete-body"),
                vec![("jobs-delete", 1, true), ("button-cancel", 0, false)],
            ),
            Confirm::DeleteAll => (
                tr("tasks-remove-all-title"),
                tr("tasks-remove-all-body"),
                vec![("tasks-remove-all", 1, true), ("button-cancel", 0, false)],
            ),
            // The editor's own question, in its words.
            Confirm::Discard(_) => (
                tr("editor-discard-title"),
                tr("editor-discard-body"),
                vec![
                    ("editor-discard-confirm", 1, true),
                    ("editor-discard-keep", 0, false),
                ],
            ),
        };
        let Some(answer) = ask(ctx, "schedules-confirm", &title, &body, choices) else {
            return;
        };
        self.confirm = None;
        if answer == 0 {
            return;
        }
        match confirm {
            Confirm::DeleteTask(name) => {
                let result = self.backend.delete_task(&name);
                self.message = result.err().map(|e| (true, e));
            }
            Confirm::DeleteAll => {
                let result = self.backend.delete_all_tasks();
                self.message = Some(match result {
                    Ok(n) => (false, tr!("tasks-removed", count = n)),
                    Err(e) => (true, e),
                });
            }
            Confirm::Discard(then) => {
                let saved = self.draft.take().map(|d| d.saved);
                self.errors.clear();
                match then {
                    Then::Choose(name) => self.job = Some(name),
                    Then::EditJob => self.request = saved.map(Request::EditJob),
                }
            }
        }
        self.load();
    }
}

/// What a task row's controls asked for.
enum TaskAction {
    /// Turn the task on (`true`) or off.
    Enable(String, bool),
    Delete(String),
}

/// A task's cell in column `col`: its name, its job (or the orphan's chip), schedule, next and
/// last run, its on/off switch and Delete.
fn task_cell(ui: &mut Ui, task: &ScheduledTask, col: usize, editable: bool) -> Option<TaskAction> {
    // Text that doesn't fit its column is cut with "…", and shown whole on hover.
    let text = |ui: &mut Ui, text: &str| ui.add(egui::Label::new(text).truncate());
    match col {
        0 => {
            path_label(ui, &task.name);
        }
        1 if task.orphan => {
            status_chip(ui, Tone::Warning, &tr("tasks-orphan"))
                .on_hover_text(tr("tasks-orphan-help"));
        }
        1 => {
            text(ui, &task.job);
        }
        2 => {
            text(ui, task.schedule.as_deref().unwrap_or("—"));
        }
        3 => {
            when(ui, task.next_run);
        }
        4 => {
            let (tone, result) = last_result(task);
            status_chip(ui, tone, &result);
            when(ui, task.last_run);
        }
        5 => {
            let mut on = task.enabled;
            let for_key = if task.enabled {
                "tasks-disable-for"
            } else {
                "tasks-enable-for"
            };
            let changed = ui
                .add_enabled_ui(editable, |ui| small_toggle(ui, &mut on))
                .inner
                .accessible_name(tr!(for_key, task = task.name.as_str()))
                .changed();
            if changed {
                return Some(TaskAction::Enable(task.name.clone(), on));
            }
        }
        _ => {
            let clicked = ui
                .add_enabled_ui(editable, |ui| {
                    icon_button(
                        ui,
                        Icon::Delete,
                        &tr!("tasks-delete-for", task = task.name.as_str()),
                    )
                })
                .inner
                .clicked();
            if clicked {
                return Some(TaskAction::Delete(task.name.clone()));
            }
        }
    }
    None
}

/// How a task's last run ended, as a chip: Replica's exit code, or Task Scheduler's own.
fn last_result(task: &ScheduledTask) -> (Tone, String) {
    if task.last_run.is_none() {
        return (Tone::Neutral, tr("result-never-run"));
    }
    match task.last_result {
        Some(0) => (Tone::Ok, outcome_text(Outcome::Ok)),
        Some(1) => (Tone::Failed, outcome_text(Outcome::Failed)),
        Some(2) => (Tone::Warning, outcome_text(Outcome::Partly)),
        Some(3) => (Tone::Warning, outcome_text(Outcome::Skipped)),
        Some(code) => (
            Tone::Neutral,
            tr!("tasks-result-code", code = format!("0x{code:08X}")),
        ),
        None => (Tone::Neutral, tr("result-never-run")),
    }
}

/// A task's time from now ("in 9 h", "2 h ago"), the exact time on hover; a dash if none.
fn when(ui: &mut Ui, at: Option<chrono::NaiveDateTime>) {
    match at {
        Some(at) => {
            let now = chrono::Local::now().naive_local();
            ui.add(egui::Label::new(relative_time(at, now)).truncate())
                .on_hover_text(time_text(Some(at)));
        }
        None => {
            ui.label("—");
        }
    }
}

fn time_text(at: Option<chrono::NaiveDateTime>) -> String {
    at.map_or_else(|| "—".into(), |t| t.format("%Y-%m-%d %H:%M").to_string())
}
