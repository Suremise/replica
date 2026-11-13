//! The Settings page, in tabs of its own so none is a long page to scroll:
//! - Scheduling: what runs the schedules (the built-in timer, or Task Scheduler, off by
//!   default) and pausing them, with a link to the Schedules page ([`super::schedules`]),
//!   which lists Replica's tasks in Task Scheduler;
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
use replica_core::settings::{AppSettings, ScheduleMode, Settings};
use replica_core::tr;

use super::backend::{Backend, Places, change_settings};
use super::restore::{Load, spawn};
use super::theme::{self, CONTROL_HEIGHT, space};
use super::widgets::{
    InfoKind, Tone, danger_button, danger_button_filled, dialog_frame, flow, heading, info_bar,
    info_row, path_row, plain_card, primary_button, secondary_button, section_card,
    section_card_with_actions, segmented, setting_row, status_chip, tab_strip, toggle,
};

/// How often it reads the settings again while shown.
const RELOAD: Duration = Duration::from_secs(3);

/// A restic's path and version.
type ResticInUse = (PathBuf, String);

/// A question the user has to answer first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Turning Task Scheduler off: delete the tasks, or keep them turned off?
    TurnOff,
    Reset,
}

/// Settings' own tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Scheduling,
    General,
    Restic,
    Storage,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Scheduling, Page::General, Page::Restic, Page::Storage];

    pub fn title(self) -> String {
        tr(match self {
            Page::Scheduling => "settings-tab-scheduling",
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
    loaded_at: Option<Instant>,
    /// The restic path being typed (empty: the bundled one).
    restic_text: String,
    /// The restic found for the saved setting, and its version.
    restic: Option<(Option<PathBuf>, Load<ResticInUse>)>,
    pub confirm: Option<Confirm>,
    message: Option<(bool, String)>,
    /// The run-log days while their control is being dragged (saved on release).
    log_days: Option<u32>,
    /// The link to the Schedules page was followed: the frame shows it.
    pub to_schedules: bool,
}

impl SettingsTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let places = backend.places();
        Self {
            page: Page::default(),
            backend,
            settings: Ok(Settings::default()),
            places,
            loaded_at: None,
            restic_text: String::new(),
            restic: None,
            confirm: None,
            message: None,
            log_days: None,
            to_schedules: false,
        }
    }

    pub fn load(&mut self) {
        self.loaded_at = Some(Instant::now());
        self.settings = Settings::load_or_default(&self.backend.settings_file())
            .map(|l| l.settings)
            .map_err(|e| e.to_string());
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
        // Each tab fits the window at its default size; a narrow or short one scrolls, each tab
        // keeping its own place.
        egui::ScrollArea::vertical()
            .id_salt(("settings-page", self.page as u8))
            .auto_shrink([false, false])
            .show(ui, |ui| match self.page {
                Page::Scheduling => {
                    ui.add_enabled_ui(editable, |ui| self.schedules(ui));
                    // Replica's tasks are a page of their own, a click away.
                    if ui.link(tr("settings-see-schedules")).clicked() {
                        self.to_schedules = true;
                    }
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
        // Untitled: "Scheduling" would only repeat its tab.
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
            let mut updates = app.check_for_updates;
            if setting_row(
                ui,
                &tr("tray-check-for-updates"),
                Some(&tr("settings-check-for-updates-help")),
                |ui| toggle(ui, &mut updates),
            )
            .changed()
            {
                self.change(|a| a.check_for_updates = updates);
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

    fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let (title, body, choices) = match &confirm {
            Confirm::TurnOff => (
                tr("tasks-turn-off-title"),
                tr("tasks-turn-off-body"),
                vec![
                    ("tasks-turn-off-delete", 1, true),
                    ("tasks-turn-off-keep", 2, false),
                    ("button-cancel", 0, false),
                ],
            ),
            Confirm::Reset => (
                tr("settings-reset-confirm-title"),
                tr("settings-reset-body"),
                vec![("settings-reset", 1, true), ("button-cancel", 0, false)],
            ),
        };
        let Some(answer) = ask(ctx, "settings-confirm", &title, &body, choices) else {
            return;
        };
        self.confirm = None;
        if answer == 0 {
            return;
        }
        match confirm {
            Confirm::TurnOff => self.turn_off(answer == 1),
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

/// A question in a dialog: its `title`, its `body`, and a button for each choice (its text's
/// key, the answer it gives, and whether it's the step that deletes, shown as a danger). The
/// answer chosen, once one is.
pub(super) fn ask(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    body: &str,
    choices: Vec<(&str, u8, bool)>,
) -> Option<u8> {
    let mut answer = None;
    Modal::new(Id::unique(id))
        .frame(dialog_frame(ctx))
        .show(ctx, |ui| {
            ui.set_max_width(460.0);
            heading(ui, title);
            ui.add_space(space::S);
            ui.label(flow(body));
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
    answer
}
