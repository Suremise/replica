//! Start with Windows: a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! that starts an exe when the user logs on. Per user, no admin rights.
//!
//! Generic, for other apps to copy too: the value's name and the exe's arguments are parameters.

use std::io;
use std::path::Path;

use crate::win32::registry;

/// The Run key, under `HKEY_CURRENT_USER`.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Where tests keep their own keys, under `HKEY_CURRENT_USER`: never the Run key.
pub const TEST_KEYS: &str = r"Software\ReplicaTest";

/// One app's entry in the Run key (or, in tests, in a key of their own).
#[derive(Debug, Clone)]
pub struct AutoStart {
    key: String,
    value: String,
    args: String,
}

impl AutoStart {
    /// The entry called `value` (`Replica`), starting the exe with no arguments.
    pub fn new(value: &str) -> Self {
        Self {
            key: RUN_KEY.to_owned(),
            value: value.to_owned(),
            args: String::new(),
        }
    }

    /// That, under `HKCU\<key>` instead of the Run key: for tests, so they never touch what
    /// runs at logon.
    pub fn in_key(mut self, key: &str) -> Self {
        self.key = key.to_owned();
        self
    }

    /// That, starting the exe with `args` (`--background`).
    pub fn with_args(mut self, args: &str) -> Self {
        self.args = args.to_owned();
        self
    }

    /// The command it writes for `exe`.
    pub fn command_for(&self, exe: &Path) -> String {
        command_for(exe, &self.args)
    }

    /// The command it runs, if it's there.
    pub fn command(&self) -> io::Result<Option<String>> {
        registry::get_string(&self.key, &self.value)
    }

    /// Whether it's there and starts `exe` with its arguments.
    pub fn starts(&self, exe: &Path) -> io::Result<bool> {
        Ok(self.command()?.is_some_and(|c| c == self.command_for(exe)))
    }

    /// Starts `exe` when the user logs on.
    pub fn enable(&self, exe: &Path) -> io::Result<()> {
        registry::set_string(&self.key, &self.value, &self.command_for(exe))
    }

    /// Stops starting with Windows. Not there already is fine.
    pub fn disable(&self) -> io::Result<()> {
        registry::delete_value(&self.key, &self.value).map(|_| ())
    }

    /// Makes the entry match `on`: there and starting `exe` with its arguments (rewritten if a
    /// moved copy runs now, or an older version wrote other arguments), or gone. It only ever
    /// removes an entry that starts an exe of `exe`'s file name, so an entry of the same name
    /// that belongs to something else is left alone.
    pub fn apply(&self, on: bool, exe: &Path) -> io::Result<()> {
        match (on, self.starts(exe)?) {
            (true, false) => self.enable(exe),
            (false, _) if self.command()?.is_some_and(|c| same_file_name(&c, exe)) => {
                self.disable()
            }
            _ => Ok(()),
        }
    }
}

/// Whether `command` (as the Run key holds it, quoted or not) starts an exe named like `exe`.
fn same_file_name(command: &str, exe: &Path) -> bool {
    let path = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => command.split(' ').next().unwrap_or(""),
    };
    match (Path::new(path).file_name(), exe.file_name()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// The exe, quoted (its folder may have spaces), then `args` if there are any.
pub fn command_for(exe: &Path, args: &str) -> String {
    match args {
        "" => format!("\"{}\"", exe.display()),
        _ => format!("\"{}\" {args}", exe.display()),
    }
}

/// A test's own key, `<TEST_KEYS>\<name>`, for [`AutoStart::in_key`].
pub fn test_key(name: &str) -> String {
    format!(r"{TEST_KEYS}\{name}")
}

/// Removes a test's key ([`test_key`]) with everything under it, and nothing else.
pub fn delete_test_key(name: &str) -> io::Result<()> {
    if name.is_empty() || name.contains(['\\', '/']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a test key's name",
        ));
    }
    registry::delete_tree(&test_key(name)).map(|_| ())
}
