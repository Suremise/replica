//! Volumes on this PC and shares that can't be reached. No external drive, NAS or real share is
//! needed: the system drive stands in for a drive target, and the shares don't exist.
#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use replica_core::job::VolumeRef;
use replica_core::platform::{Credential, Secret, ShareStatus, TargetResolver};
use replica_win::targets::{self, WindowsTargets};

fn system_root() -> String {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    format!("{drive}\\")
}

#[test]
fn the_system_drive_is_listed_with_its_volume_id() {
    let root = system_root();
    let id = targets::volume_id_of(&root).unwrap();
    assert!(targets::normalize_volume_id(&id).is_some(), "{id}");
    let volumes = targets::list_volumes().unwrap();
    let system = volumes
        .iter()
        .find(|v| v.id.eq_ignore_ascii_case(&id))
        .unwrap_or_else(|| panic!("{id} isn't listed: {volumes:?}"));
    assert_eq!(system.root(), PathBuf::from(&root));
    assert!(system.total_bytes.unwrap() > 0);
    assert!(system.free_bytes.unwrap() <= system.total_bytes.unwrap());
    assert!(!system.removable);
}

#[test]
fn the_system_drive_is_found_by_its_id_and_by_its_label() {
    let root = system_root();
    let resolver = WindowsTargets::new();
    let id = targets::volume_id_of(&root).unwrap();
    let by_id = VolumeRef {
        volume_id: Some(id.clone()),
        label: None,
    };
    assert_eq!(
        resolver.locate_volume(&by_id).unwrap(),
        Some(PathBuf::from(&root))
    );

    let volumes = targets::list_volumes().unwrap();
    let label = volumes
        .iter()
        .find(|v| v.id.eq_ignore_ascii_case(&id))
        .and_then(|v| v.label.clone());
    let unique = |l: &str| {
        volumes
            .iter()
            .filter(|v| {
                v.label
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(l))
            })
            .count()
            == 1
    };
    match label {
        Some(label) if unique(&label) => {
            let by_label = VolumeRef {
                volume_id: None,
                label: Some(label.to_lowercase()),
            };
            assert_eq!(
                resolver.locate_volume(&by_label).unwrap(),
                Some(PathBuf::from(&root))
            );
        }
        // No label, or a label another volume shares: the ID alone is checked.
        _ => {}
    }

    let missing = VolumeRef {
        volume_id: Some("{00000000-0000-0000-0000-000000000000}".into()),
        label: Some(format!("replica-test-no-such-label-{}", std::process::id())),
    };
    assert_eq!(resolver.locate_volume(&missing).unwrap(), None);
}

fn quick() -> WindowsTargets {
    let mut resolver = WindowsTargets::new();
    resolver.probe_timeout = Duration::from_secs(2);
    resolver.connect_timeout = Duration::from_secs(15);
    resolver
}

fn assert_not_available(status: ShareStatus, started: Instant, resolver: &WindowsTargets) {
    assert!(matches!(status, ShareStatus::NotAvailable(_)), "{status:?}");
    // Bounded by the resolver's limit, with room for a loaded machine.
    assert!(
        started.elapsed() < resolver.connect_timeout + Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    assert!(resolver.connected_by_us().is_empty());
}

#[test]
fn a_share_that_doesnt_exist_on_this_pc_is_not_available() {
    let resolver = quick();
    let share = format!(
        r"\\127.0.0.1\replica-test-no-such-share-{}",
        std::process::id()
    );
    let started = Instant::now();
    let status = resolver
        .connect_share(&format!(r"{share}\hive"), None)
        .unwrap();
    assert_not_available(status, started, &resolver);
}

#[test]
fn a_server_that_doesnt_exist_is_not_available_and_gets_no_login() {
    let resolver = quick();
    // `.invalid` never resolves (RFC 2606), so no login is ever sent anywhere.
    let unc = r"\\replica-test-no-such-host.invalid\backup\hive";
    let login = Credential {
        username: Some("replica-test".into()),
        secret: Secret::new("not-a-real-password"),
    };
    let started = Instant::now();
    let status = resolver.connect_share(unc, Some(&login)).unwrap();
    assert_not_available(status, started, &resolver);
}

#[test]
fn a_path_that_isnt_unc_is_an_error() {
    let resolver = quick();
    for bad in [r"C:\backup", r"\\?\C:\backup", r"\\nas"] {
        assert!(resolver.connect_share(bad, None).is_err(), "{bad}");
    }
}
