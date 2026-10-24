//! Path checks the engines share: where a path really leads, and whether two paths overlap.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Where a path really leads: made absolute, `.` and `..` resolved by text (as Windows and
/// restic both do), then its longest existing part resolved through links and junctions, with
/// the parts that don't exist yet added back.
pub fn real_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut clean = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    let mut existing = clean.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return Err(e);
                };
                missing.push(name.to_owned());
                existing = parent;
            }
            Err(e) => return Err(e),
        }
    }
    let mut real = std::fs::canonicalize(existing)?;
    real.extend(missing.iter().rev());
    Ok(real)
}

/// A canonical Windows path without its `\\?\` prefix, for tools and for people.
pub fn without_verbatim_prefix(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
    }
}

/// Whether one path is the other, or inside it, compared as Windows does (ignoring case, `/`
/// and `\` alike, no trailing separator). Compare [`real_path`]s, not paths as typed.
pub fn overlap(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let mut s = p.to_string_lossy().replace('\\', "/").to_lowercase();
        if let Some(rest) = s.strip_prefix("//?/unc/") {
            s = format!("//{rest}");
        } else if let Some(rest) = s.strip_prefix("//?/") {
            s = rest.to_string();
        }
        s.trim_end_matches('/').to_string()
    };
    let (a, b) = (norm(a), norm(b));
    let within =
        |inner: &str, outer: &str| inner == outer || inner.starts_with(&format!("{outer}/"));
    within(&a, &b) || within(&b, &a)
}

/// A path as typed (in the settings) as the folder Windows would open, by its components:
/// `/` and `\` alike, a `\\?\` or `\\?\UNC\` prefix and a UNC path's leading `\\` dropped,
/// empty and `.` components dropped, `..` taking off the component before it (never one of the
/// first `root`: a share's server and name), and the trailing dots and spaces Windows ignores
/// taken off each name, all in lower case. For comparing configured targets, which may not exist
/// yet; compare [`real_path`]s for paths on disk.
pub fn typed_components(path: &str, root: usize) -> Vec<String> {
    let text = path.replace('/', "\\");
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| text.strip_prefix(r"\\?\"))
        .unwrap_or(&text);
    let mut parts: Vec<String> = Vec::new();
    for part in text.split('\\') {
        let name = part.trim_end_matches(['.', ' ']);
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() > root {
                    parts.pop();
                }
            }
            // A name that's only dots and spaces ("..." or ". ") is no folder Windows opens:
            // kept as typed, so it never matches a real one.
            _ if name.is_empty() => parts.push(part.to_lowercase()),
            _ => parts.push(name.to_lowercase()),
        }
    }
    parts
}

/// Whether one list of [`typed_components`] is the other, or inside it.
pub fn components_overlap(a: &[String], b: &[String]) -> bool {
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

/// Whether a real path is the root of a drive or share (`C:\`, `\\nas\share`, `/`).
pub fn is_root(real: &Path) -> bool {
    let text = without_verbatim_prefix(real).replace('\\', "/");
    let text = text.trim_end_matches('/');
    if text.is_empty() {
        return true;
    }
    if let Some(unc) = text.strip_prefix("//") {
        return unc.split('/').filter(|p| !p.is_empty()).count() <= 2;
    }
    let b = text.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_paths_resolve_relative_paths_from_the_current_folder() {
        let here = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert_eq!(
            real_path(Path::new("no-such-dir/../x")).unwrap(),
            here.join("x")
        );
    }

    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(without_verbatim_prefix(Path::new(r"\\?\C:\a")), r"C:\a");
        assert_eq!(
            without_verbatim_prefix(Path::new(r"\\?\UNC\nas\s\a")),
            r"\\nas\s\a"
        );
        assert_eq!(without_verbatim_prefix(Path::new("/tmp/a")), "/tmp/a");
    }

    #[test]
    fn overlap_is_by_whole_components() {
        assert!(overlap(Path::new(r"C:\a"), Path::new("c:/A/b")));
        assert!(overlap(Path::new(r"C:\a\b\"), Path::new(r"C:\a")));
        assert!(!overlap(Path::new(r"C:\ab"), Path::new(r"C:\a")));
        assert!(overlap(Path::new(r"\\?\C:\a"), Path::new(r"C:\a")));
        assert!(overlap(
            Path::new(r"\\?\UNC\nas\s\x"),
            Path::new(r"\\nas\s")
        ));
    }

    #[test]
    fn typed_paths_compare_as_the_folder_windows_opens() {
        let drive = |p| typed_components(p, 0);
        let unc = |p| typed_components(p, 2);
        for same in [
            "m", "./m", r".\m\", "M", "m.", "m ", "x/../m", r"m\.", "//m//",
        ] {
            assert_eq!(drive(same), ["m"], "{same}");
        }
        assert_eq!(drive(""), Vec::<String>::new(), "the volume's root");
        assert_eq!(drive("a/b/../c"), ["a", "c"]);
        assert_eq!(unc(r"\\NAS\Share\.\m"), ["nas", "share", "m"]);
        assert_eq!(unc(r"\\?\UNC\nas\share\m"), ["nas", "share", "m"]);
        assert_eq!(
            unc(r"\\nas\share\..\..\m"),
            ["nas", "share", "m"],
            "`..` stops at the share"
        );
        assert!(components_overlap(&drive("m"), &drive("./m/inner")));
        assert!(components_overlap(&drive(""), &drive("anything")));
        assert!(!components_overlap(&drive("m"), &drive("m2")));
    }

    #[test]
    fn roots() {
        for root in [
            r"C:\",
            "D:",
            r"\\?\C:\",
            r"\\nas\share",
            r"\\?\UNC\nas\share\",
            "/",
        ] {
            assert!(is_root(Path::new(root)), "{root}");
        }
        for not in [r"C:\a", r"\\nas\share\x", "/home"] {
            assert!(!is_root(Path::new(not)), "{not}");
        }
    }
}
