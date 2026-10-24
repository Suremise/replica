//! The style gallery (test only): every component of `replica::ui::widgets` and every icon, on
//! one page, in the theme it's drawn in. Its sample text is English on purpose: it's never
//! shown to users.

use eframe::egui::{self, RichText, ScrollArea, Ui};
use replica::ui::icons::Icon;
use replica::ui::theme::{self, MAX_CONTENT_WIDTH, space};
use replica::ui::widgets::{
    InfoKind, Sort, TableColumn, TableState, Tone, danger_button, danger_button_filled,
    empty_state, icon_button, info_bar, path_field, primary_button, secondary_button, section_card,
    segmented, setting_row, status_chip, tab_strip, table, toggle, toolbar, toolbar_group_gap,
};

const ICONS: [Icon; 14] = [
    Icon::Jobs,
    Icon::History,
    Icon::Restore,
    Icon::Settings,
    Icon::Info,
    Icon::Folder,
    Icon::Copy,
    Icon::Check,
    Icon::Cross,
    Icon::Warning,
    Icon::Dash,
    Icon::Empty,
    Icon::Delete,
    Icon::Refresh,
];

/// The table's rows: job, result, size.
const ROWS: [(&str, Tone, &str, u64); 5] = [
    ("Documents", Tone::Ok, "Succeeded", 12_300_000),
    ("Photos", Tone::Warning, "Partly done", 931_400_000_000),
    ("Music", Tone::Failed, "Failed", 0),
    ("Projects", Tone::Ok, "Succeeded", 4_500_000_000),
    ("Old laptop", Tone::Neutral, "Not run", 0),
];

pub struct Gallery {
    on: bool,
    notify: bool,
    days: u32,
    choice: usize,
    tab: usize,
    language: usize,
    text: String,
    table: TableState,
    empty_table: TableState,
    focused: bool,
}

impl Default for Gallery {
    fn default() -> Self {
        Self {
            on: true,
            notify: false,
            days: 30,
            choice: 0,
            tab: 0,
            language: 0,
            text: "Nightly".into(),
            table: TableState::new(
                Some(Sort {
                    column: 0,
                    ascending: true,
                }),
                Some(1),
            ),
            empty_table: TableState::default(),
            focused: false,
        }
    }
}

