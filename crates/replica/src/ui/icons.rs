//! Simple line icons, drawn in code, so no icon font or image ships (the licence rule: MIT or
//! equally permissive).
//!
//! Generic, for other apps to copy: each is drawn in a square with the stroke's colour, about
//! like Windows 11's Fluent icons at 16 points.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Jobs: a checklist.
    Jobs,
    /// History: a clock.
    History,
    /// Restore: an arrow turning back.
    Restore,
    /// Settings: a gear.
    Settings,
    /// About, and an info bar: an "i" in a circle.
    Info,
    Folder,
    Copy,
    /// A tick, for success.
    Check,
    /// A cross, for failure.
    Cross,
    /// A triangle with "!", for a warning.
    Warning,
    /// A dash, for "not run".
    Dash,
    /// An empty tray, for an empty list.
    Empty,
    /// A bin, for deleting.
    Delete,
    /// A page with a folded corner, for a file.
    File,
    /// Three dots in a row, for a menu of more actions.
    More,
    /// A plus, for adding.
    Add,
    /// A triangle pointing right, for running.
    Play,
    /// A square, for stopping.
    Stop,
    /// A pencil, for editing.
    Edit,
    /// A circling arrow, for looking again.
    Refresh,
}

impl Icon {
    /// Draws the icon in `rect` (made square, centred) with `colour`.
    pub fn paint(self, painter: &Painter, rect: Rect, colour: Color32) {
        let side = rect.width().min(rect.height());
        let rect = Rect::from_center_size(rect.center(), vec2(side, side));
        let width = (side / 11.0).max(1.25);
        let stroke = Stroke::new(width, colour);
        // A point at (x, y) in a 0..1 square.
        let at = |x: f32, y: f32| pos2(rect.left() + x * side, rect.top() + y * side);
        let line = |points: &[(f32, f32)]| {
            painter.add(Shape::line(
                points.iter().map(|&(x, y)| at(x, y)).collect(),
                stroke,
            ));
        };
        let circle = |x: f32, y: f32, r: f32| painter.circle_stroke(at(x, y), r * side, stroke);
        let dot = |x: f32, y: f32, r: f32| painter.circle_filled(at(x, y), r * side, colour);
        match self {
            Icon::Jobs => {
                for y in [0.22, 0.5, 0.78] {
                    line(&[(0.1, y), (0.18, y + 0.07), (0.32, y - 0.08)]);
                    line(&[(0.45, y), (0.92, y)]);
                }
            }
            Icon::History => {
                circle(0.5, 0.5, 0.42);
                line(&[(0.5, 0.25), (0.5, 0.5), (0.68, 0.62)]);
            }
            Icon::Restore => {
                // Three quarters of a circle, from the left round under to the top, with the
                // arrowhead at the top pointing left: turning back, anticlockwise.
                arc(
                    painter,
                    at(0.5, 0.52),
                    0.38 * side,
                    -FRAC_PI_2,
                    -1.5 * PI,
                    stroke,
                );
                line(&[(0.62, 0.02), (0.48, 0.14), (0.62, 0.27)]);
                line(&[(0.5, 0.33), (0.5, 0.54), (0.64, 0.64)]);
            }
            Icon::Settings => {
                let centre = at(0.5, 0.5);
                let mut points = Vec::new();
                let teeth = 8;
                for i in 0..teeth * 4 {
                    let a = i as f32 / (teeth * 4) as f32 * TAU;
                    let r = if (i / 2) % 2 == 0 { 0.44 } else { 0.34 };
                    points.push(centre + vec2(a.cos(), a.sin()) * r * side);
                }
                painter.add(Shape::closed_line(points, stroke));
                circle(0.5, 0.5, 0.14);
            }
            Icon::Info => {
                circle(0.5, 0.5, 0.42);
                dot(0.5, 0.3, 0.06);
                line(&[(0.5, 0.45), (0.5, 0.74)]);
            }
            Icon::Folder => {
                line(&[
                    (0.08, 0.22),
                    (0.38, 0.22),
                    (0.48, 0.32),
                    (0.92, 0.32),
                    (0.92, 0.82),
                    (0.08, 0.82),
                    (0.08, 0.22),
                ]);
            }
            Icon::Copy => {
                painter.rect_stroke(
                    Rect::from_min_max(at(0.3, 0.3), at(0.88, 0.9)),
                    2.0,
                    stroke,
                    eframe::egui::StrokeKind::Middle,
                );
                line(&[(0.12, 0.68), (0.12, 0.1), (0.66, 0.1)]);
            }
            Icon::Check => line(&[(0.18, 0.52), (0.4, 0.74), (0.82, 0.28)]),
            Icon::Cross => {
                line(&[(0.24, 0.24), (0.76, 0.76)]);
                line(&[(0.76, 0.24), (0.24, 0.76)]);
            }
            Icon::Warning => {
                line(&[(0.5, 0.1), (0.94, 0.88), (0.06, 0.88), (0.5, 0.1)]);
                line(&[(0.5, 0.38), (0.5, 0.62)]);
                dot(0.5, 0.75, 0.05);
            }
            Icon::Dash => line(&[(0.25, 0.5), (0.75, 0.5)]),
            Icon::File => {
                line(&[
                    (0.2, 0.06),
                    (0.58, 0.06),
                    (0.82, 0.3),
                    (0.82, 0.94),
                    (0.2, 0.94),
                    (0.2, 0.06),
                ]);
                line(&[(0.58, 0.06), (0.58, 0.3), (0.82, 0.3)]);
            }
            Icon::Delete => {
                line(&[(0.1, 0.22), (0.9, 0.22)]);
                line(&[(0.38, 0.22), (0.38, 0.1), (0.62, 0.1), (0.62, 0.22)]);
                line(&[(0.2, 0.22), (0.26, 0.9), (0.74, 0.9), (0.8, 0.22)]);
                line(&[(0.42, 0.38), (0.42, 0.74)]);
                line(&[(0.58, 0.38), (0.58, 0.74)]);
            }
            Icon::More => {
                for x in [0.2, 0.5, 0.8] {
                    dot(x, 0.5, 0.07);
                }
            }
            Icon::Add => {
                line(&[(0.5, 0.18), (0.5, 0.82)]);
                line(&[(0.18, 0.5), (0.82, 0.5)]);
            }
            Icon::Play => line(&[(0.26, 0.14), (0.82, 0.5), (0.26, 0.86), (0.26, 0.14)]),
            Icon::Stop => {
                painter.rect_stroke(
                    Rect::from_min_max(at(0.2, 0.2), at(0.8, 0.8)),
                    1.5,
                    stroke,
                    eframe::egui::StrokeKind::Middle,
                );
            }
            Icon::Edit => {
                line(&[
                    (0.14, 0.86),
                    (0.18, 0.66),
                    (0.66, 0.18),
                    (0.82, 0.34),
                    (0.34, 0.82),
                    (0.14, 0.86),
                ]);
                line(&[(0.56, 0.28), (0.72, 0.44)]);
            }
            Icon::Refresh => {
                // Five sixths of a circle, clockwise from the right round to the top right,
                // with the arrowhead there pointing on round.
                arc(
                    painter,
                    at(0.5, 0.5),
                    0.38 * side,
                    5.0 * PI / 3.0,
                    5.0 * PI / 3.0,
                    stroke,
                );
                line(&[(0.7, 0.04), (0.78, 0.22), (0.58, 0.25)]);
            }
            Icon::Empty => {
                line(&[
                    (0.06, 0.55),
                    (0.22, 0.2),
                    (0.78, 0.2),
                    (0.94, 0.55),
                    (0.94, 0.85),
                    (0.06, 0.85),
                    (0.06, 0.55),
                    (0.34, 0.55),
                    (0.4, 0.66),
                    (0.6, 0.66),
                    (0.66, 0.55),
                    (0.94, 0.55),
                ]);
            }
        }
    }
}

/// An arc of `sweep` radians round `centre`, from `start` (0 is to the right, angles go
/// clockwise on screen), drawn anticlockwise.
fn arc(painter: &Painter, centre: Pos2, radius: f32, start: f32, sweep: f32, stroke: Stroke) {
    let steps = 32;
    let points = (0..=steps)
        .map(|i| {
            let a = start - sweep * i as f32 / steps as f32;
            centre + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    painter.add(Shape::line(points, stroke));
}
