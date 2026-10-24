//! The window's look: colours for Windows' light and dark modes, the type scale and the
//! spacing, applied to egui's two styles. Modelled on Windows 11 Settings, not a copy of it.
//!
//! egui follows Windows' light or dark mode by itself (its theme preference is "system", and
//! winit reports a change while the app runs), so [`install`] sets both styles once and the
//! window switches live. Widgets that draw their own colours take them from [`of`].
//!
//! Generic, for other apps to copy: nothing here is Replica's except the accent's value in
//! [`LIGHT`] and [`DARK`]. Every pair of text and background passes WCAG AA, which this module's
//! tests compute from the values below.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, Style, TextStyle,
    Theme, Ui, Visuals,
};

use super::fonts::SEMIBOLD;

/// The colours of one theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The window behind everything, with the sidebar.
    pub background: Color32,
    /// A card (a group of settings, a list), raised a little from the background.
    pub card: Color32,
    /// A card's hairline border and the dividers between its rows: decoration only, so it
    /// needs no contrast of its own.
    pub border: Color32,
    /// A button or field at rest, hovered and pressed.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_pressed: Color32,
    /// A control's outline, where it is what shows the control (3:1 against the card).
    pub control_border: Color32,
    /// An item in a list or the sidebar, hovered and selected.
    pub subtle_hover: Color32,
    pub subtle_selected: Color32,
    /// Every other row of a table, on a card.
    pub stripe: Color32,
    pub text: Color32,
    /// Descriptions, hints and other text that comes second.
    pub text_secondary: Color32,
    /// The accent: links, the selected item's bar, primary buttons, focus.
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,
    /// Text on the accent.
    pub on_accent: Color32,
    /// Behind selected text and selected rows.
    pub selection: Color32,
    /// The keyboard focus ring.
    pub focus: Color32,
    /// Status text and glyphs, each with the fill of its chip or bar.
    pub ok: Color32,
    pub ok_fill: Color32,
    pub failed: Color32,
    pub failed_fill: Color32,
    pub warning: Color32,
    pub warning_fill: Color32,
    pub neutral: Color32,
    pub neutral_fill: Color32,
    pub info_fill: Color32,
    /// A danger button (delete, remove all), hovered and pressed, and the text on it.
    pub danger: Color32,
    pub danger_hover: Color32,
    pub danger_pressed: Color32,
    pub on_danger: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Windows' light mode.
pub const LIGHT: Palette = Palette {
    background: hex(0xF3F3F3),
    card: hex(0xFBFBFB),
    border: hex(0xE5E5E5),
    control: hex(0xFEFEFE),
    control_hover: hex(0xF6F6F6),
    control_pressed: hex(0xEDEDED),
    control_border: hex(0x8A8A8A),
    subtle_hover: hex(0xEAEAEA),
    subtle_selected: hex(0xE3E3E3),
    stripe: hex(0xF4F4F4),
    text: hex(0x1B1B1B),
    text_secondary: hex(0x5D5D5D),
    // Replica's teal.
    accent: hex(0x0E7490),
    accent_hover: hex(0x0C6680),
    accent_pressed: hex(0x0A5870),
    on_accent: hex(0xFFFFFF),
    selection: hex(0xCDE6EE),
    focus: hex(0x1B1B1B),
    ok: hex(0x0F7B0F),
    ok_fill: hex(0xDFF6DD),
    failed: hex(0xB4231A),
    failed_fill: hex(0xFDE7E9),
    warning: hex(0x8A5100),
    warning_fill: hex(0xFFF4CE),
    neutral: hex(0x5D5D5D),
    neutral_fill: hex(0xEBEBEB),
    info_fill: hex(0xE3F1F5),
    danger: hex(0xB4231A),
    danger_hover: hex(0xA01F17),
    danger_pressed: hex(0x8C1B14),
    on_danger: hex(0xFFFFFF),
};

