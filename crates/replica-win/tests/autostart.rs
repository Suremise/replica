//! Start with Windows, in a test-only key (`HKCU\Software\ReplicaTest\<pid>-<test>`) removed
//! when the test ends: never the Run key, so nothing here runs at logon.
#![cfg(windows)]

use std::path::Path;

use replica_win::autostart::{AutoStart, RUN_KEY, command_for, delete_test_key, test_key};

/// A `Replica` entry in the test's own key, which is removed when dropped.
struct TestValue(AutoStart, String);

impl TestValue {
    fn new(test: &str) -> Self {
        let name = format!("{}-{test}", std::process::id());
        Self(AutoStart::new("Replica").in_key(&test_key(&name)), name)
    }
}

impl Drop for TestValue {
    fn drop(&mut self) {
        let _ = delete_test_key(&self.1);
    }
}

/// The key is the test's own, and the cleanup takes only test keys and their entries.
#[test]
fn a_test_entry_is_in_its_own_key() {
    let value = TestValue::new("isolated");
    let exe = Path::new(r"C:\Program Files\Replica Test\replica-tray.exe");
    assert!(
        test_key(&value.1).starts_with(r"Software\ReplicaTest\")
            && !test_key(&value.1).eq_ignore_ascii_case(RUN_KEY)
    );
    value.0.enable(exe).unwrap();
    assert!(value.0.starts(exe).unwrap());
    assert!(delete_test_key(r"..\Microsoft").is_err());
    assert!(delete_test_key("").is_err());

    delete_test_key(&value.1).unwrap();
    assert_eq!(value.0.command().unwrap(), None);
}

#[test]
fn enable_points_at_the_exe_and_disable_removes_it() {
    let value = TestValue::new("toggle");
    let a = &value.0;
    let exe = Path::new(r"C:\Program Files\Replica Test\replica-tray.exe");
    assert_eq!(a.command().unwrap(), None);
    assert!(!a.starts(exe).unwrap());

    a.enable(exe).unwrap();
    assert_eq!(a.command().unwrap(), Some(command_for(exe, "")));
    assert_eq!(
        command_for(exe, ""),
        r#""C:\Program Files\Replica Test\replica-tray.exe""#
    );
    assert!(a.starts(exe).unwrap());
    assert!(
        !a.starts(Path::new(r"D:\elsewhere\replica-tray.exe"))
            .unwrap()
    );

    a.disable().unwrap();
    a.disable().unwrap();
    assert_eq!(a.command().unwrap(), None);
}

/// An entry of the same name that starts something else is never removed.
#[test]
fn turning_it_off_leaves_another_program_s_entry_alone() {
    let value = TestValue::new("other");
    let a = &value.0;
    let other = Path::new(r"C:\Old Replica\replica.exe");
    a.enable(other).unwrap();
    a.apply(false, Path::new(r"D:\new\replica-tray.exe"))
        .unwrap();
    assert_eq!(a.command().unwrap(), Some(command_for(other, "")));
}

#[test]
fn apply_follows_the_setting_and_repoints_a_moved_copy() {
    let value = TestValue::new("apply");
    let a = &value.0;
    let old = Path::new(r"D:\old\replica-tray.exe");
    let new = Path::new(r"D:\new\replica-tray.exe");
    a.apply(true, old).unwrap();
    assert!(a.starts(old).unwrap());
    a.apply(true, new).unwrap();
    assert!(
        a.starts(new).unwrap(),
        "moved: points at the copy running now"
    );
    a.apply(false, new).unwrap();
    assert_eq!(a.command().unwrap(), None);
}

/// An entry an older version wrote without arguments (or with others) is rewritten with this
/// version's, so upgrading changes what a login runs; turning it off still removes it.
#[test]
fn apply_rewrites_an_entry_with_other_arguments() {
    let value = TestValue::new("args");
    let plain = &value.0;
    let background = plain.clone().with_args("--background");
    let exe = Path::new(r"C:\Program Files\Replica Test\replica-tray.exe");
    assert_eq!(
        background.command_for(exe),
        r#""C:\Program Files\Replica Test\replica-tray.exe" --background"#
    );

    plain.apply(true, exe).unwrap();
    assert!(!background.starts(exe).unwrap());
    background.apply(true, exe).unwrap();
    assert_eq!(plain.command().unwrap(), Some(background.command_for(exe)));
    assert!(background.starts(exe).unwrap());
    assert!(!plain.starts(exe).unwrap(), "the arguments count");

    background.apply(false, exe).unwrap();
    assert_eq!(plain.command().unwrap(), None);
}
