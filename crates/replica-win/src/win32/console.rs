//! The console's echo, for reading a password without showing it.

use std::io;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetConsoleWindow, GetStdHandle,
    STD_INPUT_HANDLE, SetConsoleMode,
};

/// Whether this process's console has a window: false with no console, or one started with
/// `CREATE_NO_WINDOW`.
pub fn has_window() -> bool {
    // SAFETY: takes nothing and only returns a handle, or null.
    !unsafe { GetConsoleWindow() }.0.is_null()
}

/// Echo is off on standard input until this is dropped, which puts the console's mode back.
pub struct EchoOff {
    handle: HANDLE,
    mode: CONSOLE_MODE,
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: the handle is this process's standard input, and the mode the one read from it.
        let _ = unsafe { SetConsoleMode(self.handle, self.mode) };
    }
}

/// Turns echo off on standard input if it's a console; `None` if it isn't one (piped input).
pub fn echo_off() -> io::Result<Option<EchoOff>> {
    // SAFETY: plain calls on this process's standard input handle, with a mode it owns.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE).map_err(io::Error::from)?;
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(handle, &mut mode).is_err() {
            return Ok(None);
        }
        SetConsoleMode(handle, CONSOLE_MODE(mode.0 & !ENABLE_ECHO_INPUT.0))
            .map_err(io::Error::from)?;
        Ok(Some(EchoOff { handle, mode }))
    }
}
