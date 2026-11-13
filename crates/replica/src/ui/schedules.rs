//! The Schedules page, in the sidebar: Replica's tasks in Windows Task Scheduler (nothing
//! hidden, by design, so they're listed whatever runs the schedules): every task in its
//! "Replica" folder, orphans flagged, each turned on or off or deleted, all removed, and
//! Windows' own Task Scheduler a click away.
//!
//! What runs the schedules, and pausing them, are options: they stay in Settings, on its
//! Scheduling tab, which links here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Ui};
use replica_core::i18n::tr;
use replica_core::run::Outcome;
use replica_core::settings::Settings;
use replica_core::tr;
use replica_win::scheduler::ScheduledTask;

use super::backend::{Backend, Places};
use super::icons::Icon;
use super::jobs::outcome_text;
use super::settings::ask;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, TableColumn, TableState, Tone, danger_button, empty_state, flow, icon_button,
    info_bar, pager, path_label, relative_time, secondary_button, section_card_with_actions,
    small_toggle, status_chip, table, table_frame,
};

/// How often it reads the settings and the tasks again while shown.
const RELOAD: Duration = Duration::from_secs(3);

/// A question the user has to answer first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    DeleteTask(String),
    DeleteAll,
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
        }
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
            .show(ui, |ui| self.tasks_section(ui, editable));
        self.confirm_window(ui.ctx());
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