/// Windows' dark mode.
pub const DARK: Palette = Palette {
    background: hex(0x202020),
    card: hex(0x2B2B2B),
    border: hex(0x1D1D1D),
    control: hex(0x353535),
    control_hover: hex(0x3C3C3C),
    control_pressed: hex(0x303030),
    control_border: hex(0x8C8C8C),
    subtle_hover: hex(0x2D2D2D),
    subtle_selected: hex(0x333333),
    stripe: hex(0x313131),
    text: hex(0xFFFFFF),
    text_secondary: hex(0xC8C8C8),
    // Replica's teal, lighter for the dark background.
    accent: hex(0x5CC6DB),
    accent_hover: hex(0x52B4C8),
    accent_pressed: hex(0x48A2B4),
    on_accent: hex(0x0B1B1F),
    selection: hex(0x1E4A55),
    focus: hex(0xFFFFFF),
    ok: hex(0x6CCB5F),
    ok_fill: hex(0x2A3A27),
    failed: hex(0xFF99A4),
    failed_fill: hex(0x442726),
    warning: hex(0xF2C661),
    warning_fill: hex(0x433519),
    neutral: hex(0xC8C8C8),
    neutral_fill: hex(0x3A3A3A),
    info_fill: hex(0x1F3A41),
    danger: hex(0xFF99A4),
    danger_hover: hex(0xF08893),
    danger_pressed: hex(0xE07782),
    on_danger: hex(0x2B0A0D),
};

/// The spacing scale, in points.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
    pub const XXXL: f32 = 40.0;

    // What the scale is used for (roomier than a first pass).

    /// Round a page's content: the window's edges, and the sidebar at a narrower width.
    pub const PAGE_MARGIN: f32 = XXL;
    /// Between the full-width sidebar and the page.
    pub const SIDEBAR_GAP: f32 = XXXL;
    /// Under a page's header, before its first content.
    pub const UNDER_HEADER: f32 = XL;
    /// Between one card and the next.
    pub const CARD_GAP: f32 = L;
    /// Inside a card, round its contents.
    pub const CARD_PADDING: f32 = 20.0;
    /// Above and below a setting row's text and control.
    pub const ROW_PADDING: f32 = M;
    /// The least height of a setting row with a description.
    pub const ROW_MIN_HEIGHT: f32 = 52.0;
    /// Between the controls of a toolbar, and between its groups.
    pub const TOOLBAR_GAP: f32 = S;
    pub const TOOLBAR_GROUP_GAP: f32 = M;
    /// Above and below a table row's tallest cell, and the least height of a row.
    pub const TABLE_ROW_PADDING: f32 = 10.0;
    pub const TABLE_ROW_MIN_HEIGHT: f32 = 40.0;
    /// The sidebar's items.
    pub const NAV_ITEM_HEIGHT: f32 = 40.0;
    pub const NAV_ITEM_GAP: f32 = XS;
}

/// Buttons, fields and the sidebar's items.
pub const CONTROL_HEIGHT: f32 = 32.0;
pub const CARD_RADIUS: u8 = 6;
pub const CONTROL_RADIUS: u8 = 4;
/// The widest a page's content gets, so lines stay readable on a wide window.
pub const MAX_CONTENT_WIDTH: f32 = 880.0;

/// The type scale, in points.
pub const TITLE_SIZE: f32 = 24.0;
pub const SECTION_SIZE: f32 = 16.0;
pub const BODY_SIZE: f32 = 14.0;
pub const SMALL_SIZE: f32 = 12.0;

/// A section's title: 16 semibold ([`TextStyle::Heading`] is the page's title).
pub fn section_style() -> TextStyle {
    TextStyle::Name("section".into())
}

/// Semibold at `size`.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, SEMIBOLD.clone())
}

/// The colours of the theme `ui` is drawn in.
pub fn of(ui: &Ui) -> &'static Palette {
    palette(if ui.visuals().dark_mode {
        Theme::Dark
    } else {
        Theme::Light
    })
}

