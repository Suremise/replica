//! File operations std doesn't have.

use std::io;
use std::path::Path;

/// Renames `from` to `to` unless `to` exists, as one step: an existing `to` is
/// `io::ErrorKind::AlreadyExists` and stays as it was. std's `rename` always replaces. Works on
/// every file system, FAT and exFAT USB sticks too. `replica_core::jobs_file::write` publishes a
/// jobs file with it.
pub fn rename_no_clobber(from: &Path, to: &Path) -> io::Result<()> {
    crate::win32::move_file_no_replace(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn it_renames_but_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "new").unwrap();
        rename_no_clobber(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "new");
        assert!(!a.exists());

        fs::write(&a, "newer").unwrap();
        let err = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(fs::read_to_string(&b).unwrap(), "new", "left as it was");
        assert_eq!(fs::read_to_string(&a).unwrap(), "newer");
    }

    /// A jobs file published with it, as the CLI and the window do: a reader beside the writer
    /// sees no file or the whole export, never an empty or partial one.
    #[test]
    fn a_jobs_file_appears_whole_or_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let seen =
            replica_core::jobs_file::check::whole_or_absent(dir.path(), 300, rename_no_clobber)
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(seen > 0, "the reader saw the file");
    }
}
