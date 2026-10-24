//! An app's own top-level window: its icon from a `.ico` file, whether a remembered position
//! is still on a monitor, and the work area a new one is placed in.
//!
//! Generic, for other apps to copy too: nothing here is Replica's.

use std::io;

use crate::tray::IconFile;
pub use crate::win32::ui::DialogOwner;
use crate::win32::ui::{self, Icon};

/// A window's big and small icons, kept for as long as the window shows them.
pub struct WindowIcons {
    _big: Icon,
    _small: Icon,
}

/// Gives the window `hwnd` icons from `file`, at the sizes its monitor wants. Keep what it
/// returns while the window lives: the window shows those icons.
pub fn set_icons(hwnd: isize, file: IconFile) -> io::Result<WindowIcons> {
    let make = |small: bool| {
        let size = ui::window_icon_size(hwnd, small);
        let (_, image) = file
            .image_for(size as u32)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an .ico file"))?;
        Icon::from_ico_image(image, size)
    };
    let (big, small) = (make(false)?, make(true)?);
    ui::set_window_icons(hwnd, &big, &small);
    Ok(WindowIcons {
        _big: big,
        _small: small,
    })
}

/// Whether the window `hwnd` still exists.
pub fn exists(hwnd: isize) -> bool {
    ui::window_exists(hwnd)
}

/// Brings the window `hwnd` back and forward from another thread, whatever its own thread is
/// doing: shown, restored if minimised, and the dialog it owns in front, if it has one open.
pub fn bring_forward(hwnd: isize) {
    ui::bring_forward(hwnd)
}

/// The top-level windows of process `pid`, as (`HWND`, title, visible): to find an app's own
/// window, such as a child process's in a test, without touching any other.
pub fn process_windows(pid: u32) -> Vec<(isize, String, bool)> {
    ui::process_windows(pid)
}

/// Asks the window `hwnd` to close, as its close button does: one message to that window
/// alone, never input to whatever has the focus.
pub fn post_close(hwnd: isize) -> io::Result<()> {
    ui::post_close(hwnd)
}

/// Whether the window `hwnd` shows (visible, not minimised) and its thread answers within
/// `timeout`: after [`bring_forward`], whether that took. A window whose thread is stuck
/// never does, since what's posted to it waits for that thread.
pub fn responds(hwnd: isize, timeout: std::time::Duration) -> bool {
    ui::window_responds(hwnd, timeout)
}

/// Gives the focus to the window `hwnd`, or to the dialog it owns if one is open. A dialog
/// shows only once its owner has restored, so call it again a moment after [`bring_forward`].
pub fn focus_front(hwnd: isize) {
    ui::focus_front(hwnd)
}

/// Whether any part of a rectangle in screen pixels (left, top, right, bottom) is on a
/// monitor, so a window put there can be seen.
pub fn is_on_screen(rect: [i32; 4]) -> bool {
    ui::rect_on_screen(rect)
}

/// Where a window opening now should go: the work area (left, top, right, bottom in pixels,
/// without the taskbar) of the monitor the pointer is on, or else the primary one, and that
/// monitor's scale (pixels per point). `None` if Windows can't say.
pub fn work_area() -> Option<([i32; 4], f32)> {
    ui::work_area()
}

/// Opens a file or folder with its default program, as a double-click in Explorer does (a log
/// in its viewer, a folder in Explorer).
pub fn open(path: &std::path::Path) -> io::Result<()> {
    ui::shell_open(&path.to_string_lossy())
}
