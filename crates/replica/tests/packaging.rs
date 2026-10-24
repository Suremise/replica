//! The packaging scripts' gates, in temp folders only, with no network:
//!
//! - a cached restic.exe is kept only when it's the pinned exe, byte for byte: one that still
//!   reports the pinned version but was changed is refused;
//! - the MSI's own text comes from Fluent (`locales/en/installer.ftl`).

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// PowerShell 7, which the scripts need.
fn pwsh() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("pwsh.exe"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
            "PowerShell 7 (pwsh) is required here but wasn't found"
        );
    }
    found
}

/// The pinned restic, as fetched for development and CI.
fn pinned_restic() -> Option<PathBuf> {
    let exe = workspace().join("third_party/restic/bin/restic.exe");
    if !exe.is_file() {
        assert!(
            std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
            "restic is required (REPLICA_REQUIRE_RESTIC=1) but wasn't found"
        );
        return None;
    }
    Some(exe)
}

/// A copy of the fetch script and the pin, as the repository lays them out, with `exe` as the
/// cached restic.
fn layout(temp: &Path, exe: &[u8]) {
    std::fs::create_dir_all(temp.join("scripts")).unwrap();
    std::fs::create_dir_all(temp.join("third_party/restic/bin")).unwrap();
    std::fs::copy(
        workspace().join("scripts/fetch-restic.ps1"),
        temp.join("scripts/fetch-restic.ps1"),
    )
    .unwrap();
    std::fs::copy(
        workspace().join("third_party/restic/VERSION"),
        temp.join("third_party/restic/VERSION"),
    )
    .unwrap();
    std::fs::write(temp.join("third_party/restic/bin/restic.exe"), exe).unwrap();
}

/// Runs the copied fetch script with `-VerifyOnly` (it never downloads): its exit code and
/// what it printed.
fn verify(pwsh: &Path, temp: &Path) -> (i32, String) {
    let out = Command::new(pwsh)
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(temp.join("scripts/fetch-restic.ps1"))
        .arg("-VerifyOnly")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn a_changed_restic_that_still_reports_the_pinned_version_is_refused() {
    let (Some(pwsh), Some(restic)) = (pwsh(), pinned_restic()) else {
        return;
    };
    let official = std::fs::read(&restic).unwrap();

    let temp = tempfile::tempdir().unwrap();
    let mut changed = official.clone();
    changed.push(0);
    layout(temp.path(), &changed);
    // It still runs, and says it's the pinned version.
    let reported = Command::new(temp.path().join("third_party/restic/bin/restic.exe"))
        .arg("version")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&reported.stdout).starts_with("restic 0.19.1 "));
    let (code, text) = verify(&pwsh, temp.path());
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("isn't the pinned restic"), "{text}");

    let temp = tempfile::tempdir().unwrap();
    layout(temp.path(), &official);
    let (code, text) = verify(&pwsh, temp.path());
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("SHA-256 verified"), "{text}");
}

/// The DLLs a PE file imports, read from its import directory.
fn imported_dlls(exe: &Path) -> Vec<String> {
    let b = std::fs::read(exe).unwrap();
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize;
    let pe = u32_at(0x3C);
    assert_eq!(&b[pe..pe + 4], b"PE\0\0", "{}", exe.display());
    let sections = u16_at(pe + 6);
    let optional = pe + 24;
    let size_of_optional = u16_at(pe + 20);
    assert_eq!(u16_at(optional), 0x20B, "a 64-bit exe");
    // Data directory 1 (imports) in the PE32+ optional header.
    let import_rva = u32_at(optional + 112 + 8);
    let table = optional + size_of_optional;
    let to_offset = |rva: usize| {
        (0..sections)
            .map(|i| table + i * 40)
            .find_map(|s| {
                let (va, size, raw) = (
                    u32_at(s + 12),
                    u32_at(s + 8).max(u32_at(s + 16)),
                    u32_at(s + 20),
                );
                (rva >= va && rva < va + size).then(|| rva - va + raw)
            })
            .expect("the RVA is in a section")
    };
    let mut dlls = Vec::new();
    let mut entry = to_offset(import_rva);
    loop {
        let name_rva = u32_at(entry + 12);
        if name_rva == 0 {
            break;
        }
        let at = to_offset(name_rva);
        let end = at + b[at..].iter().position(|&c| c == 0).unwrap();
        dlls.push(String::from_utf8_lossy(&b[at..end]).to_ascii_lowercase());
        entry += 20;
    }
    dlls.sort();
    dlls
}