pub fn palette(theme: Theme) -> &'static Palette {
    match theme {
        Theme::Light => &LIGHT,
        Theme::Dark => &DARK,
    }
}

/// Gives `ctx` both themes' styles. The fonts (with [`SEMIBOLD`]) must be installed first.
pub fn install(ctx: &egui::Context) {
    ctx.set_style_of(Theme::Light, style(&LIGHT, Visuals::light()));
    ctx.set_style_of(Theme::Dark, style(&DARK, Visuals::dark()));
}

fn style(p: &Palette, mut visuals: Visuals) -> Style {
    let text_styles = [
        (TextStyle::Heading, semibold(TITLE_SIZE)),
        (section_style(), semibold(SECTION_SIZE)),
        (
            TextStyle::Body,
            FontId::new(BODY_SIZE, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(BODY_SIZE, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(SMALL_SIZE, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    let mut style = Style {
        text_styles,
        ..Style::default()
    };

    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(space::S, 6.0);
    spacing.button_padding = egui::vec2(space::M, 5.0);
    spacing.interact_size = egui::vec2(40.0, 24.0);
    spacing.icon_width = 18.0;
    spacing.icon_width_inner = 10.0;
    spacing.icon_spacing = space::S;
    spacing.window_margin = Margin::same(space::XL as i8);
    spacing.menu_margin = Margin::same(space::XS as i8);

    let control = CornerRadius::same(CONTROL_RADIUS);
    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = control;
    for (v, fill) in [
        (&mut w.inactive, p.control),
        (&mut w.hovered, p.control_hover),
        (&mut w.open, p.control_hover),
        (&mut w.active, p.control_pressed),
    ] {
        v.bg_fill = fill;
        v.weak_bg_fill = fill;
        v.bg_stroke = Stroke::new(1.0, p.control_border);
        v.fg_stroke = Stroke::new(1.0, p.text);
        v.corner_radius = control;
        v.expansion = 0.0;
    }
    visuals.selection.bg_fill = p.selection;
    visuals.selection.stroke = Stroke::new(1.0, p.text);
    visuals.hyperlink_color = p.accent;
    visuals.faint_bg_color = p.stripe;
    visuals.extreme_bg_color = p.control;
    visuals.text_edit_bg_color = Some(p.control);
    visuals.code_bg_color = p.subtle_hover;
    visuals.warn_fg_color = p.warning;
    visuals.error_fg_color = p.failed;
    visuals.panel_fill = p.background;
    visuals.window_fill = p.card;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(CARD_RADIUS);
    visuals.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if visuals.dark_mode { 110 } else { 40 }),
    };
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if visuals.dark_mode { 90 } else { 30 }),
    };
    visuals.text_cursor.stroke = Stroke::new(2.0, p.accent);
    visuals.slider_trailing_fill = true;
    visuals.collapsing_header_frame = false;
    style.visuals = visuals;
    style
}

/// Rings the widget that has the keyboard focus, so it's clear where typing and Space go.
/// Called once a frame, after everything is drawn.
pub fn paint_focus(ctx: &egui::Context) {
    let Some(response) = ctx
        .memory(|m| m.focused())
        .and_then(|id| ctx.read_response(id))
    else {
        return;
    };
    let p = palette(ctx.theme());
    let ring = response.rect.expand(2.0);
    ctx.layer_painter(response.layer_id)
        .with_clip_rect(response.interact_rect.expand(3.0))
        .rect_stroke(
            ring,
            CornerRadius::same(CONTROL_RADIUS + 2),
            Stroke::new(2.0, p.focus),
            egui::StrokeKind::Outside,
        );
}

/// WCAG 2's contrast ratio between two opaque colours: 1 to 21.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn luminance(c: Color32) -> f32 {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG AA: body text.
    const TEXT: f32 = 4.5;
    /// WCAG AA: large text (the 24 pt title), and glyphs and outlines that show a control.
    const UI: f32 = 3.0;

    /// Each pair the widgets draw: (what, foreground, background, the ratio it needs).
    fn pairs(p: &Palette) -> Vec<(&'static str, Color32, Color32, f32)> {
        let mut pairs = Vec::new();
        for (bg_name, bg) in [
            ("background", p.background),
            ("card", p.card),
            ("subtle hover", p.subtle_hover),
            ("subtle selected", p.subtle_selected),
            ("table stripe", p.stripe),
            ("control", p.control),
            ("control hover", p.control_hover),
            ("control pressed", p.control_pressed),
            ("selection", p.selection),
        ] {
            pairs.push((bg_name, p.text, bg, TEXT));
            pairs.push((bg_name, p.text_secondary, bg, TEXT));
        }
        for (bg_name, bg) in [("background", p.background), ("card", p.card)] {
            pairs.push((bg_name, p.accent, bg, TEXT));
            pairs.push((bg_name, p.ok, bg, TEXT));
            pairs.push((bg_name, p.failed, bg, TEXT));
            pairs.push((bg_name, p.warning, bg, TEXT));
            pairs.push((bg_name, p.neutral, bg, TEXT));
            pairs.push((bg_name, p.focus, bg, UI));
            pairs.push((bg_name, p.control_border, bg, UI));
            // A primary or danger button's own shape against what it sits on.
            pairs.push((bg_name, p.accent, bg, UI));
            pairs.push((bg_name, p.danger, bg, UI));
        }
        // A danger button's red text (and icon) on a secondary button's fill.
        for (bg_name, bg) in [
            ("control", p.control),
            ("control hover", p.control_hover),
            ("control pressed", p.control_pressed),
        ] {
            pairs.push((bg_name, p.failed, bg, TEXT));
        }
        // The selected item's accent bar.
        pairs.push(("sidebar selected", p.accent, p.subtle_selected, UI));
        for (bg_name, bg) in [
            ("accent", p.accent),
            ("accent hover", p.accent_hover),
            ("accent pressed", p.accent_pressed),
        ] {
            pairs.push((bg_name, p.on_accent, bg, TEXT));
        }
        for (bg_name, bg) in [
            ("danger", p.danger),
            ("danger hover", p.danger_hover),
            ("danger pressed", p.danger_pressed),
        ] {
            pairs.push((bg_name, p.on_danger, bg, TEXT));
        }
        for (name, fg, fill) in [
            ("ok chip", p.ok, p.ok_fill),
            ("failed chip", p.failed, p.failed_fill),
            ("warning chip", p.warning, p.warning_fill),
            ("neutral chip", p.neutral, p.neutral_fill),
            ("info chip", p.accent, p.info_fill),
        ] {
            pairs.push((name, fg, fill, TEXT));
            pairs.push((name, p.text, fill, TEXT));
        }
        pairs.push(("info bar", p.text, p.info_fill, TEXT));
        pairs.push(("info bar glyph", p.accent, p.info_fill, UI));
        pairs
    }

    #[test]
    fn every_pair_passes_wcag_aa_in_both_themes() {
        let mut failures = Vec::new();
        for (theme, p) in [("light", &LIGHT), ("dark", &DARK)] {
            for (what, fg, bg, needs) in pairs(p) {
                let ratio = contrast(fg, bg);
                if ratio < needs {
                    failures.push(format!(
                        "{theme}: {fg:?} on {what} {bg:?} is {ratio:.2}:1, needs {needs}:1"
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn contrast_matches_wcag_examples() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.001);
        // #767676 on white is the well-known 4.54:1.
        assert!((contrast(hex(0x767676), Color32::WHITE) - 4.54).abs() < 0.01);
    }

    #[test]
    fn the_accent_is_replicas_teal_in_light_mode() {
        assert_eq!(LIGHT.accent, Color32::from_rgb(0x0E, 0x74, 0x90));
    }
}
