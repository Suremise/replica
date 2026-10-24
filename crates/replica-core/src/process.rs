//! Starting child processes: the one place Replica makes a [`Command`].
//!
//! Every program Replica starts (restic, a step's `cmd`, anything else) is a console program,
//! and Replica's tray and window are a GUI exe with no console. Started plainly, each would get a
//! console window of its own, flashing up whenever a job runs. So every child is started with
//! Windows' `CREATE_NO_WINDOW`: it gets a console without a window, its output still piped back
//! (to the run log and restic's JSON parser). Nothing a child starts can ask for input: there's
//! nowhere to type it.
//!
//! - [`command`] makes every child's [`Command`]; nothing else calls `Command::new` outside tests
//!   (a test in `tests/process.rs` reads the sources to keep it so).
//! - [`set_creation_flags`] is how another creation flag is added (`replica-win`'s job object
//!   needs `CREATE_SUSPENDED`): Rust's `creation_flags` replaces the flags rather than adding to
//!   them, so it goes through here to keep `CREATE_NO_WINDOW`.
//!
//! `replica.exe`, the console CLI, prints to its own terminal as ever; only its children are
//! started window-less. On other platforms there are no such flags and these are plain commands.

use std::ffi::OsStr;
use std::process::Command;

/// Windows' `CREATE_NO_WINDOW`: a console program runs without a console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The creation flags a child is started with: `extra` and always [`CREATE_NO_WINDOW`].
pub fn creation_flags(extra: u32) -> u32 {
    extra | CREATE_NO_WINDOW
}

/// A [`Command`] for `program` that never opens a console window.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    set_creation_flags(&mut command, 0);
    command
}

/// Sets `command`'s creation flags to [`creation_flags`]`(extra)`: `extra` added, and never a
/// console window. (Windows only; elsewhere it does nothing.)
pub fn set_creation_flags(command: &mut Command, extra: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(creation_flags(extra));
    }
    #[cfg(not(windows))]
    let _ = (command, extra);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_child_is_window_less_whatever_else_it_needs() {
        assert_eq!(creation_flags(0), CREATE_NO_WINDOW);
        let suspended = 0x0000_0004;
        assert_eq!(creation_flags(suspended), suspended | CREATE_NO_WINDOW);
    }

    #[test]
    fn a_command_still_runs_and_its_output_comes_back() {
        let output = if cfg!(windows) {
            command("cmd").args(["/D", "/C", "echo hello"]).output()
        } else {
            command("sh").args(["-c", "echo hello"]).output()
        }
        .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }
}
