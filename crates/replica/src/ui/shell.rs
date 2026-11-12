//! The window's frame: a menu bar across the top ([`menu`]), a status bar along the bottom
//! ([`status_bar`]), a compact sidebar of the main pages and a slim header over the page shown;
//! going to the tray when it's closed or minimised (with a one-off notice saying so), and
//! remembering where it was.
//!
//! Each page is a module of its own, shown by [`Shell::tab`]: Jobs, History and Restore, in the
//! sidebar; Settings (with the Scheduled-tasks view) and About, from the Tools and Help menus.
//!
//! [`menu`]: super::menu
//! [`status_bar`]: super::status_bar

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, Color32, CornerRadius, Id, Margin, Modal, RichText, Role, Sense, Stroke, Ui, UiBuilder,
    ViewportCommand, vec2,
};
use replica_core::i18n::tr;
use replica_core::settings::Settings;
use replica_core::tr;
use replica_win::window::WindowIcons;

use super::Shared;
use super::about::{AboutTab, PROJECT_URL};
use super::backend::{Backend, Places, UiRequest, change_settings};
use super::geometry::{SavedRect, WindowState};
use super::history::HistoryTab;
use super::icons::Icon;
use super::jobs::JobsTab;
use super::menu::{self, Command, MenuState};
use super::restore::RestoreTab;
use super::settings::{Page, SettingsTab};
use super::status_bar::{self, Facts};
use super::theme::{self, CONTROL_RADIUS, MAX_CONTENT_WIDTH, space};
use super::widgets::{dialog_frame, flow, heading, option_info};

/// How long the window's size and place must stay put before they're saved.
const SETTLE: Duration = Duration::from_secs(1);

/// How often the frame looks for a changed settings file (the status bar's schedules, and
/// whether it shows), as the Jobs page does.
const RECHECK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Jobs,
    History,
    Restore,
    Settings,
    About,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Jobs,
        Tab::History,
        Tab::Restore,
        Tab::Settings,
        Tab::About,
    ];

    /// The pages in the sidebar. Settings and About are reached from the menus.
    pub const SIDEBAR: [Tab; 3] = [Tab::Jobs, Tab::History, Tab::Restore];

    /// Its name, in the sidebar and as its page's title.
    pub fn title(self) -> String {
        tr(match self {
            Tab::Jobs => "tab-jobs",
            Tab::History => "tab-history",
            Tab::Restore => "tab-restore",
            Tab::Settings => "tab-settings",
            Tab::About => "tab-about",
        })
    }

    /// The line under its title.
    pub fn subtitle(self) -> String {
        tr(match self {
            Tab::Jobs => "page-jobs-subtitle",
            Tab::History => "page-history-subtitle",
            Tab::Restore => "page-restore-subtitle",
            Tab::Settings => "page-settings-subtitle",
            Tab::About => "page-about-subtitle",
        })
    }

    pub fn icon(self) -> Icon {
        match self {
            Tab::Jobs => Icon::Jobs,
            Tab::History => Icon::History,
            Tab::Restore => Icon::Restore,
            Tab::Settings => Icon::Settings,
            Tab::About => Icon::Info,
        }
    }
}

/// The sidebar's width, a narrower one for a window under [`NARROW`] wide, and only its
/// icons (a rail) under [`RAIL`] wide, which leaves the page room at 800 points.
const SIDEBAR_WIDTH: f32 = 176.0;
const SIDEBAR_NARROW: f32 = 156.0;
const SIDEBAR_RAIL: f32 = 44.0;
const NARROW: f32 = 1000.0;
const RAIL: f32 = 880.0;

/// The sidebar's rows, the gap between them, its app icon and its text, in points.
const NAV_ITEM_HEIGHT: f32 = 32.0;
const NAV_ITEM_GAP: f32 = 2.0;
const NAV_ICON_SIZE: f32 = 16.0;
const APP_ICON_SIZE: f32 = 16.0;
const NAV_TEXT_SIZE: f32 = 13.0;

/// The room round the page, slimmer than the pages' own spacing: the frame is dense, as a
/// desktop tool's is. The title's size is the theme's ([`theme::TITLE_SIZE`]).
const PAGE_TOP: f32 = 14.0;
const PAGE_SIDE: f32 = 20.0;
const UNDER_HEADER: f32 = 14.0;

