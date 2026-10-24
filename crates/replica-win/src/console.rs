//! Reading a password typed at the console without showing it.
//!
//! On a console, echo is turned off while the line is read and turned back on after, even if
//! reading fails. Piped input (a script, a test) has nothing to hide and is read as it is. The
//! text never goes on a command line or in a log, so it isn't in the shell's history either.

use std::io::{self, BufRead};

use replica_core::platform::Secret;

use crate::win32;

/// Whether this process's console has a window on screen: false for one started without a
/// console, or with `CREATE_NO_WINDOW` (how Replica starts every child).
pub fn has_console_window() -> bool {
    win32::console::has_window()
}

/// Reads one line from standard input without echoing it, its line ending removed. Nothing
/// read (the input closed) is an `UnexpectedEof` error.
pub fn read_secret_line() -> io::Result<Secret> {
    let echo_off = win32::console::echo_off()?;
    let mut line = String::new();
    let read = io::stdin().lock().read_line(&mut line);
    if echo_off.is_some() {
        // The Enter wasn't echoed either.
        println!();
    }
    drop(echo_off);
    if read? == 0 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let text = line.trim_end_matches(['\r', '\n']).to_owned();
    line.clear();
    Ok(Secret::new(text))
}
