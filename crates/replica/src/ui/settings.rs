//! The Settings tab, in tabs of its own so none is a long page to scroll:
//! - Schedules: what runs them (the built-in timer, or Task Scheduler, off by default),
//!   pausing them, and the Scheduled-tasks view (nothing hidden, by design, so it's
//!   there whatever runs them): every task in Replica's Task Scheduler folder, orphans flagged,
//!   each turned on or off or deleted, all removed, and Windows' own Task Scheduler a click
//!   away;
//! - General: Start with Windows, how long run logs are kept, and resetting the settings;
//! - restic: which restic runs;
//! - Storage: where Replica keeps its files.
//!
//! Every change is saved to `settings.toml` at once and the tray told
//! ([`change_settings`]): it brings the tasks and Start with Windows in line.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Id, Modal, RichText, Ui, vec2};
use replica_core::i18n::tr;
use replica_core::run::Outcome;
use replica_core::settings::{AppSettings, ScheduleMode, Settings};
use replica_core::tr;
use replica_win::scheduler::ScheduledTask;

use super::backend::{Backend, Places, change_settings};
use super::icons::Icon;
use super::jobs::outcome_text;
use super::restore::{Load, spawn};
use super::theme::{self, CONTROL_HEIGHT, space};
use super::widgets::{
    InfoKind, TableColumn, TableState, Tone, danger_button, danger_button_filled, dialog_frame,
    empty_state, flow, heading, info_bar, info_row, path_row, plain_card, primary_button,
    relative_time, secondary_button, section_card, section_card_with_actions, segmented,
    setting_row, status_chip, tab_strip, table, toggle,
};

/// How often it reads the settings and the tasks again while shown.
const RELOAD: Duration = Duration::from_secs(3);

/// A restic's path and version.
type ResticInUse = (PathBuf, String);

/// A question the user has to answer first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Turning Task Scheduler off: delete the tasks, or keep them turned off?
    TurnOff,
    DeleteTask(String),
    DeleteAll,
    Reset,
}

/// Settings' own tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Schedules,
    General,
    Restic,
    Storage,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Schedules, Page::General, Page::Restic, Page::Storage];

    pub fn title(self) -> String {
        tr(match self {
            Page::Schedules => "settings-tab-schedules",
            Page::General => "settings-tab-general",
            Page::Restic => "settings-tab-restic",
            Page::Storage => "settings-tab-storage",
        })
    }
}

pub struct SettingsTab {
    /// The tab shown, kept while the app runs.
    pub page: Page,
    backend: Arc<dyn Backend>,
    pub settings: Result<Settings, String>,
    places: Places,
    pub tasks: Result<Vec<ScheduledTask>, String>,
    loaded_at: Option<Instant>,
    /// The restic path being typed (empty: the bundled one).
    restic_text: String,
    /// The restic found for the saved setting, and its version.
    restic: Option<(Option<PathBuf>, Load<ResticInUse>)>,
    pub confirm: Option<Confirm>,
    message: Option<(bool, String)>,
    /// The run-log days while their control is being dragged (saved on release).
    log_days: Option<u32>,
    task_table: TableState,
}