pub struct Shell {
    shared: Arc<Shared>,
    backend: Arc<dyn Backend>,
    /// Where Replica keeps its files: File's folders, and the status bar's "Portable".
    places: Places,
    /// The settings as last read, for the menus and the status bar.
    loaded: Result<Settings, String>,
    stamp: Option<SystemTime>,
    checked_at: Option<Instant>,
    /// The status bar turned on or off here when that couldn't be saved.
    status_bar_here: Option<bool>,
    /// A menu command that failed, and why: shown until it's closed.
    failed: Option<String>,
    pub tab: Tab,
    pub jobs: JobsTab,
    pub history: HistoryTab,
    pub restore: RestoreTab,
    pub settings: SettingsTab,
    pub about: AboutTab,
    state: WindowState,
    state_file: PathBuf,
    writable: bool,
    /// The tray notice is up, with its "Don't show again" box.
    notice: Option<bool>,
    /// It has been shown since the app started.
    notice_shown: bool,
    /// The size or place changed and isn't saved yet: since when.
    moved_at: Option<Instant>,
    hidden: bool,
    /// Shown from the tray and not yet seen restored (see [`Shell::logic`]).
    restoring: bool,
    _icons: Option<WindowIcons>,
}

impl Shell {
    pub fn new(
        backend: Arc<dyn Backend>,
        shared: Arc<Shared>,
        state: WindowState,
        icons: Option<WindowIcons>,
    ) -> Self {
        let mut shell = Self {
            shared,
            places: backend.places(),
            loaded: Ok(Settings::default()),
            stamp: None,
            checked_at: None,
            status_bar_here: None,
            failed: None,
            tab: Tab::Jobs,
            jobs: JobsTab::new(Arc::clone(&backend)),
            history: HistoryTab::new(Arc::clone(&backend)),
            restore: RestoreTab::new(Arc::clone(&backend)),
            settings: SettingsTab::new(Arc::clone(&backend)),
            about: AboutTab::new(Arc::clone(&backend)),
            state,
            state_file: backend.window_state_file(),
            writable: backend.writable(),
            notice: None,
            notice_shown: false,
            moved_at: None,
            hidden: false,
            restoring: false,
            _icons: icons,
            backend,
        };
        shell.load();
        shell
    }

