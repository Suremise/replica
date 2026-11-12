//! The window's menu bar (File, Jobs, View, Tools, Help) and its keyboard shortcuts.
//!
//! Every item is a command the window already had somewhere (a page's button, the tray's menu):
//! the menu only gathers them, each with its shortcut shown, and greyed out when it doesn't
//! apply. It gives back the [`Command`] chosen; [`super::shell::Shell`] does it.
//!
//! egui's menu bar opens its menus with the mouse or with Tab and Enter; it has no Alt or F10
//! to the bar and no arrow keys between menus, so neither is offered here. The shortcuts
//! ([`shortcuts`]) are the keyboard's way to the commands.

use eframe::egui::{self, Button, Key, KeyboardShortcut, Modifiers, Response, Role, Ui, UiBuilder};
use replica_core::i18n::tr;

use super::icons::Icon;
use super::shell::Tab;
use super::theme;

/// The menus' text, a size under the pages' body text: dense, as a desktop app's menus are.
pub const MENU_TEXT_SIZE: f32 = 13.0;

/// The menus' gutter, left of each item's name: where a tick goes.
const GUTTER: f32 = 24.0;

/// What a menu item or shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    NewJob,
    ImportJobs,
    ExportJobs,
    /// Replica's folder, where `settings.toml` is.
    OpenAppFolder,
    OpenLogFolder,
    /// To the tray, as the window's close button does.
    CloseWindow,
    /// Quit Replica, as the tray's Quit does.
    Exit,
    RunAll,
    Stop,
    Show(Tab),
    ToggleStatusBar,
    Settings,
    /// Settings, on its Schedules tab with the scheduled tasks.
    ScheduledTasks,
    /// Windows' own Task Scheduler.
    TaskScheduler,
    ProjectPage,
    About,
}

impl Command {
    /// Its keyboard shortcut, if it has one.
    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        let key = match self {
            Command::NewJob => Key::N,
            Command::Show(Tab::Jobs) => Key::Num1,
            Command::Show(Tab::History) => Key::Num2,
            Command::Show(Tab::Restore) => Key::Num3,
            Command::Settings => Key::Comma,
            _ => return None,
        };
        Some(KeyboardShortcut::new(Modifiers::COMMAND, key))
    }

    /// Its shortcut as a menu shows it: "Ctrl+N", "Ctrl+1", "Ctrl+," (egui would say
    /// "Ctrl+Comma").
    pub fn shortcut_text(self, ctx: &egui::Context) -> Option<String> {
        let shortcut = self.shortcut()?;
        let key = match shortcut.logical_key {
            Key::Comma => ",",
            Key::Num1 => "1",
            Key::Num2 => "2",
            Key::Num3 => "3",
            key => key.name(),
        };
        Some(format!(
            "{}+{key}",
            ctx.format_modifiers(shortcut.modifiers)
        ))
    }

    /// The commands with a shortcut.
    const WITH_SHORTCUTS: [Command; 5] = [
        Command::NewJob,
        Command::Show(Tab::Jobs),
        Command::Show(Tab::History),
        Command::Show(Tab::Restore),
        Command::Settings,
    ];
}

/// What the menus need to know: which commands apply now, the page shown and whether the
/// status bar is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    pub tab: Tab,
    pub status_bar: bool,
    /// Jobs can be added or imported (the settings can be written and no job is being edited).
    pub can_add: bool,
    /// There are jobs to export, and no job is being edited.
    pub can_export: bool,
    /// A job is on, to run.
    pub can_run_all: bool,
    /// A job is running or queued, to stop.
    pub can_stop: bool,
}

impl MenuState {
    pub fn applies(&self, command: Command) -> bool {
        match command {
            Command::NewJob | Command::ImportJobs => self.can_add,
            Command::ExportJobs => self.can_export,
            Command::RunAll => self.can_run_all,
            Command::Stop => self.can_stop,
            _ => true,
        }
    }
}

