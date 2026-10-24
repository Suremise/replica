//! The About tab: the version, Replica's MIT licence, the third-party notices (every crate, and
//! restic's section with its dependencies' licences, the approved exception to the licence
//! rule) and the project's page, each in a card.
//!
//! The texts are compiled in, so the portable copy needs no extra files.

use std::sync::Arc;

use eframe::egui::{self, RichText, TextWrapMode, Ui};
use replica_core::i18n::tr;
use replica_core::tr;

use super::backend::Backend;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, card_frame, collapsing, info_bar, info_row, secondary_button, section_card,
    section_card_with_actions,
};

/// The project's page.
pub const PROJECT_URL: &str = "https://github.com/Suremise/replica";
/// The licence on the project's page.
pub const LICENCE_URL: &str = "https://github.com/Suremise/replica/blob/main/LICENSE";

/// Replica's licence.
pub const LICENSE: &str = include_str!("../../../../LICENSE");
/// The crates' and restic's notices, as made for the release (`THIRD-PARTY-NOTICES.txt`):
/// empty in a development build made before them.
pub const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/third-party-notices.txt"));
/// restic's licences, which the notices carry too; shown on their own when the notices
/// haven't been made.
pub const RESTIC_LICENCES: &str = include_str!("../../../../third_party/restic/LICENSES.md");
/// The line that starts restic's part of the notices (`scripts/third-party-notices.ps1`).
const RESTIC_PART: &str = "PART 2: RESTIC";

/// The app icon's size here, in points.
const ICON_SIZE: f32 = 40.0;

pub struct AboutTab {
    backend: Arc<dyn Backend>,
    message: Option<String>,
    /// "Go to restic's section" was clicked: scroll the notices there next frame.
    to_restic: bool,
}

impl AboutTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            message: None,
            to_restic: false,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(e) = &self.message {
                    info_bar(ui, InfoKind::Error, None, e);
                    ui.add_space(space::M);
                }
                self.app_card(ui);
                self.licence_card(ui);
                self.notices_card(ui);
                section_card(ui, &tr("about-links"), None, |ui| {
                    info_row(ui, &tr("about-project"), None, |ui| {
                        let link = ui.link(PROJECT_URL).on_hover_text(tr("about-project-hint"));
                        if link.clicked() {
                            self.message = self.backend.open_url(PROJECT_URL).err();
                        }
                        link
                    });
                });
            });
    }

    /// The app icon, its name, version and what it is.
    fn app_card(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
        card_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE), egui::Sense::hover());
                let texture = super::app_icon(ui.ctx(), ICON_SIZE);
                ui.painter().image(
                    texture,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                ui.add_space(space::M);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(tr("app-name")).font(theme::semibold(20.0)));
                    ui.label(tr!("about-version", version = env!("CARGO_PKG_VERSION")));
                    ui.label(RichText::new(tr("about-tagline")).color(p.text_secondary));
                });
            });
        });
        ui.add_space(space::M);
    }

    fn licence_card(&mut self, ui: &mut Ui) {
        let mut open = false;
        section_card_with_actions(
            ui,
            &tr("about-licence-title"),
            Some(&tr("about-licence")),
            |ui| {
                open = secondary_button(ui, &tr("about-licence-online"))
                    .on_hover_text(LICENCE_URL)
                    .clicked();
            },
            |ui| {
                collapsing(
                    ui,
                    egui::CollapsingHeader::new(tr("about-licence-text")).id_salt("licence"),
                    |ui| text_block(ui, "licence-text", LICENSE, None, false),
                );
            },
        );
        if open {
            self.message = self.backend.open_url(LICENCE_URL).err();
        }
    }

    /// The notices in a viewer, with Copy and a way to restic's section: a card that opens
    /// from its title (Windows' expander), as the notices are long.
    fn notices_card(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
        let missing = NOTICES.trim().is_empty();
        let text = if missing { RESTIC_LICENCES } else { NOTICES };
        // Where restic's part starts, as a byte offset.
        let restic_line = (!missing).then(|| text.find(RESTIC_PART)).flatten();
        let mut copy = false;
        let mut to_restic = false;
        card_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = RichText::new(tr("about-notices")).text_style(theme::section_style());
            collapsing(
                ui,
                egui::CollapsingHeader::new(title).id_salt("notices"),
                |ui| {
                    ui.horizontal(|ui| {
                        copy = secondary_button(ui, &tr("about-copy")).clicked();
                        if restic_line.is_some() {
                            to_restic = secondary_button(ui, &tr("about-go-to-restic")).clicked();
                        }
                    });
                    ui.add_space(space::S);
                    if missing {
                        info_bar(ui, InfoKind::Info, None, &tr("about-notices-missing"));
                        ui.add_space(space::S);
                    }
                    text_block(ui, "notices-text", text, restic_line, self.to_restic);
                },
            );
            ui.label(RichText::new(tr("about-notices-help")).color(p.text_secondary));
        });
        ui.add_space(space::M);
        self.to_restic = to_restic;
        if copy {
            ui.ctx().copy_text(text.to_owned());
        }
    }
}

/// A long text in monospace, wrapped to the viewer's width, selectable and readable by a
/// screen reader, in a framed viewer that scrolls. With `split` (a byte offset), the text
/// from there is a part of its own, which `jump` scrolls to the top.
fn text_block(ui: &mut Ui, id: &str, text: &str, split: Option<usize>, jump: bool) {
    let p = theme::of(ui);
    let label = |ui: &mut Ui, part: &str| {
        ui.add(
            egui::Label::new(RichText::new(part).monospace())
                .wrap_mode(TextWrapMode::Wrap)
                .selectable(true),
        )
    };
    egui::Frame::new()
        .fill(p.control)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CONTROL_RADIUS)
        .inner_margin(space::S)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(360.0)
                .auto_shrink([false, true])
                .show(ui, |ui| match split {
                    Some(at) => {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        label(ui, text[..at].trim_end());
                        ui.add_space(ui.text_style_height(&egui::TextStyle::Monospace));
                        let rest = label(ui, &text[at..]);
                        if jump {
                            rest.scroll_to_me(Some(egui::Align::TOP));
                        }
                    }
                    None => {
                        label(ui, text);
                    }
                });
        });
}
