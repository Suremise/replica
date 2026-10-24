//! The window's frame: a sidebar of its pages with a header over the page shown, going
//! to the tray when it's closed or minimised (with a one-off notice saying so), and
//! remembering where it was.
//!
//! Each page is a module of its own, shown by [`Shell::tab`]: Jobs, History, Restore,
//! Settings (with the Scheduled-tasks view) and About.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CornerRadius, Id, Margin, Modal, RichText, Role, Sense, Ui, UiBuilder,
    ViewportCommand, vec2,
};
use replica_core::i18n::tr;
use replica_win::window::WindowIcons;

use super::Shared;
use super::about::AboutTab;
use super::backend::Backend;
use super::geometry::{SavedRect, WindowState};
use super::history::HistoryTab;
use super::icons::Icon;
use super::jobs::JobsTab;
use super::restore::RestoreTab;
use super::settings::SettingsTab;
use super::theme::{self, CONTROL_HEIGHT, CONTROL_RADIUS, MAX_CONTENT_WIDTH, space};
use super::widgets::{dialog_frame, flow, option_info};

/// How long the window's size and place must stay put before they're saved.
const SETTLE: Duration = Duration::from_secs(1);

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
const SIDEBAR_WIDTH: f32 = 240.0;
const SIDEBAR_NARROW: f32 = 200.0;
const SIDEBAR_RAIL: f32 = 56.0;
const NARROW: f32 = 1000.0;
const RAIL: f32 = 880.0;

/// The sidebar's app icon, in points.
const APP_ICON_SIZE: f32 = 20.0;

pub struct Shell {
    shared: Arc<Shared>,
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
        Self {
            shared,
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
        }
    }

    /// The sidebar, the page shown under its header, and the tray notice if it's up.
    pub fn show(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
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
                    .inner_margin(Margin::symmetric(space::S as i8, space::M as i8)),
            )
            .show(ui, |ui| self.sidebar(ui, rail));
        // The page's left margin is the gap from the sidebar: wider beside the full one.
        let left = if window >= NARROW {
            space::SIDEBAR_GAP
        } else {
            space::PAGE_MARGIN
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.background).inner_margin(Margin {
                left: left as i8,
                right: space::PAGE_MARGIN as i8,
                top: space::PAGE_MARGIN as i8,
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
        self.tray_notice(ui.ctx());
        theme::paint_focus(ui.ctx());
    }

    /// The app's icon and name, then a row for each page, the one shown marked by the
    /// accent's bar. As a `rail`, only the icons, with each page's name in its hover text. A
    /// screen reader reads it as a tab list, with the pages' names either way.
    fn sidebar(&mut self, ui: &mut Ui, rail: bool) {
        let p = theme::of(ui);
        ui.add_space(space::XS);
        let (header, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_HEIGHT), Sense::hover());
        let icon = egui::Rect::from_center_size(
            egui::pos2(header.left() + space::M + 8.0, header.center().y),
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
                theme::semibold(theme::SECTION_SIZE),
                p.text,
            );
        }
        ui.add_space(space::M);
        let id = ui.scope_id().with("pages");
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(Role::TabList);
            node.set_label(tr("nav-pages"));
        });
        ui.scope_builder(UiBuilder::new().accessibility_parent(id), |ui| {
            ui.spacing_mut().item_spacing.y = space::NAV_ITEM_GAP;
            for tab in Tab::ALL {
                let selected = self.tab == tab;
                let title = tab.title();
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), space::NAV_ITEM_HEIGHT),
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
                        vec2(3.0, 16.0),
                    );
                    painter.rect_filled(bar, CornerRadius::same(2), p.accent);
                }
                let icon = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + space::M + 8.0, rect.center().y),
                    vec2(16.0, 16.0),
                );
                tab.icon().paint(painter, icon, p.text);
                if rail {
                    response.on_hover_text(title);
                } else {
                    painter.text(
                        egui::pos2(icon.right() + space::M, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        title,
                        egui::FontId::proportional(theme::BODY_SIZE),
                        p.text,
                    );
                }
            }
        });
    }

    /// The page's title and the line under it.
    fn header(&self, ui: &mut Ui) {
        let p = theme::of(ui);
        ui.heading(self.tab.title());
        ui.label(RichText::new(self.tab.subtitle()).color(p.text_secondary));
        ui.add_space(space::UNDER_HEADER);
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
                ui.heading(tr("tray-notice-title"));
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