/// The menu bar, across the top of the window: the command chosen, if any.
pub fn menu_bar(ui: &mut Ui, state: &MenuState) -> Option<Command> {
    let mut chosen = None;
    let id = ui.scope_id().with("menu-bar");
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_role(Role::MenuBar);
        node.set_label(tr("menu-bar"));
    });
    ui.scope_builder(UiBuilder::new().accessibility_parent(id), |ui| {
        let config = egui::containers::menu::MenuConfig::new().style(menu_style);
        egui::MenuBar::new()
            .style(bar_style)
            .config(config)
            .ui(ui, |ui| {
                menu(ui, "menu-file", |ui| {
                    item(ui, state, Command::NewJob, "menu-new-job", &mut chosen);
                    item(
                        ui,
                        state,
                        Command::ImportJobs,
                        "menu-import-jobs",
                        &mut chosen,
                    );
                    item(
                        ui,
                        state,
                        Command::ExportJobs,
                        "menu-export-jobs",
                        &mut chosen,
                    );
                    ui.separator();
                    item(
                        ui,
                        state,
                        Command::OpenAppFolder,
                        "menu-open-app-folder",
                        &mut chosen,
                    );
                    item(
                        ui,
                        state,
                        Command::OpenLogFolder,
                        "menu-open-log-folder",
                        &mut chosen,
                    );
                    ui.separator();
                    item(
                        ui,
                        state,
                        Command::CloseWindow,
                        "menu-close-window",
                        &mut chosen,
                    );
                    item(ui, state, Command::Exit, "menu-exit", &mut chosen);
                });
                menu(ui, "menu-jobs", |ui| {
                    item(ui, state, Command::RunAll, "menu-run-all", &mut chosen);
                    item(ui, state, Command::Stop, "menu-stop", &mut chosen);
                });
                menu(ui, "menu-view", |ui| {
                    for (tab, key) in [
                        (Tab::Jobs, "menu-view-jobs"),
                        (Tab::History, "menu-view-history"),
                        (Tab::Restore, "menu-view-restore"),
                    ] {
                        let r = item(ui, state, Command::Show(tab), key, &mut chosen);
                        let shown = state.tab == tab;
                        ui.ctx().accesskit_node_builder(r.id, |node| {
                            node.set_role(Role::MenuItemRadio);
                            node.set_toggled(shown.into());
                        });
                        if shown {
                            mark(ui, &r);
                        }
                    }
                    ui.separator();
                    let r = check_item(ui, state.status_bar, &tr("menu-status-bar"));
                    if r.clicked() {
                        ui.close();
                        chosen = Some(Command::ToggleStatusBar);
                    }
                });
                menu(ui, "menu-tools", |ui| {
                    item(ui, state, Command::Settings, "menu-settings", &mut chosen);
                    item(
                        ui,
                        state,
                        Command::ScheduledTasks,
                        "menu-scheduled-tasks",
                        &mut chosen,
                    );
                    item(
                        ui,
                        state,
                        Command::TaskScheduler,
                        "menu-task-scheduler",
                        &mut chosen,
                    );
                });
                menu(ui, "menu-help", |ui| {
                    item(
                        ui,
                        state,
                        Command::ProjectPage,
                        "menu-project-page",
                        &mut chosen,
                    );
                    ui.separator();
                    item(ui, state, Command::About, "menu-about", &mut chosen);
                });
            });
    });
    chosen
}

/// The command whose shortcut was pressed, if it applies now. Not while a dialog is up: the
/// dialog has the keyboard.
pub fn shortcuts(ctx: &egui::Context, state: &MenuState) -> Option<Command> {
    if ctx.memory(|m| m.top_modal_layer()).is_some() {
        return None;
    }
    Command::WITH_SHORTCUTS.into_iter().find(|&command| {
        let shortcut = command.shortcut().expect("a command with a shortcut");
        state.applies(command) && ctx.input_mut(|i| i.consume_shortcut(&shortcut))
    })
}