    /// The menu bar, the status bar, the sidebar, the page shown under its header, and the
    /// tray notice or a failed command's message if one is up.
    pub fn show(&mut self, ui: &mut Ui) {
        self.reload_if_changed();
        let p = theme::of(ui);
        let ctx = ui.ctx().clone();
        let state = self.menu_state();
        if let Some(command) = menu::shortcuts(&ctx, &state) {
            self.apply(command, &ctx);
        }
        let chosen = egui::Panel::top("menu-bar")
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.card)
                    .inner_margin(Margin::symmetric(space::XS as i8, 2)),
            )
            .show(ui, |ui| {
                let chosen = menu::menu_bar(ui, &state);
                hairline(ui, ui.max_rect().bottom() + 2.0);
                chosen
            })
            .inner;
        if let Some(command) = chosen {
            self.apply(command, &ctx);
        }
        if self.status_bar_shown() {
            let facts = Facts::of(
                self.jobs(),
                &self.app(),
                &self.backend.status(),
                self.places.portable,
                chrono::Utc::now(),
            );
            let clicked = egui::Panel::bottom("status-bar")
                .resizable(false)
                .exact_size(status_bar::HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(p.card)
                        .inner_margin(Margin::symmetric(space::S as i8, 0)),
                )
                .show(ui, |ui| {
                    hairline(ui, ui.max_rect().top());
                    status_bar::show(ui, &facts)
                })
                .inner;
            if clicked {
                self.show_problems(facts.problems.only);
            }
        }
        let window = ui.available_width();
        let rail = window < RAIL;
        let width = if rail {
            SIDEBAR_RAIL
        } else if window < NARROW {
            SIDEBAR_NARROW
        } else {
            SIDEBAR_WIDTH
        };
        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(width)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.background)
                    .inner_margin(Margin::symmetric(6, space::S as i8)),
            )
            .show(ui, |ui| {
                self.sidebar(ui, rail);
                let x = ui.max_rect().right() + 6.0;
                ui.painter().vline(
                    x - 0.5,
                    ui.clip_rect().y_range(),
                    Stroke::new(1.0, frame_line(p)),
                );
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.background).inner_margin(Margin {
                left: PAGE_SIDE as i8,
                right: PAGE_SIDE as i8,
                top: PAGE_TOP as i8,
                bottom: 0,
            }))
            .show(ui, |ui| {
                // The page's widest.
                ui.set_max_width(ui.available_width().min(MAX_CONTENT_WIDTH));
                // The job editor has a header of its own, with the way back to Jobs.
                if !(self.tab == Tab::Jobs && self.jobs.editing()) {
                    self.header(ui);
                }
                self.tab(ui);
            });
        self.tray_notice(&ctx);
        self.failed_message(&ctx);
        theme::paint_focus(&ctx);
    }

    /// The app's icon and name, then a row for each page in [`Tab::SIDEBAR`], the one shown
    /// marked by the accent's bar. As a `rail`, only the icons, with each page's name in its
    /// hover text. A screen reader reads it as a tab list, with the pages' names either way.
    fn sidebar(&mut self, ui: &mut Ui, rail: bool) {
        let p = theme::of(ui);
        let (header, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), NAV_ITEM_HEIGHT), Sense::hover());
        let icon_x = |rect: egui::Rect| {
            if rail {
                rect.center().x
            } else {
                rect.left() + space::S + NAV_ICON_SIZE / 2.0
            }
        };
        let icon = egui::Rect::from_center_size(
            egui::pos2(icon_x(header), header.center().y),
            vec2(APP_ICON_SIZE, APP_ICON_SIZE),
        );
        let texture = super::app_icon(ui.ctx(), APP_ICON_SIZE);
        ui.painter().image(
            texture,
            icon,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        if !rail {
            ui.painter().text(
                egui::pos2(icon.right() + space::S, header.center().y),
                egui::Align2::LEFT_CENTER,
                tr("app-name"),
                theme::semibold(NAV_TEXT_SIZE),
                p.text,
            );
        }
        ui.add_space(space::S);
        let id = ui.scope_id().with("pages");
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(Role::TabList);
            node.set_label(tr("nav-pages"));
        });
        ui.scope_builder(UiBuilder::new().accessibility_parent(id), |ui| {
            ui.spacing_mut().item_spacing.y = NAV_ITEM_GAP;
            for tab in Tab::SIDEBAR {
                let selected = self.tab == tab;
                let title = tab.title();
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), NAV_ITEM_HEIGHT),
                    Sense::click(),
                );
                option_info(&response, Role::Tab, selected, &title);
                if response.clicked() {
                    self.tab = tab;
                }
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let fill = if selected || response.is_pointer_button_down_on() {
                    p.subtle_selected
                } else if response.hovered() {
                    p.subtle_hover
                } else {
                    Color32::TRANSPARENT
                };
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(CONTROL_RADIUS), fill);
                if selected {
                    let bar = egui::Rect::from_center_size(
                        egui::pos2(rect.left() + 1.5, rect.center().y),
                        vec2(3.0, 14.0),
                    );
                    painter.rect_filled(bar, CornerRadius::same(2), p.accent);
                }
                let icon = egui::Rect::from_center_size(
                    egui::pos2(icon_x(rect), rect.center().y),
                    vec2(NAV_ICON_SIZE, NAV_ICON_SIZE),
                );
                tab.icon().paint(painter, icon, p.text);
                if rail {
                    response.on_hover_text(title);
                } else {
                    painter.text(
                        egui::pos2(icon.right() + space::S, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        title,
                        egui::FontId::proportional(NAV_TEXT_SIZE),
                        p.text,
                    );
                }
            }
        });
    }

    /// The page's title, with the line saying what it's for beside it in secondary text.
    fn header(&self, ui: &mut Ui) {
        let p = theme::of(ui);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::M;
            let title = ui.label(
                RichText::new(self.tab.title())
                    .text_style(egui::TextStyle::Heading)
                    .color(p.text),
            );
            ui.ctx()
                .accesskit_node_builder(title.id, |node| node.set_role(Role::Heading));
            ui.label(
                RichText::new(self.tab.subtitle())
                    .size(theme::SMALL_SIZE + 1.0)
                    .color(p.text_secondary),
            );
        });
        ui.add_space(UNDER_HEADER);
    }

    // --- The settings the frame reads ---------------------------------------------------

    fn load(&mut self) {
        let path = self.backend.settings_file();
        self.stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        self.checked_at = Some(Instant::now());
        self.loaded = Settings::load_or_default(&path)
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

    /// The jobs as the frame last read them: fresh whichever page shows (the Jobs page reads
    /// its own only while it shows).
    fn jobs(&self) -> &[replica_core::job::Job] {
        self.loaded.as_ref().map_or(&[], |s| &s.jobs)
    }

    fn app(&self) -> replica_core::settings::AppSettings {
        self.loaded
            .as_ref()
            .map(|s| s.app.clone())
            .unwrap_or_default()
    }

    /// Whether the status bar shows: as the settings say, unless turned here and not saved.
    pub fn status_bar_shown(&self) -> bool {
        self.status_bar_here
            .unwrap_or_else(|| self.loaded.as_ref().map_or(true, |s| s.app.show_status_bar))
    }

    // --- The menus' commands --------------------------------------------------------------

    fn menu_state(&self) -> MenuState {
        let editing = self.jobs.editing();
        let jobs = self.jobs();
        let status = self.backend.status();
        MenuState {
            tab: self.tab,
            status_bar: self.status_bar_shown(),
            can_add: self.writable && self.loaded.is_ok() && !editing,
            can_export: !jobs.is_empty() && !editing,
            can_run_all: jobs.iter().any(|j| j.enabled),
            can_stop: status.running.is_some() || !status.queued.is_empty(),
        }
    }

    /// Does what a menu item or shortcut asked, if it still applies.
    pub fn apply(&mut self, command: Command, ctx: &egui::Context) {
        if !self.menu_state().applies(command) {
            return;
        }
        tracing::info!(?command, "a menu command");
        match command {
            Command::NewJob => {
                self.tab = Tab::Jobs;
                self.jobs.edit(None);
            }
            Command::ImportJobs => {
                self.tab = Tab::Jobs;
                self.jobs.open_import();
            }
            Command::ExportJobs => {
                self.tab = Tab::Jobs;
                self.jobs.open_export(None);
            }
            Command::OpenAppFolder => {
                let folder = self.places.folder.clone();
                self.open(&folder);
            }
            Command::OpenLogFolder => {
                let folder = self.places.logs.clone();
                self.open(&folder);
            }
            Command::CloseWindow => self.send_to_tray(ctx),
            Command::Exit => {
                self.save_state();
                self.backend.send(UiRequest::Quit);
            }
            Command::RunAll => {
                let enabled = self
                    .jobs()
                    .iter()
                    .filter(|j| j.enabled)
                    .map(|j| j.name.clone())
                    .collect();
                self.backend.send(UiRequest::Run(enabled));
            }
            Command::Stop => self.backend.send(UiRequest::Stop),
            Command::Show(tab) => self.tab = tab,
            Command::ToggleStatusBar => self.toggle_status_bar(),
            Command::Settings => self.tab = Tab::Settings,
            Command::ScheduledTasks => {
                self.tab = Tab::Settings;
                self.settings.page = Page::Schedules;
            }
            Command::TaskScheduler => {
                self.failed = self.backend.open_task_scheduler().err();
            }
            Command::ProjectPage => self.failed = self.backend.open_url(PROJECT_URL).err(),
            Command::About => self.tab = Tab::About,
        }
    }

    /// Opens a folder in Explorer, or says why it couldn't.
    fn open(&mut self, folder: &Path) {
        self.failed = self.backend.open_path(folder).err();
    }

    /// Turns the status bar on or off and saves that, or keeps it for this run if it can't be
    /// saved (a read-only portable copy).
    fn toggle_status_bar(&mut self) {
        let show = !self.status_bar_shown();
        if !self.writable || self.loaded.is_err() {
            self.status_bar_here = Some(show);
            return;
        }
        match change_settings(self.backend.as_ref(), |s| s.app.show_status_bar = show) {
            Ok(_) => self.status_bar_here = None,
            Err(e) => {
                self.status_bar_here = Some(show);
                self.failed = Some(e);
            }
        }
        self.load();
    }

    /// History, with the one job that has a problem chosen, or every job.
    fn show_problems(&mut self, only: Option<String>) {
        self.tab = Tab::History;
        self.history.job = only;
        self.history.result = None;
        self.history.load();
    }

    /// A command that failed: why, until it's closed.
    fn failed_message(&mut self, ctx: &egui::Context) {
        let Some(error) = &self.failed else {
            return;
        };
        let mut ok = false;
        Modal::new(Id::unique("menu-failed"))
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                heading(ui, &tr("menu-failed-title"));
                ui.add_space(6.0);
                ui.label(flow(&tr!("menu-failed", error = error.as_str())));
                ui.add_space(8.0);
                ok = ui.button(tr("button-ok")).clicked();
            });
        if ok {
            self.failed = None;
        }
    }

    fn tab(&mut self, ui: &mut Ui) {
        match self.tab {
            Tab::Jobs => self.jobs.ui(ui),
            Tab::History => self.history.ui(ui),
            Tab::Restore => self.restore.ui(ui),
            Tab::Settings => self.settings.ui(ui),
            Tab::About => self.about.ui(ui),
        }
    }

    /// Sends the window to the tray: first says so, once, unless that was turned off.
    fn send_to_tray(&mut self, ctx: &egui::Context) {
        if !self.state.tray_notice_off && !self.notice_shown {
            self.notice_shown = true;
            self.notice = Some(false);
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            return;
        }
        self.hide(ctx);
    }

    fn hide(&mut self, ctx: &egui::Context) {
        tracing::info!("the window went to the tray");
        self.save_state();
        self.hidden = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn tray_notice(&mut self, ctx: &egui::Context) {
        let Some(mut dont_show) = self.notice else {
            return;
        };
        let mut ok = false;
        Modal::new(Id::unique("tray-notice"))
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                heading(ui, &tr("tray-notice-title"));
                ui.add_space(6.0);
                ui.label(flow(&tr("tray-notice-body")));
                ui.add_space(8.0);
                ui.checkbox(&mut dont_show, tr("tray-notice-dont-show"));
                ui.add_space(8.0);
                ok = ui.button(tr("button-ok")).clicked();
            });
        self.notice = Some(dont_show);
        if ok {
            self.notice = None;
            self.state.tray_notice_off = dont_show;
            self.hide(ctx);
        }
    }

    /// Notes the window's size and place, to save once it settles.
    fn track(&mut self, ctx: &egui::Context) {
        let (outer, inner, maximized, minimized, ppp) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.maximized.unwrap_or(false),
                v.minimized.unwrap_or(false),
                i.pixels_per_point,
            )
        });
        if minimized || self.hidden {
            return;
        }
        let rect = match (outer, inner) {
            (Some(outer), Some(inner)) if !maximized => Some(SavedRect {
                x: outer.min.x,
                y: outer.min.y,
                width: inner.width(),
                height: inner.height(),
                pixels_per_point: ppp,
            }),
            _ => self.state.rect,
        };
        if rect != self.state.rect || maximized != self.state.maximized {
            self.state.rect = rect;
            self.state.maximized = maximized;
            self.moved_at = Some(Instant::now());
        }
        if self.moved_at.is_some_and(|t| t.elapsed() >= SETTLE) {
            self.save_state();
        } else if self.moved_at.is_some() {
            ctx.request_repaint_after(SETTLE);
        }
    }

    fn save_state(&mut self) {
        self.moved_at = None;
        if !self.writable {
            return;
        }
        if let Err(error) = self.state.save(&self.state_file) {
            tracing::warn!(%error, "couldn't save the window's state");
        }
    }

    /// Between frames, and while hidden: show when the tray asks, go to the tray when the
    /// window is closed or minimised.
    pub fn logic(&mut self, ctx: &egui::Context) {
        if self.shared.show.swap(false, Ordering::SeqCst) {
            tracing::info!("the window is showing");
            self.hidden = false;
            self.restoring = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            ctx.request_repaint();
        }
        let (close, minimized) = ctx.input(|i| {
            (
                i.viewport().close_requested(),
                i.viewport().minimized.unwrap_or(false),
            )
        });
        // A window shown from the tray is still minimised for a moment while it restores.
        if self.restoring && !minimized {
            self.restoring = false;
        }
        if close {
            tracing::info!("the window was closed");
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.send_to_tray(ctx);
        } else if minimized && !self.hidden && !self.restoring && self.notice.is_none() {
            tracing::info!("the window was minimised");
            self.send_to_tray(ctx);
        }
        self.track(ctx);
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        Shell::logic(self, ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// A hairline across the frame at `y`: under the menu bar, over the status bar.
fn hairline(ui: &Ui, y: f32) {
    let p = theme::of(ui);
    let x = ui.ctx().content_rect().x_range();
    ui.painter().with_clip_rect(ui.ctx().content_rect()).hline(
        x,
        y - 0.5,
        Stroke::new(1.0, frame_line(p)),
    );
}

/// The frame's hairlines: between the bars, the sidebar and the page.
fn frame_line(p: &theme::Palette) -> Color32 {
    status_bar::separator(p)
}
