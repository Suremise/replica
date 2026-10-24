//! Shared by the integration tests that need the pinned restic.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The pinned restic (`scripts/fetch-restic.ps1`), or `REPLICA_RESTIC`. `None` means skip the
/// test, unless `REPLICA_REQUIRE_RESTIC=1` (as CI on Windows sets), when it fails instead.
pub fn restic() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("REPLICA_RESTIC") {
        return Some(PathBuf::from(path));
    }
    let exe = if cfg!(windows) {
        "restic.exe"
    } else {
        "restic"
    };
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/restic/bin")
        .join(exe);
    if bundled.is_file() {
        return Some(bundled);
    }
    assert!(
        std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
        "restic is required (REPLICA_REQUIRE_RESTIC=1) but wasn't found: run scripts/fetch-restic.ps1"
    );
    eprintln!("skipped: no restic (run scripts/fetch-restic.ps1)");
    None
}