/// A menu in the bar, `key` its name.
fn menu(ui: &mut Ui, key: &str, content: impl FnOnce(&mut Ui)) {
    let name = tr(key);
    let (response, _) = egui::containers::menu::MenuButton::new(name.as_str()).ui(ui, |ui| {
        ui.set_min_width(220.0);
        content(ui)
    });
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(Role::MenuItem);
        node.set_label(name.as_str());
        node.set_has_popup(egui::accesskit::HasPopup::Menu);
    });
}

/// An item for `command`, named by `key`, with its shortcut, greyed out when it doesn't apply.
fn item(
    ui: &mut Ui,
    state: &MenuState,
    command: Command,
    key: &str,
    chosen: &mut Option<Command>,
) -> Response {
    let name = tr(key);
    let shortcut = command.shortcut_text(ui.ctx());
    let mut button = Button::new(name.as_str());
    if let Some(text) = &shortcut {
        button = button.shortcut_text(text.as_str());
    }
    let response = ui.add_enabled(state.applies(command), button);
    // Its name alone (the button's would run the shortcut into it), the shortcut apart.
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(Role::MenuItem);
        node.set_label(name.as_str());
        if let Some(text) = &shortcut {
            node.set_keyboard_shortcut(text.as_str());
        }
    });
    if response.clicked() {
        // A pointer's click closes the menu; Enter on an item, or a screen reader's Click,
        // doesn't by itself.
        ui.close();
        *chosen = Some(command);
    }
    response
}

/// An item that's ticked or not: the tick in the menus' left gutter.
fn check_item(ui: &mut Ui, on: bool, name: &str) -> Response {
    let response = ui.add(Button::new(name));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(Role::MenuItemCheckBox);
        node.set_label(name);
        node.set_toggled(on.into());
    });
    if on {
        mark(ui, &response);
    }
    response
}

/// A tick in an item's left gutter: the status bar on, the page shown.
fn mark(ui: &Ui, response: &Response) {
    let p = theme::of(ui);
    let rect = response.rect;
    let icon = egui::Rect::from_center_size(
        egui::pos2(rect.left() + GUTTER / 2.0, rect.center().y),
        egui::vec2(12.0, 12.0),
    );
    Icon::Check.paint(ui.painter(), icon, p.text);
}

/// The bar's buttons: flat until hovered, the frame's text size.
fn bar_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    text_size(style);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.item_spacing.x = 0.0;
}

/// The menus' items: flat until hovered, tight rows.
fn menu_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    text_size(style);
    // A gutter each side: for a tick on the left, and between an item and its shortcut.
    style.spacing.button_padding = egui::vec2(GUTTER, 4.0);
    style.spacing.item_spacing.y = 0.0;
}

fn text_size(style: &mut egui::Style) {
    for text in [egui::TextStyle::Body, egui::TextStyle::Button] {
        style
            .text_styles
            .insert(text, egui::FontId::proportional(MENU_TEXT_SIZE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortcuts_are_the_cards() {
        let ctx = egui::Context::default();
        let text = |c: Command| c.shortcut_text(&ctx).unwrap();
        assert_eq!(Command::NewJob.shortcut().unwrap().logical_key, Key::N);
        assert_eq!(text(Command::Show(Tab::History)), "Ctrl+2");
        assert_eq!(text(Command::Settings), "Ctrl+,");
        assert_eq!(text(Command::NewJob), "Ctrl+N");
        assert!(Command::Show(Tab::Settings).shortcut().is_none());
        assert!(Command::Exit.shortcut().is_none());
        for c in Command::WITH_SHORTCUTS {
            assert!(c.shortcut().is_some(), "{c:?}");
        }
    }

    #[test]
    fn commands_that_dont_apply_are_off() {
        let state = MenuState {
            tab: Tab::Jobs,
            status_bar: true,
            can_add: false,
            can_export: false,
            can_run_all: false,
            can_stop: false,
        };
        for c in [
            Command::NewJob,
            Command::ImportJobs,
            Command::ExportJobs,
            Command::RunAll,
            Command::Stop,
        ] {
            assert!(!state.applies(c), "{c:?}");
        }
        assert!(state.applies(Command::Settings));
        assert!(state.applies(Command::Exit));
    }
}
