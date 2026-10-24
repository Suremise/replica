//! Three things for the exes:
//!
//! - THIRD-PARTY-NOTICES.txt (`scripts/third-party-notices.ps1`), embedded for the window's
//!   About tab, or nothing when it hasn't been made: such a build then says where the notices are.
//! - The app icon's 20, 32, 40, 48 and 64 px images as raw RGBA (`app-icon-<size>.rgba`), for the
//!   window's sidebar and About page, so the window decodes nothing at start-up.
//! - On Windows, a resource with the app's icon and a version naming Replica, so Explorer, the
//!   taskbar and notifications show "Replica" rather than the exe's file name, the exes' Details
//!   show the crate's version, and the installer can compare the files it replaces
//!   (`scripts/package.ps1` checks both exes carry it).

use std::path::Path;

/// The icon's sizes the window draws, for 20 and 40 points at 100% to 200% (`ui::app_icon`).
const SIDEBAR_ICON_SIZES: [u32; 5] = [20, 32, 40, 48, 64];

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let out = Path::new(&out_dir);

    let notices = manifest.join("../../THIRD-PARTY-NOTICES.txt");
    println!("cargo:rerun-if-changed={}", notices.display());
    let text = std::fs::read_to_string(&notices).unwrap_or_default();
    std::fs::write(out.join("third-party-notices.txt"), text).expect("OUT_DIR can be written to");

    sidebar_icons(manifest, out);

    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        resources(manifest, out);
    }
}

/// Each of [`SIDEBAR_ICON_SIZES`] from `replica.ico` (whose images are PNGs), decoded to RGBA.
fn sidebar_icons(manifest: &Path, out: &Path) {
    let icon = manifest.join("../../assets/icon/replica.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let ico = std::fs::read(&icon).expect("assets/icon/replica.ico");
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    for size in SIDEBAR_ICON_SIZES {
        let entry = (0..usize::from(u16_at(4)))
            .map(|i| 6 + 16 * i)
            .find(|&e| u32::from(ico[e]) == size)
            .unwrap_or_else(|| panic!("replica.ico has no {size} px image"));
        let (len, at) = (u32_at(entry + 8) as usize, u32_at(entry + 12) as usize);
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&ico[at..at + len]));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
        let mut reader = decoder.read_info().expect("the icon's image is a PNG");
        let mut rgba = vec![0; reader.output_buffer_size().expect("a small image")];
        let info = reader
            .next_frame(&mut rgba)
            .expect("the icon's PNG decodes");
        assert_eq!(
            (info.width, info.height, info.color_type, info.bit_depth),
            (size, size, png::ColorType::Rgba, png::BitDepth::Eight),
            "the {size} px image is 8-bit RGBA"
        );
        rgba.truncate(info.buffer_size());
        std::fs::write(out.join(format!("app-icon-{size}.rgba")), rgba)
            .expect("OUT_DIR can be written to");
    }
}

/// The icon and version resource, compiled with the Windows SDK's rc.exe.
fn resources(manifest: &Path, out: &Path) {
    let icon = manifest.join("../../assets/icon/replica.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = env!("CARGO_PKG_VERSION");
    // LICENSE's own "Copyright (c) <year> <holder>" line, and its holder for CompanyName.
    let license = manifest.join("../../LICENSE");
    println!("cargo:rerun-if-changed={}", license.display());
    let copyright = std::fs::read_to_string(&license)
        .expect("LICENSE")
        .lines()
        .find(|l| l.starts_with("Copyright"))
        .expect("LICENSE has a Copyright line")
        .trim()
        .to_owned();
    let holder = copyright
        .split_whitespace()
        .skip_while(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !holder.is_empty(),
        "LICENSE's Copyright line names its holder after the year"
    );
    let mut parts: Vec<u16> = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    parts.resize(4, 0);
    let numbers = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let rc = format!(
        r#"#include <winver.h>
1 ICON "{icon}"
VS_VERSION_INFO VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "080904b0"
    BEGIN
      VALUE "CompanyName", "{holder}"
      VALUE "FileDescription", "Replica"
      VALUE "FileVersion", "{version}"
      VALUE "LegalCopyright", "{copyright}"
      VALUE "ProductName", "Replica"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x809, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "/"),
        // rc.exe's strings write a quote as "".
        copyright = copyright.replace('"', "\"\""),
        holder = holder.replace('"', "\"\""),
    );
    let path = out.join("replica.rc");
    std::fs::write(&path, rc).expect("OUT_DIR can be written to");
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_optional()
        .expect("the Windows resource compiles (rc.exe, from the Windows SDK)");
}