impl SettingsTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let places = backend.places();
        Self {
            page: Page::default(),
            backend,
            settings: Ok(Settings::default()),
            places,
            tasks: Ok(Vec::new()),
            loaded_at: None,
            restic_text: String::new(),
            restic: None,
            confirm: None,
            message: None,
            log_days: None,
            task_table: TableState::default(),
        }
    }

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
        if let Ok(s) = &self.settings
            && self.restic.is_none()
        {
            self.restic_text = s
                .app
                .restic_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
    }

    fn app(&self) -> AppSettings {
        self.settings
            .as_ref()
            .map(|s| s.app.clone())
            .unwrap_or_default()
    }

    /// Saves a change to the app's settings, and says so if it can't.
    fn change(&mut self, edit: impl FnOnce(&mut AppSettings)) {
        match change_settings(self.backend.as_ref(), |s| edit(&mut s.app)) {
            Ok(_) => self.message = None,
            Err(e) => self.message = Some((true, e)),
        }
        self.load();
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        if self.loaded_at.is_none_or(|t| t.elapsed() >= RELOAD) {
            self.load();
        }
        ui.ctx().request_repaint_after(RELOAD);
        let titles = Page::ALL.map(Page::title);
        let mut chosen = Page::ALL.iter().position(|p| *p == self.page).unwrap_or(0);
        if tab_strip(
            ui,
            &tr("settings-tabs"),
            &titles.each_ref().map(String::as_str),
            &mut chosen,
        ) {
            self.page = Page::ALL[chosen];
        }
        ui.add_space(space::L);
        if let Some((error, text)) = &self.message {
            let kind = if *error {
                InfoKind::Error
            } else {
                InfoKind::Info
            };
            info_bar(ui, kind, None, text);
            ui.add_space(space::M);
        }
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
        let editable = self.places.writable;
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
        // Each tab fits the window at its default size; a narrow or short one scrolls (and so
        // does Schedules with many tasks), each tab keeping its own place.
        egui::ScrollArea::vertical()
            .id_salt(("settings-page", self.page as u8))
            .auto_shrink([false, false])
            .show(ui, |ui| match self.page {
                Page::Schedules => {
                    ui.add_enabled_ui(editable, |ui| self.schedules(ui));
                    self.tasks_section(ui, editable);
                }
                Page::General => {
                    ui.add_enabled_ui(editable, |ui| self.general(ui));
                    self.reset_section(ui, editable);
                }
                Page::Restic => {
                    ui.add_enabled_ui(editable, |ui| self.restic_section(ui));
                }
                Page::Storage => self.places_section(ui),
            });
        self.confirm_window(ui.ctx());
    }

    fn reset_section(&mut self, ui: &mut Ui, editable: bool) {
        section_card(
            ui,
            &tr("settings-reset-title"),
            Some(&tr("settings-reset-help")),
            |ui| {
                if ui
                    .add_enabled_ui(editable, |ui| {
                        danger_button(ui, None, &tr("settings-reset"))
                    })
                    .inner
                    .clicked()
                {
                    self.confirm = Some(Confirm::Reset);
                }
            },
        );
    }

    fn schedules(&mut self, ui: &mut Ui) {
        let app = self.app();
        // Untitled: "Schedules" would only repeat its tab.
        plain_card(ui, None, |ui| {
            let timer = app.schedule_mode == ScheduleMode::Timer;
            let modes = [tr("settings-mode-timer"), tr("settings-mode-tasks")];
            // The chosen mode's line, under the row's label.
            let help = tr(if timer {
                "settings-mode-timer-help"
            } else {
                "settings-mode-tasks-help"
            });
            let mut chosen = None;
            setting_row(ui, &tr("settings-mode"), Some(&help), |ui| {
                let (group, clicked) =
                    segmented(ui, &[&modes[0], &modes[1]], if timer { 0 } else { 1 });
                chosen = clicked;
                group
            });
            match chosen {
                // Turning Task Scheduler off asks first what to do with its tasks.
                Some(0) => self.confirm = Some(Confirm::TurnOff),
                Some(_) => self.change(|a| a.schedule_mode = ScheduleMode::TaskScheduler),
                None => {}
            }
            let mut paused = app.pause_schedules;
            if setting_row(
                ui,
                &tr("tray-pause-schedules"),
                Some(&tr("settings-pause-help")),
                |ui| toggle(ui, &mut paused),
            )
            .changed()
            {
                self.change(|a| a.pause_schedules = paused);
            }
            ui.add_space(space::S);
            info_bar(ui, InfoKind::Info, None, &flow(&tr("settings-mode-help")));
        });
    }

    fn general(&mut self, ui: &mut Ui) {
        let app = self.app();
        // Untitled: "General" would only repeat its tab.
        plain_card(ui, None, |ui| {
            let mut autostart = app.start_with_windows;
            if setting_row(
                ui,
                &tr("tray-start-with-windows"),
                Some(&tr("settings-start-with-windows-help")),
                |ui| toggle(ui, &mut autostart),
            )
            .changed()
            {
                self.change(|a| a.start_with_windows = autostart);
            }
            // While it's dragged, the value being dragged; saved when it's let go.
            let mut days = self.log_days.unwrap_or(app.log_retention_days);
            let r = setting_row(
                ui,
                &tr("settings-log-days-label"),
                Some(&tr("settings-log-days-help")),
                |ui| {
                    ui.label(
                        RichText::new(tr("settings-days-unit")).color(theme::of(ui).text_secondary),
                    );
                    ui.add(egui::DragValue::new(&mut days).range(1..=3650))
                },
            )
            // The unit is in its name: it's beside the number, not in the label.
            .accessible_name(tr("settings-log-days"));
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.log_days = None;
                self.change(|a| a.log_retention_days = days);
            } else if r.dragged() {
                self.log_days = Some(days);
            }
        });
    }

    fn restic_section(&mut self, ui: &mut Ui) {
        let saved = self.app().restic_path;
        // Which restic runs now (as saved), found off the UI thread.
        if self
            .restic
            .as_ref()
            .is_none_or(|(for_path, _)| *for_path != saved)
        {
            let path = saved.clone();
            let load = spawn(ui.ctx(), &self.backend, move |b| b.restic_in_use(path));
            self.restic = Some((saved.clone(), load));
        }
        // Untitled: "restic" would only repeat its tab.
        plain_card(ui, None, |ui| {
            if let Some((_, load)) = &mut self.restic {
                load.poll();
                let source = tr(if saved.is_some() {
                    "settings-restic-in-use-own"
                } else {
                    "settings-restic-in-use-bundled"
                });
                match load {
                    Load::Loading(_) => {
                        // The same two rows as once it's found, so what's below doesn't move
                        // when it is (a click aimed at it would miss).
                        info_row(ui, &tr("settings-restic-finding"), None, |ui| ui.spinner());
                        info_row(ui, &tr("settings-restic-file"), None, |ui| ui.spinner());
                    }
                    Load::Ready((path, version)) => {
                        let version = tr!("settings-restic-version", version = version.as_str());
                        info_row(ui, &source, None, |ui| status_chip(ui, Tone::Ok, &version));
                        path_row(
                            ui,
                            &tr("settings-restic-file"),
                            None,
                            &path.display().to_string(),
                        );
                    }
                    Load::Failed(e) => {
                        info_bar(ui, InfoKind::Error, None, e);
                    }
                }
            }
            setting_row(
                ui,
                &tr("settings-restic-path"),
                Some(&tr("settings-restic-path-hint")),
                |ui| {
                    let browse = secondary_button(ui, &tr("button-browse"));
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let width = (ui.available_width() - space::L).clamp(120.0, 360.0);
                    let text = ui.add(
                        egui::TextEdit::singleline(&mut self.restic_text)
                            .font(font)
                            .desired_width(width)
                            .min_size(vec2(0.0, CONTROL_HEIGHT - 4.0)),
                    );
                    if browse.clicked()
                        && let Some(file) = self.backend.pick_file()
                    {
                        self.restic_text = file.to_string_lossy().into_owned();
                    }
                    text
                },
            );
            let typed =
                Some(PathBuf::from(self.restic_text.trim())).filter(|p| !p.as_os_str().is_empty());
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled_ui(typed != saved, |ui| primary_button(ui, &tr("button-save")))
                    .inner
                    .clicked()
                {
                    let path = typed.clone();
                    self.change(|a| a.restic_path = path);
                }
                if ui
                    .add_enabled_ui(saved.is_some(), |ui| {
                        secondary_button(ui, &tr("settings-restic-bundled"))
                    })
                    .inner
                    .clicked()
                {
                    self.restic_text.clear();
                    self.change(|a| a.restic_path = None);
                }
            });
        });
    }

    fn places_section(&mut self, ui: &mut Ui) {
        let p = self.places.clone();
        let mut open = None;
        section_card_with_actions(
            ui,
            &tr("where-title"),
            None,
            |ui| {
                if secondary_button(ui, &tr("settings-open-logs")).clicked() {
                    open = Some(p.logs.clone());
                }
                if secondary_button(ui, &tr("settings-open-folder")).clicked() {
                    open = Some(p.folder.clone());
                }
            },
            |ui| {
                let mode = tr(if p.portable {
                    "where-mode-portable"
                } else {
                    "where-mode-installed"
                });
                info_row(ui, &tr("where-mode"), None, |ui| {
                    status_chip(ui, Tone::Info, &mode)
                });
                let folder_note = (!p.writable).then(|| tr("where-not-writable"));
                path_row(
                    ui,
                    &tr("where-folder"),
                    folder_note.as_deref(),
                    &p.folder.display().to_string(),
                );
                for (key, path) in [
                    ("where-settings-file", &p.settings),
                    ("where-history", &p.history),
                    ("where-logs", &p.logs),
                    ("where-run-logs", &p.run_logs),
                ] {
                    path_row(ui, &tr(key), None, &path.display().to_string());
                }
            },
        );
        if let Some(path) = open {
            self.open(&path);
        }
    }

    fn open(&mut self, path: &Path) {
        if let Err(e) = self.backend.open_path(path) {
            self.message = Some((true, e));
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
            return;
        }
        let columns = [
            // The schedule gets its room first; a name that's cut shows whole on hover.
            TableColumn::new(tr("tasks-col-task"), Some(90.0)),
            TableColumn::new(tr("tasks-col-job"), Some(150.0)),
            TableColumn::new(tr("tasks-col-trigger"), Some(150.0)),
            TableColumn::new(tr("tasks-col-next"), Some(90.0)),
            TableColumn::new(tr("tasks-col-last"), Some(160.0)),
            TableColumn::new(tr("tasks-col-on"), Some(50.0)),
            TableColumn::new(tr("tasks-col-actions"), Some(100.0)),
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
                    ui.set_width(width.max(790.0));
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
        // Each choice: its text, the answer, and whether it's the step that deletes.
        let (title, body, choices): (String, String, Vec<(&str, u8, bool)>) = match &confirm {
            Confirm::TurnOff => (
                tr("tasks-turn-off-title"),
                tr("tasks-turn-off-body"),
                vec![
                    ("tasks-turn-off-delete", 1, true),
                    ("tasks-turn-off-keep", 2, false),
                    ("button-cancel", 0, false),
                ],
            ),
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
            Confirm::Reset => (
                tr("settings-reset-confirm-title"),
                tr("settings-reset-body"),
                vec![("settings-reset", 1, true), ("button-cancel", 0, false)],
            ),
        };
        let mut answer = None;
        Modal::new(Id::unique("settings-confirm"))
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                heading(ui, &title);
                ui.add_space(space::S);
                ui.label(flow(&body));
                ui.add_space(space::L);
                ui.horizontal(|ui| {
                    for (key, value, deletes) in choices {
                        let clicked = if deletes {
                            danger_button_filled(ui, &tr(key))
                        } else {
                            secondary_button(ui, &tr(key))
                        }
                        .clicked();
                        if clicked {
                            answer = Some(value);
                        }
                    }
                });
            });
        let Some(answer) = answer else {
            return;
        };
        self.confirm = None;
        if answer == 0 {
            return;
        }
        match confirm {
            Confirm::TurnOff => self.turn_off(answer == 1),
            Confirm::DeleteTask(name) => {
                let result = self.backend.delete_task(&name);
                self.message = result.err().map(|e| (true, e));
                self.load();
            }
            Confirm::DeleteAll => {
                let result = self.backend.delete_all_tasks();
                self.message = Some(match result {
                    Ok(n) => (false, tr!("tasks-removed", count = n)),
                    Err(e) => (true, e),
                });
                self.load();
            }
            Confirm::Reset => {
                if self.app().schedule_mode == ScheduleMode::TaskScheduler
                    && let Err(e) = self.backend.turn_off_tasks(true)
                {
                    self.message = Some((true, e));
                    return;
                }
                self.restic = None;
                self.change(|a| *a = AppSettings::default());
                if self.message.is_none() {
                    self.message = Some((false, tr("settings-reset-done")));
                }
            }
        }
    }

    /// Task Scheduler off: its tasks deleted (`delete`) or turned off, then the built-in timer.
    fn turn_off(&mut self, delete: bool) {
        if let Err(e) = self.backend.turn_off_tasks(delete) {
            self.message = Some((true, e));
            self.load();
            return;
        }
        self.change(|a| a.schedule_mode = ScheduleMode::Timer);
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
            text(ui, &task.name);
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
                .add_enabled_ui(editable, |ui| toggle(ui, &mut on))
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
                    danger_button(ui, Some(Icon::Delete), &tr("jobs-delete"))
                })
                .inner
                .accessible_name(tr!("tasks-delete-for", task = task.name.as_str()))
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
