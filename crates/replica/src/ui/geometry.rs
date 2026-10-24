//! What the window remembers between runs (`window.json` in the app's folder): where it was,
//! how big, and whether the "still running in the tray" notice was turned off.
//!
//! Not in `settings.toml`, which is the user's own file: this is the window's state, and losing
//! it only costs a default size.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The file's name in the app's folder.
pub const FILE_NAME: &str = "window.json";
/// The smallest size, in points, the window opens at.
pub const MIN_SIZE: [f32; 2] = [720.0, 480.0];
/// The size, in points, it first opens at, unless that's more than [`WORK_AREA_SHARE`]
/// of the monitor's work area ([`first_rect`]).
pub const DEFAULT_SIZE: [f32; 2] = [1180.0, 780.0];
/// The most of the work area's width and height a first-run window takes.
pub const WORK_AREA_SHARE: f32 = 0.85;

/// Where the window first opens ([`WindowState::rect`] is `None`): [`DEFAULT_SIZE`], or less on
/// a small work area (`work`: left, top, right, bottom in pixels, at `pixels_per_point`), and
/// never under [`MIN_SIZE`]; centred on the work area.
pub fn first_rect(work: [i32; 4], pixels_per_point: f32) -> SavedRect {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let [left, top, right, bottom] = work.map(|v| v as f32 / ppp);
    let (work_w, work_h) = ((right - left).max(0.0), (bottom - top).max(0.0));
    let width = DEFAULT_SIZE[0]
        .min(work_w * WORK_AREA_SHARE)
        .max(MIN_SIZE[0]);
    let height = DEFAULT_SIZE[1]
        .min(work_h * WORK_AREA_SHARE)
        .max(MIN_SIZE[1]);
    SavedRect {
        x: (left + (work_w - width) / 2.0).round(),
        y: (top + (work_h - height) / 2.0).round(),
        width,
        height,
        pixels_per_point: ppp,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// Where the window was, if it has been shown.
    pub rect: Option<SavedRect>,
    pub maximized: bool,
    /// "Don't show again" was ticked on the notice that closing keeps Replica in the tray.
    pub tray_notice_off: bool,
}

/// The window's outer rectangle in points, with the scale it had (pixels per point), so it can
/// be checked against the monitors in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub pixels_per_point: f32,
}

impl SavedRect {
    /// In screen pixels: left, top, right, bottom.
    pub fn pixels(&self) -> [i32; 4] {
        let px = |v: f32| (v * self.pixels_per_point).round() as i32;
        [
            px(self.x),
            px(self.y),
            px(self.x + self.width),
            px(self.y + self.height),
        ]
    }

    /// Whether to put the window back here: a sane size, and on a monitor that's still there
    /// (`on_screen` checks a rectangle in pixels).
    pub fn usable(&self, on_screen: impl Fn([i32; 4]) -> bool) -> bool {
        let finite = [
            self.x,
            self.y,
            self.width,
            self.height,
            self.pixels_per_point,
        ]
        .iter()
        .all(|v| v.is_finite());
        finite
            && self.pixels_per_point > 0.0
            && self.width >= MIN_SIZE[0] / 2.0
            && self.height >= MIN_SIZE[1] / 2.0
            && on_screen(self.title_bar())
    }

    /// The top strip, where the title bar is: that much must be on a monitor to move it.
    fn title_bar(&self) -> [i32; 4] {
        let [left, top, right, _] = self.pixels();
        [
            left,
            top,
            right,
            top + (32.0 * self.pixels_per_point) as i32,
        ]
    }
}

impl WindowState {
    /// The saved state, or the default when there's none or it can't be read.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> SavedRect {
        SavedRect {
            x,
            y,
            width: 900.0,
            height: 600.0,
            pixels_per_point: 1.5,
        }
    }

    #[test]
    fn it_round_trips_and_a_bad_file_gives_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(WindowState::load(&path), WindowState::default());

        let state = WindowState {
            rect: Some(rect(10.0, 20.0)),
            maximized: true,
            tray_notice_off: true,
        };
        state.save(&path).unwrap();
        assert_eq!(WindowState::load(&path), state);

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(WindowState::load(&path), WindowState::default());
    }

    #[test]
    fn a_first_run_window_is_the_default_size_centred_on_the_work_area() {
        // A 1920 × 1080 monitor at 100%, with a 48-pixel taskbar along the bottom.
        let r = first_rect([0, 0, 1920, 1032], 1.0);
        assert_eq!([r.width, r.height], DEFAULT_SIZE);
        assert_eq!([r.x, r.y], [370.0, 126.0]);
        // A second monitor to the left, at 150%: in points, and centred on that monitor.
        let r = first_rect([-2880, 0, 0, 1728], 1.5);
        assert_eq!([r.width, r.height], DEFAULT_SIZE);
        assert_eq!([r.x, r.y], [-1920.0 + 370.0, 186.0]);
        assert_eq!(r.pixels_per_point, 1.5);
    }

    #[test]
    fn a_small_work_area_gives_a_smaller_centred_window() {
        // 1366 × 728 at 125%: 1092.8 × 582.4 points of work area.
        let r = first_rect([0, 0, 1366, 728], 1.25);
        assert!((r.width - 1092.8 * WORK_AREA_SHARE).abs() < 0.01, "{r:?}");
        assert!((r.height - 582.4 * WORK_AREA_SHARE).abs() < 0.01, "{r:?}");
        let centre = |start: f32, size: f32| start + size / 2.0;
        assert!((centre(r.x, r.width) - 1092.8 / 2.0).abs() <= 0.5, "{r:?}");
        assert!((centre(r.y, r.height) - 582.4 / 2.0).abs() <= 0.5, "{r:?}");
        // Smaller than the least size: the least size, still centred.
        let r = first_rect([100, 50, 900, 550], 1.0);
        assert_eq!([r.width, r.height], MIN_SIZE);
        assert_eq!([r.x, r.y], [140.0, 60.0]);
        // A scale that makes no sense counts as 100%.
        assert_eq!(first_rect([0, 0, 1920, 1032], 0.0).pixels_per_point, 1.0);
    }

    #[test]
    fn pixels_scale_the_points() {
        assert_eq!(rect(100.0, 10.0).pixels(), [150, 15, 1500, 915]);
    }

    #[test]
    fn a_rect_off_every_monitor_or_too_small_isn_t_used() {
        let on = |_: [i32; 4]| true;
        let off = |_: [i32; 4]| false;
        assert!(rect(0.0, 0.0).usable(on));
        assert!(!rect(0.0, 0.0).usable(off));
        let tiny = SavedRect {
            width: 50.0,
            ..rect(0.0, 0.0)
        };
        assert!(!tiny.usable(on));
        let broken = SavedRect {
            x: f32::NAN,
            ..rect(0.0, 0.0)
        };
        assert!(!broken.usable(on));
    }
}