/// Whether an exe may import a DLL: one of Windows' own (an API set, or in System32), but no C
/// runtime DLL at all, as the C runtime is linked in (`.cargo/config.toml`). That includes the
/// Universal CRT's API sets, which Windows has but a static build doesn't need.
fn is_system_dll(dll: &str) -> bool {
    let system32 =
        Path::new(&std::env::var("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32");
    (dll.starts_with("api-ms-win-") || dll.starts_with("ext-ms-") || system32.join(dll).is_file())
        && !dll.starts_with("api-ms-win-crt-")
        && !dll.starts_with("vcruntime")
        && !dll.starts_with("msvcp")
        && dll != "ucrtbase.dll"
}

#[test]
fn the_exes_need_no_c_runtime_dll() {
    for exe in [
        env!("CARGO_BIN_EXE_replica"),
        env!("CARGO_BIN_EXE_replica-tray"),
    ] {
        let dlls = imported_dlls(Path::new(exe));
        assert!(dlls.iter().any(|d| d == "kernel32.dll"), "{exe}: {dlls:?}");
        let foreign: Vec<&String> = dlls.iter().filter(|d| !is_system_dll(d)).collect();
        assert!(
            foreign.is_empty(),
            "{exe} imports C runtime DLLs, or DLLs that aren't part of Windows; the C runtime \
             should be linked in (.cargo/config.toml): {foreign:?}"
        );
    }
}

#[test]
fn the_msi_s_own_text_comes_from_fluent() {
    let ftl = std::fs::read_to_string(workspace().join("locales/en/installer.ftl")).unwrap();
    let script = std::fs::read_to_string(workspace().join("scripts/package.ps1")).unwrap();
    let wxs = std::fs::read_to_string(workspace().join("installer/replica.wxs")).unwrap();

    // Every key package.ps1 asks for is a one-line, plain message in installer.ftl.
    let mut keys = Vec::new();
    let mut rest = script.as_str();
    while let Some(at) = rest.find("'installer-") {
        let key = &rest[at + 1..];
        let end = key.find('\'').unwrap();
        keys.push(key[..end].to_owned());
        rest = &key[end..];
    }
    keys.sort();
    keys.dedup();
    assert_eq!(
        keys,
        ["installer-downgrade", "installer-shortcut-description"]
    );
    for key in &keys {
        let line = ftl
            .lines()
            .find(|l| l.starts_with(&format!("{key} =")))
            .unwrap_or_else(|| panic!("installer.ftl has no {key}"));
        let value = line.split_once('=').unwrap().1.trim();
        assert!(!value.is_empty() && !value.contains(['{', '}']), "{line}");
    }
    // They're also in the app's English, so the strings tests see them.
    let english =
        replica_core::i18n::Localizer::new(replica_core::i18n::embedded::LOCALES, "en", "en");
    for key in &keys {
        assert!(english.has(key), "{key}");
    }

    // The WiX source takes them as variables, with no English of its own in their place.
    assert!(wxs.contains(r#"DowngradeErrorMessage="$(var.DowngradeMessage)""#));
    assert!(wxs.contains(r#"Description="$(var.ShortcutDescription)""#));
    for literal in ["A newer version", "Versioned backups"] {
        assert!(!wxs.contains(literal), "{literal}");
    }
}
