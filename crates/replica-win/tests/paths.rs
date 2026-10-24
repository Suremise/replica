//! `AppPaths::detect` as a real exe sees it: this test exe copies itself into a temp folder, sets
//! up that folder, and runs the copy, whose `child` test prints what it detected.
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use replica_win::paths::AppPaths;

const CHILD: &str = "REPLICA_PATHS_TEST_CHILD";

/// Run only as the copy: prints the detected paths.
#[test]
fn child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let paths = AppPaths::detect("Replica").unwrap();
    println!("MODE={}", paths.mode().as_str());
    println!("ROOT={}", paths.root().display());
    println!("WRITABLE={}", paths.is_writable());
}

struct Detected {
    mode: String,
    root: PathBuf,
    writable: bool,
}

struct Setup {
    _temp: tempfile::TempDir,
    exe_dir: PathBuf,
    appdata: PathBuf,
}

fn setup() -> Setup {
    let temp = tempfile::tempdir().unwrap();
    let exe_dir = temp.path().join("app");
    let appdata = temp.path().join("appdata");
    fs::create_dir_all(&exe_dir).unwrap();
    fs::create_dir_all(&appdata).unwrap();
    let me = std::env::current_exe().unwrap();
    fs::copy(&me, exe_dir.join(me.file_name().unwrap())).unwrap();
    Setup {
        _temp: temp,
        exe_dir,
        appdata,
    }
}

fn detect(setup: &Setup) -> Detected {
    let me = std::env::current_exe().unwrap();
    let output = Command::new(setup.exe_dir.join(me.file_name().unwrap()))
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("APPDATA", &setup.appdata)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = |name: &str| {
        let prefix = format!("{name}=");
        stdout
            .lines()
            .find_map(|l| l.split_once(&prefix).map(|(_, v)| v))
            .unwrap_or_else(|| panic!("no {name} in {stdout}"))
            .to_owned()
    };
    Detected {
        mode: value("MODE"),
        root: PathBuf::from(value("ROOT")),
        writable: value("WRITABLE") == "true",
    }
}

fn same(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
}

#[test]
fn the_marker_makes_the_copy_portable() {
    let setup = setup();
    fs::write(setup.exe_dir.join("portable"), "").unwrap();
    let detected = detect(&setup);
    assert_eq!(detected.mode, "portable");
    assert!(same(&detected.root, &setup.exe_dir));
    assert!(detected.writable);
    assert!(!setup.appdata.join("Replica").exists());
}

#[test]
fn settings_beside_the_exe_make_the_copy_portable() {
    let setup = setup();
    fs::write(setup.exe_dir.join("settings.toml"), "").unwrap();
    let detected = detect(&setup);
    assert_eq!(detected.mode, "portable");
    assert!(same(&detected.root, &setup.exe_dir));
}

#[test]
fn without_either_the_copy_is_installed_in_appdata() {
    let setup = setup();
    let detected = detect(&setup);
    assert_eq!(detected.mode, "installed");
    assert!(same(&detected.root, &setup.appdata.join("Replica")));
    assert!(detected.writable);
}

/// Denies everyone write access to the folder until dropped.
struct DenyWrite(PathBuf);

impl DenyWrite {
    fn new(dir: &Path) -> Self {
        icacls(dir, &["/deny", "*S-1-1-0:(W)"]);
        Self(dir.to_path_buf())
    }
}

impl Drop for DenyWrite {
    fn drop(&mut self) {
        icacls(&self.0, &["/remove:d", "*S-1-1-0"]);
    }
}

fn icacls(dir: &Path, args: &[&str]) {
    let status = Command::new("icacls")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "icacls {args:?} failed");
}

#[test]
fn a_portable_folder_that_cant_be_written_stays_put() {
    let setup = setup();
    fs::write(setup.exe_dir.join("portable"), "").unwrap();
    let deny = DenyWrite::new(&setup.exe_dir);
    assert!(
        fs::write(setup.exe_dir.join("probe"), "").is_err(),
        "the deny didn't apply"
    );
    let detected = detect(&setup);
    drop(deny);
    assert_eq!(detected.mode, "portable");
    assert!(same(&detected.root, &setup.exe_dir));
    assert!(!detected.writable);
    // Nothing went anywhere else.
    assert!(!setup.appdata.join("Replica").exists());
    let names: Vec<_> = fs::read_dir(&setup.exe_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}
