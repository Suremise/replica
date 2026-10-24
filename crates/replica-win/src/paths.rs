//! Where an app keeps its settings, data and logs: portable (beside the exe) or installed
//! (`%APPDATA%\<App>\`).
//!
//! Self-contained so other apps can copy it: the app's name is a parameter and
//! nothing here is Replica's. It follows the common pattern (ShareX, Notepad++, KeePass): a file
//! named `portable`, or a `settings.toml`, beside the exe makes the copy portable.
//!
//! A portable folder that can't be written to stays the app's folder: [`AppPaths::is_writable`]
//! is then false, and the app says so and keeps its settings in memory. Nothing falls back to
//! another folder silently.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The file beside the exe that turns portable mode on (empty, no extension).
pub const PORTABLE_MARKER: &str = "portable";
/// The settings file. Beside the exe, it also turns portable mode on.
pub const SETTINGS_FILE: &str = "settings.toml";
/// The logs folder, inside the app's folder.
pub const LOGS_DIR: &str = "logs";
/// The per-run logs, inside the logs folder.
pub const RUN_LOGS_DIR: &str = "runs";

/// Portable or installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Everything beside the exe.
    Portable,
    /// Everything in `%APPDATA%\<App>\`.
    Installed,
}

impl Mode {
    /// `portable` or `installed`, for `--json` output and logs (not translated).
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Portable => "portable",
            Mode::Installed => "installed",
        }
    }
}

/// Why the app's folder couldn't be worked out.
#[derive(Debug)]
pub enum PathsError {
    /// The exe's own path isn't known.
    NoExe(io::Error),
    /// Installed, and `%APPDATA%` isn't set.
    NoAppData,
}

impl fmt::Display for PathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathsError::NoExe(e) => write!(f, "the exe's folder isn't known: {e}"),
            PathsError::NoAppData => f.write_str("%APPDATA% isn't set"),
        }
    }
}

impl std::error::Error for PathsError {}

/// Where the app's files live, and whether it can write there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    mode: Mode,
    root: PathBuf,
    exe_dir: PathBuf,
    writable: bool,
}

impl AppPaths {
    /// Works out the folders for the running exe, reading `%APPDATA%` for an installed copy.
    pub fn detect(app_name: &str) -> Result<Self, PathsError> {
        let exe = std::env::current_exe().map_err(PathsError::NoExe)?;
        let exe_dir = exe.parent().ok_or_else(|| {
            PathsError::NoExe(io::Error::new(io::ErrorKind::NotFound, "no parent folder"))
        })?;
        let appdata = std::env::var_os("APPDATA").filter(|v| !v.is_empty());
        Self::resolve(app_name, exe_dir, appdata.map(PathBuf::from).as_deref())
    }

    /// Works out the folders for an exe in `exe_dir`, with `%APPDATA%` given. Creates the
    /// installed folder if it's missing; writes nothing anywhere else.
    pub fn resolve(
        app_name: &str,
        exe_dir: &Path,
        appdata: Option<&Path>,
    ) -> Result<Self, PathsError> {
        let (mode, root) = if is_portable(exe_dir) {
            (Mode::Portable, exe_dir.to_path_buf())
        } else {
            let root = appdata.ok_or(PathsError::NoAppData)?.join(app_name);
            // A failure shows as not writable, below.
            let _ = fs::create_dir_all(&root);
            (Mode::Installed, root)
        };
        let writable = can_write(&root);
        Ok(Self {
            mode,
            root,
            exe_dir: exe_dir.to_path_buf(),
            writable,
        })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_portable(&self) -> bool {
        self.mode == Mode::Portable
    }

    /// The app's folder: settings, data and logs all live here.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder the exe runs from.
    pub fn exe_dir(&self) -> &Path {
        &self.exe_dir
    }

    /// Whether the app's folder could be written to when the paths were worked out. When it's
    /// false, keep settings in memory and tell the user; don't write elsewhere.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join(SETTINGS_FILE)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_DIR)
    }

    pub fn run_logs_dir(&self) -> PathBuf {
        self.logs_dir().join(RUN_LOGS_DIR)
    }

    /// A file of the app's in its folder, such as its history database.
    pub fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

fn is_portable(exe_dir: &Path) -> bool {
    exe_dir.join(PORTABLE_MARKER).is_file() || exe_dir.join(SETTINGS_FILE).is_file()
}

/// Creates and removes a probe file: the only reliable test on Windows, where a folder's
/// read-only attribute doesn't stop files being created but its permissions do.
fn can_write(dir: &Path) -> bool {
    let mut name = OsString::from(".write-probe-");
    name.push(std::process::id().to_string());
    let probe = dir.join(name);
    match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let exe_dir = temp.path().join("app");
        let appdata = temp.path().join("appdata");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(&appdata).unwrap();
        (temp, exe_dir, appdata)
    }

    #[test]
    fn the_marker_makes_it_portable() {
        let (_temp, exe_dir, appdata) = folders();
        fs::write(exe_dir.join(PORTABLE_MARKER), "").unwrap();
        let paths = AppPaths::resolve("Replica", &exe_dir, Some(&appdata)).unwrap();
        assert_eq!(paths.mode(), Mode::Portable);
        assert_eq!(paths.root(), exe_dir);
        assert_eq!(paths.settings_file(), exe_dir.join("settings.toml"));
        assert_eq!(paths.run_logs_dir(), exe_dir.join("logs").join("runs"));
        assert!(paths.is_writable());
        assert!(!appdata.join("Replica").exists());
    }

    #[test]
    fn settings_beside_the_exe_make_it_portable() {
        let (_temp, exe_dir, appdata) = folders();
        fs::write(exe_dir.join(SETTINGS_FILE), "").unwrap();
        let paths = AppPaths::resolve("Replica", &exe_dir, Some(&appdata)).unwrap();
        assert_eq!(paths.mode(), Mode::Portable);
    }

    #[test]
    fn a_folder_named_portable_isnt_the_marker() {
        let (_temp, exe_dir, appdata) = folders();
        fs::create_dir(exe_dir.join(PORTABLE_MARKER)).unwrap();
        let paths = AppPaths::resolve("Replica", &exe_dir, Some(&appdata)).unwrap();
        assert_eq!(paths.mode(), Mode::Installed);
    }

    #[test]
    fn installed_lives_in_appdata() {
        let (_temp, exe_dir, appdata) = folders();
        let paths = AppPaths::resolve("Replica", &exe_dir, Some(&appdata)).unwrap();
        assert_eq!(paths.mode(), Mode::Installed);
        assert_eq!(paths.root(), appdata.join("Replica"));
        assert!(paths.root().is_dir());
        assert_eq!(
            paths.file("history.db"),
            appdata.join("Replica").join("history.db")
        );
        assert_eq!(paths.exe_dir(), exe_dir);
        assert!(paths.is_writable());
    }

    #[test]
    fn installed_needs_appdata() {
        let (_temp, exe_dir, _) = folders();
        let error = AppPaths::resolve("Replica", &exe_dir, None).unwrap_err();
        assert!(matches!(error, PathsError::NoAppData));
    }

    #[test]
    fn the_probe_leaves_nothing_behind() {
        let (_temp, exe_dir, _) = folders();
        assert!(can_write(&exe_dir));
        assert_eq!(fs::read_dir(&exe_dir).unwrap().count(), 0);
    }
}
