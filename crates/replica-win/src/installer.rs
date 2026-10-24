//! What an installer leaves for the app's first start: choices made at install time, under
//! `HKCU\Software\<app>\Installer` (the MSI writes them; the app only reads them).
//!
//! Generic, for other apps to copy too: the app's name is a parameter.

use crate::win32::registry;

/// The key an installer writes its choices to.
pub fn key(app: &str) -> String {
    format!(r"Software\{app}\Installer")
}

/// The installer's DWORD choice `name` (`StartWithWindows`), if it made one.
pub fn choice(app: &str, name: &str) -> Option<u32> {
    registry::get_dword(&key(app), name).ok().flatten()
}