impl Gallery {
    pub fn ui(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.background)
                    .inner_margin(space::XL),
            )
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(MAX_CONTENT_WIDTH));
                    self.contents(ui);
                });
            });
        theme::paint_focus(ui.ctx());
    }

    fn contents(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
        ui.heading("Style gallery");
        ui.label(RichText::new("Every component, in this theme").color(p.text_secondary));
        ui.add_space(space::L);

        section_card(
            ui,
            "Setting rows",
            Some("A card of rows, each with its control on the right"),
            |ui| {
                setting_row(ui, "Pause schedules", None, |ui| toggle(ui, &mut self.on));
                setting_row(
                    ui,
                    "Notify on success",
                    Some("A failure always notifies"),
                    |ui| ui.checkbox(&mut self.notify, ""),
                );
                setting_row(
                    ui,
                    "Keep run logs for (days)",
                    Some("Older logs are deleted when Replica starts"),
                    |ui| ui.add(egui::DragValue::new(&mut self.days).range(1..=365)),
                );
                setting_row(
                    ui,
                    "What runs the schedules",
                    Some("Task Scheduler runs them even when the tray isn't running"),
                    |ui| {
                        let (group, chosen) =
                            segmented(ui, &["Built-in timer", "Task Scheduler"], self.choice);
                        if let Some(c) = chosen {
                            self.choice = c;
                        }
                        group
                    },
                );
                setting_row(ui, "Language", Some("A dropdown"), |ui| {
                    egui::ComboBox::from_id_salt("language")
                        .selected_text("English")
                        .show_index(ui, &mut self.language, 1, |_| "English")
                });
                setting_row(ui, "Job name", None, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.text).desired_width(200.0))
                });
            },
        );

        section_card(ui, "Buttons", None, |ui| {
            ui.horizontal(|ui| {
                primary_button(ui, "Run now");
                secondary_button(ui, "Edit");
                danger_button(ui, None, "Remove");
                danger_button(ui, Some(Icon::Delete), "Delete");
                ui.add_enabled_ui(false, |ui| secondary_button(ui, "Disabled"));
                icon_button(ui, Icon::Folder, "Open the folder");
            });
            ui.add_space(space::S);
            ui.label(
                RichText::new("Filled red only for the last step, in a confirmation:")
                    .color(p.text_secondary),
            );
            egui::Frame::new()
                .fill(p.card)
                .stroke(egui::Stroke::new(1.0, p.border))
                .corner_radius(8)
                .inner_margin(space::L)
                .show(ui, |ui| {
                    ui.label(RichText::new("Delete Photos?").text_style(theme::section_style()));
                    ui.label("Its backups and their passwords stay.");
                    ui.add_space(space::M);
                    ui.horizontal(|ui| {
                        danger_button_filled(ui, "Delete");
                        secondary_button(ui, "Cancel");
                    });
                });
            ui.add_space(space::S);
            ui.label(
                RichText::new("The focused button has the focus ring:").color(p.text_secondary),
            );
            let focused = secondary_button(ui, "Focused");
            if !self.focused {
                focused.request_focus();
                self.focused = true;
            }
        });

        section_card(ui, "Tabs and toolbars", None, |ui| {
            tab_strip(
                ui,
                "Settings sections",
                &["Schedules", "General", "restic", "Storage"],
                &mut self.tab,
            );
            ui.add_space(space::L);
            toolbar(ui, |ui| {
                primary_button(ui, "New job");
                secondary_button(ui, "Run all");
                toolbar_group_gap(ui);
                secondary_button(ui, "Export");
                secondary_button(ui, "Import");
            });
        });

        section_card(ui, "Status chips", None, |ui| {
            ui.horizontal(|ui| {
                status_chip(ui, Tone::Ok, "Succeeded");
                status_chip(ui, Tone::Failed, "Failed");
                status_chip(ui, Tone::Warning, "Skipped");
                status_chip(ui, Tone::Warning, "Overdue");
                status_chip(ui, Tone::Neutral, "Not run");
                status_chip(ui, Tone::Info, "Installed");
            });
        });

        section_card(ui, "Path fields", None, |ui| {
            path_field(ui, r"C:\Users\Alex\Documents");
            ui.add_space(space::S);
            path_field(
                ui,
                r"\\nas\backups\Replica\Documents\2026\October\Projects\Northwind\design\notes\final-final.docx",
            );
        });

        info_bar(
            ui,
            InfoKind::Info,
            Some("Schedules are paused"),
            "No job runs until you turn them back on.",
        );
        ui.add_space(space::S);
        info_bar(
            ui,
            InfoKind::Warning,
            None,
            "The drive BACKUP isn't connected, so Documents was skipped.",
        );
        ui.add_space(space::S);
        info_bar(
            ui,
            InfoKind::Error,
            Some("Music failed"),
            "restic couldn't open the repository: wrong password.",
        );
        ui.add_space(space::M);

        section_card(
            ui,
            "Table",
            Some("Sortable, resizable, with a selected row"),
            |ui| {
                let columns = [
                    TableColumn::new("Job", Some(200.0)).sortable(),
                    TableColumn::new("Result", Some(160.0)).sortable(),
                    TableColumn::new("Size", Some(120.0)),
                ];
                table(
                    ui,
                    "Runs",
                    &columns,
                    ROWS.len(),
                    None,
                    &mut self.table,
                    |i| ROWS[i].0.to_owned(),
                    |_| {},
                    |ui, row, col| {
                        let (job, tone, result, bytes) = ROWS[row];
                        match col {
                            0 => {
                                ui.label(job);
                            }
                            1 => {
                                status_chip(ui, tone, result);
                            }
                            _ => {
                                ui.label(replica::ui::widgets::size_text(bytes));
                            }
                        }
                    },
                );
            },
        );

        section_card(ui, "An empty table", None, |ui| {
            table(
                ui,
                "Nothing",
                &[
                    TableColumn::new("Job", Some(200.0)),
                    TableColumn::new("When", None),
                ],
                0,
                None,
                &mut self.empty_table,
                |_| String::new(),
                |ui| {
                    empty_state(
                        ui,
                        Icon::Empty,
                        "No runs yet",
                        "Runs show here once a job has run.",
                        Some("Run all now"),
                    );
                },
                |_, _, _| {},
            );
        });

        section_card(ui, "Icons", None, |ui| {
            ui.horizontal(|ui| {
                for icon in ICONS {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                    icon.paint(ui.painter(), rect.shrink(4.0), p.text);
                }
            });
        });

        section_card(ui, "Type", None, |ui| {
            ui.heading("Page title, 24 semibold");
            ui.label(
                RichText::new("Section title, 16 semibold").text_style(theme::section_style()),
            );
            ui.label("Body text, 14");
            ui.label(
                RichText::new("Secondary text, 12")
                    .small()
                    .color(p.text_secondary),
            );
            ui.label(RichText::new(r"C:\Paths\in\monospace.txt").monospace());
            ui.hyperlink_to("A link", "https://example.com");
        });
    }
}
