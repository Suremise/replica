//! The mirror engine, in temp folders only.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use replica_core::cancel::CancelToken;
use replica_core::job::Source;
use replica_core::mirror::{Compare, MirrorError, MirrorOutcome, MirrorRequest, mirror};

struct Fixture {
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            temp: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }

    fn run(&self, sources: &[Source], limit: u32) -> Result<MirrorOutcome, MirrorError> {
        self.run_with(sources, limit, Compare::SizeAndTime)
    }

    fn run_with(
        &self,
        sources: &[Source],
        limit: u32,
        compare: Compare,
    ) -> Result<MirrorOutcome, MirrorError> {
        let target = self.path("target");
        mirror(
            &MirrorRequest {
                sources,
                target: &target,
                compare,
                delete_limit_percent: limit,
                case_insensitive: true,
                exclude_paths: &[],
            },
            &mut |_| {},
            &CancelToken::new(),
        )
    }
}

fn source(path: PathBuf, exclude: &[&str]) -> Source {
    Source {
        path,
        exclude: exclude.iter().map(|s| s.to_string()).collect(),
    }
}

/// Sets a file's modified time, so a change of content with the same size and time can be made.
fn set_mtime(path: &Path, time: SystemTime) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn first_copy_change_delete_and_exclusions() {
    let f = Fixture::new();
    f.write("src/a.txt", "a1");
    f.write("src/sub/b.txt", "b1");
    f.write("src/sub/deep/c.txt", "c1");
    f.write("src/node_modules/x.js", "excluded");
    f.write("src/t.tmp", "excluded");
    let sources = [source(f.path("src"), &["node_modules", "*.tmp"])];

    let first = f.run(&sources, 30).unwrap();
    assert_eq!((first.copied, first.deleted, first.unchanged), (3, 0, 0));
    assert!(!first.partial());
    assert_eq!(first.bytes_copied, 6);
    assert_eq!(f.read("target/src/a.txt"), "a1");
    assert_eq!(f.read("target/src/sub/deep/c.txt"), "c1");
    assert!(!f.exists("target/src/node_modules") && !f.exists("target/src/t.tmp"));
    let src_time = fs::metadata(f.path("src/a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let dst_time = fs::metadata(f.path("target/src/a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(src_time, dst_time, "the modified time is copied");

    let again = f.run(&sources, 30).unwrap();
    assert_eq!(
        (again.copied, again.deleted, again.unchanged),
        (0, 0, 3),
        "a no-change run copies nothing"
    );

    f.write("src/a.txt", "a2 longer");
    let changed = f.run(&sources, 30).unwrap();
    assert_eq!((changed.copied, changed.unchanged), (1, 2));
    assert_eq!(f.read("target/src/a.txt"), "a2 longer");

    // What the exclusions match in the target, and what's beside the mirror, are left alone.
    f.write("target/src/node_modules/kept.js", "mine");
    f.write("target/beside.txt", "not the mirror's");
    fs::remove_dir_all(f.path("src/sub/deep")).unwrap();
    let deleted = f.run(&sources, 100).unwrap();
    assert_eq!(deleted.deleted, 2, "the file, then its emptied folder");
    assert!(!f.exists("target/src/sub/deep"));
    assert!(f.exists("target/src/sub/b.txt"));
    assert_eq!(f.read("target/src/node_modules/kept.js"), "mine");
    assert_eq!(f.read("target/beside.txt"), "not the mirror's");
    // No temp files are left behind.
    let names: Vec<String> = fs::read_dir(f.path("target/src"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains("replica-tmp")),
        "{names:?}"
    );
}

#[test]
fn type_conflicts_are_left_alone_and_reported() {
    let f = Fixture::new();
    f.write("src/x/inner.txt", "folder");
    f.write("src/y", "file");
    f.write("src/z.txt", "z1");
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();
    fs::remove_dir_all(f.path("src/x")).unwrap();
    fs::remove_file(f.path("src/y")).unwrap();
    f.write("src/x", "now a file");
    f.write("src/y/inner.txt", "now a folder");
    f.write("src/z.txt", "z2 changed");
    let out = f.run(&sources, 100).unwrap();
    // Both swaps are reported, and nothing is done to them.
    assert!(out.partial());
    let conflicts: Vec<String> = out
        .failed
        .iter()
        .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(conflicts, ["x", "y"], "{:?}", out.failed);
    assert!(
        out.failed
            .iter()
            .all(|c| c.message.contains("type conflict"))
    );
    assert_eq!(
        f.read("target/src/x/inner.txt"),
        "folder",
        "the old folder is untouched"
    );
    assert_eq!(f.read("target/src/y"), "file", "the old file is untouched");
    // The rest of the tree still mirrors.
    assert_eq!(f.read("target/src/z.txt"), "z2 changed");
    assert_eq!(out.copied, 1);
    assert_eq!(out.deleted, 0);
}

#[test]
fn a_type_conflict_at_the_top_level_is_left_alone_and_the_rest_mirrors() {
    let f = Fixture::new();
    // A folder source whose place in the target is a file, at a 100% allowance.
    f.write("src/a.txt", "a");
    f.write("target/src", "an old file where the folder goes");
    // A file source whose place in the target is a folder.
    f.write("notes.md", "notes");
    f.write(
        "target/notes.md/old.txt",
        "an old folder where the file goes",
    );
    // And a source with no conflict.
    f.write("other/b.txt", "b");
    let sources = [
        source(f.path("src"), &[]),
        source(f.path("notes.md"), &[]),
        source(f.path("other"), &[]),
    ];
    let out = f.run(&sources, 100).unwrap();
    assert!(out.partial());
    assert_eq!(out.failed.len(), 2, "{:?}", out.failed);
    assert!(
        out.failed
            .iter()
            .all(|c| c.message.contains("type conflict"))
    );
    assert_eq!(f.read("target/src"), "an old file where the folder goes");
    assert_eq!(
        f.read("target/notes.md/old.txt"),
        "an old folder where the file goes"
    );
    assert_eq!(f.read("target/other/b.txt"), "b");
    assert_eq!((out.copied, out.deleted), (1, 0));
}

#[test]
fn read_only_files_are_copied_and_replaced() {
    let f = Fixture::new();
    f.write("src/ro.txt", "v1");
    let set_ro = |p: &Path, ro: bool| {
        let mut perms = fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(ro);
        fs::set_permissions(p, perms).unwrap();
    };
    set_ro(&f.path("src/ro.txt"), true);
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();
    assert!(
        fs::metadata(f.path("target/src/ro.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );

    set_ro(&f.path("src/ro.txt"), false);
    f.write("src/ro.txt", "version 2");
    set_ro(&f.path("src/ro.txt"), true);
    let out = f.run(&sources, 100).unwrap();
    assert_eq!(out.copied, 1, "{:?}", out.failed);
    assert_eq!(f.read("target/src/ro.txt"), "version 2");

    // A read-only extra is deleted too.
    set_ro(&f.path("src/ro.txt"), false);
    fs::remove_file(f.path("src/ro.txt")).unwrap();
    f.write("src/other.txt", "keeps the source non-empty");
    let out = f.run(&sources, 100).unwrap();
    assert!(!out.partial(), "{:?}", out.failed);
    assert!(!f.exists("target/src/ro.txt"));
    set_ro(&f.path("target/src/other.txt"), false);
}

#[test]
fn content_comparison_catches_a_change_size_and_time_miss() {
    let f = Fixture::new();
    f.write("src/same.txt", "aaaa");
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();
    let time = fs::metadata(f.path("src/same.txt"))
        .unwrap()
        .modified()
        .unwrap();
    f.write("src/same.txt", "bbbb");
    set_mtime(&f.path("src/same.txt"), time);
    assert_eq!(
        f.run(&sources, 100).unwrap().copied,
        0,
        "size and time alone miss it"
    );
    assert_eq!(
        f.run_with(&sources, 100, Compare::Content).unwrap().copied,
        1
    );
    assert_eq!(f.read("target/src/same.txt"), "bbbb");
}

#[test]
fn a_file_source_and_several_sources() {
    let f = Fixture::new();
    f.write("one/a.txt", "a");
    f.write("two/b.txt", "b");
    f.write("notes.md", "n");
    let sources = [
        source(f.path("one"), &[]),
        source(f.path("two"), &[]),
        source(f.path("notes.md"), &[]),
    ];
    let out = f.run(&sources, 100).unwrap();
    assert_eq!(out.copied, 3);
    assert_eq!(f.read("target/one/a.txt"), "a");
    assert_eq!(f.read("target/two/b.txt"), "b");
    assert_eq!(f.read("target/notes.md"), "n");

    // Two sources with the same name can't share a folder.
    f.write("deeper/one/c.txt", "c");
    let clash = [
        source(f.path("one"), &[]),
        source(f.path("deeper/one"), &[]),
    ];
    assert!(matches!(
        f.run(&clash, 100),
        Err(MirrorError::DuplicateName { .. })
    ));
}

#[cfg(windows)]
#[test]
fn a_file_in_use_is_reported_and_the_rest_copied() {
    use std::os::windows::fs::OpenOptionsExt;

    let f = Fixture::new();
    f.write("src/ok.txt", "fine");
    f.write("src/busy.txt", "in use");
    let busy = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(f.path("src/busy.txt"))
        .unwrap();
    let out = f.run(&[source(f.path("src"), &[])], 100).unwrap();
    drop(busy);
    assert!(out.partial());
    assert_eq!(out.copied, 1);
    assert_eq!(out.failed.len(), 1);
    assert!(out.failed[0].path.ends_with("busy.txt"), "{:?}", out.failed);
    assert_eq!(f.read("target/src/ok.txt"), "fine");
    assert!(!f.exists("target/src/busy.txt"));
    // Once it's free, the next run copies it.
    assert_eq!(f.run(&[source(f.path("src"), &[])], 100).unwrap().copied, 1);
}

#[test]
fn an_empty_or_missing_source_is_refused_and_the_target_untouched() {
    let f = Fixture::new();
    f.write("src/a.txt", "a");
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();

    // Emptied, as a disconnected drive would look.
    fs::remove_file(f.path("src/a.txt")).unwrap();
    fs::create_dir_all(f.path("src/empty-folder")).unwrap();
    assert!(matches!(
        f.run(&sources, 100),
        Err(MirrorError::SourceEmpty { .. })
    ));
    // Only excluded files count as empty too.
    f.write("src/x.tmp", "excluded");
    let excluded = [source(f.path("src"), &["*.tmp"])];
    assert!(matches!(
        f.run(&excluded, 100),
        Err(MirrorError::SourceEmpty { .. })
    ));
    fs::remove_dir_all(f.path("src")).unwrap();
    assert!(matches!(
        f.run(&sources, 100),
        Err(MirrorError::SourceMissing { .. })
    ));
    assert_eq!(f.read("target/src/a.txt"), "a", "nothing was deleted");
}

#[test]
fn a_target_that_is_or_holds_a_source_is_refused() {
    let f = Fixture::new();
    f.write("src/a.txt", "a");
    let inside = f.path("src/mirror");
    let request = |target: &Path| {
        mirror(
            &MirrorRequest {
                sources: &[source(f.path("src"), &[])],
                target,
                compare: Compare::SizeAndTime,
                delete_limit_percent: 100,
                case_insensitive: true,
                exclude_paths: &[],
            },
            &mut |_| {},
            &CancelToken::new(),
        )
    };
    assert!(matches!(
        request(&inside),
        Err(MirrorError::TargetOverlapsSource { .. })
    ));
    assert!(matches!(
        request(&f.path("src")),
        Err(MirrorError::TargetOverlapsSource { .. })
    ));
    assert!(
        matches!(
            request(f.temp.path()),
            Err(MirrorError::TargetOverlapsSource { .. })
        ),
        "around it"
    );
    assert!(matches!(
        request(&f.path("elsewhere").join("..").join("src").join("m")),
        Err(MirrorError::TargetOverlapsSource { .. })
    ));
    assert!(!inside.exists());
}

#[test]
fn the_deletion_guard_stops_a_run_that_would_delete_too_much() {
    let f = Fixture::new();
    for i in 0..40 {
        f.write(&format!("src/f{i:02}.txt"), "x");
    }
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 30).unwrap();
    for i in 0..20 {
        fs::remove_file(f.path(&format!("src/f{i:02}.txt"))).unwrap();
    }
    f.write("src/new.txt", "added meanwhile");
    let err = f.run(&sources, 30).unwrap_err();
    assert_eq!(
        err,
        MirrorError::TooManyDeletions {
            would_delete: 20,
            of: 40,
            limit_percent: 30
        }
    );
    assert!(f.exists("target/src/f00.txt"), "nothing deleted");
    assert!(!f.exists("target/src/new.txt"), "nothing copied either");

    // Within the limit, or with the job allowing it, it runs.
    assert_eq!(f.run(&sources, 50).unwrap().deleted, 20);
    assert!(!f.exists("target/src/f00.txt"));
}

#[test]
fn the_deletion_guard_holds_for_small_mirrors_and_a_zero_limit() {
    let f = Fixture::new();
    f.write("src/old1.txt", "1");
    f.write("src/old2.txt", "2");
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();
    fs::remove_file(f.path("src/old1.txt")).unwrap();
    fs::remove_file(f.path("src/old2.txt")).unwrap();
    f.write("src/new.txt", "new");
    // Both of the mirror's two files would go: 100%, over 30%.
    assert_eq!(
        f.run(&sources, 30).unwrap_err(),
        MirrorError::TooManyDeletions {
            would_delete: 2,
            of: 2,
            limit_percent: 30
        }
    );
    assert!(f.exists("target/src/old1.txt") && f.exists("target/src/old2.txt"));
    // A limit of 0 refuses any deletion; with no deletion it runs.
    assert!(matches!(
        f.run(&sources, 0),
        Err(MirrorError::TooManyDeletions { .. })
    ));
    // The job allowing it lets it through.
    assert_eq!(f.run(&sources, 100).unwrap().deleted, 2);
    f.write("src/another.txt", "added, nothing deleted");
    assert_eq!(f.run(&sources, 0).unwrap().copied, 1);
}

#[test]
fn long_paths() {
    let f = Fixture::new();
    let mut rel = String::from("src");
    while rel.len() < 300 {
        rel.push_str("/a-rather-long-folder-name-to-pass-max-path");
    }
    let file = format!("{rel}/deep.txt");
    f.write(&file, "deep");
    f.write("src/top.txt", "top");
    let out = f.run(&[source(f.path("src"), &[])], 100).unwrap();
    assert!(!out.partial(), "{:?}", out.failed);
    assert_eq!(out.copied, 2);
    let mirrored = f.path(&format!("target/{file}"));
    assert!(mirrored.to_string_lossy().len() > 300);
    assert_eq!(fs::read_to_string(&mirrored).unwrap(), "deep");
    // And deleting a long path.
    fs::remove_dir_all(f.path("src/a-rather-long-folder-name-to-pass-max-path")).unwrap();
    let out = f.run(&[source(f.path("src"), &[])], 100).unwrap();
    assert!(!out.partial(), "{:?}", out.failed);
    assert_eq!(out.deleted, 8, "the file and its seven folders");
    assert!(!mirrored.exists());
}

#[test]
fn a_cancelled_copy_leaves_no_half_file() {
    let f = Fixture::new();
    let big = f.path("src/big.bin");
    fs::create_dir_all(big.parent().unwrap()).unwrap();
    fs::write(&big, vec![7u8; 16 << 20]).unwrap();
    let cancel = CancelToken::new();
    let target = f.path("target");
    let sources = [source(f.path("src"), &[])];
    let result = mirror(
        &MirrorRequest {
            sources: &sources,
            target: &target,
            compare: Compare::SizeAndTime,
            delete_limit_percent: 30,
            case_insensitive: true,
            exclude_paths: &[],
        },
        &mut |p| {
            if p.bytes_done > 0 {
                cancel.cancel();
            }
        },
        &cancel,
    );
    assert_eq!(result.unwrap_err(), MirrorError::Cancelled);
    assert!(
        !f.exists("target/src"),
        "no half file, staging file or new empty folder is left"
    );
}

#[test]
fn a_run_cancelled_while_making_folders_leaves_nothing_behind() {
    let f = Fixture::new();
    f.write("src/keep.txt", "already mirrored");
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();
    for i in 0..50 {
        f.write(&format!("src/d{i:02}/file.txt"), "new");
    }
    let cancel = CancelToken::new();
    let target = f.path("target");
    let mut folders_seen = 0;
    let result = mirror(
        &MirrorRequest {
            sources: &sources,
            target: &target,
            compare: Compare::SizeAndTime,
            delete_limit_percent: 30,
            case_insensitive: true,
            exclude_paths: &[],
        },
        &mut |p| {
            // Progress names each folder as it's made: cancel after the first few.
            if p.files_total == 0 && p.current.is_some() {
                folders_seen += 1;
                if folders_seen == 3 {
                    cancel.cancel();
                }
            }
        },
        &cancel,
    );
    assert_eq!(result.unwrap_err(), MirrorError::Cancelled);
    assert_eq!(folders_seen, 3, "cancelled while making folders");
    let left: Vec<String> = fs::read_dir(f.path("target/src"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        ["keep.txt"],
        "the new folders are removed; the mirror is as it was"
    );
}

#[test]
fn progress_counts_files_and_bytes() {
    let f = Fixture::new();
    f.write("src/a.txt", "12345");
    f.write("src/b.txt", "678");
    let target = f.path("target");
    let mut last = None;
    mirror(
        &MirrorRequest {
            sources: &[source(f.path("src"), &[])],
            target: &target,
            compare: Compare::SizeAndTime,
            delete_limit_percent: 30,
            case_insensitive: true,
            exclude_paths: &[],
        },
        &mut |p| last = Some(p.clone()),
        &CancelToken::new(),
    )
    .unwrap();
    let last = last.unwrap();
    assert_eq!(
        (
            last.files_done,
            last.files_total,
            last.bytes_done,
            last.bytes_total
        ),
        (2, 2, 8, 8)
    );
}

#[test]
fn leftover_temporary_files_are_cleaned_up() {
    let f = Fixture::new();
    f.write("src/a.txt", "a");
    let sources = [source(f.path("src"), &[])];
    f.run(&sources, 100).unwrap();
    f.write("target/src/.a.txt.replica-tmp", "from a run that crashed");
    assert_eq!(f.run(&sources, 100).unwrap().deleted, 1);
    assert!(!f.exists("target/src/.a.txt.replica-tmp"));
}

/// A junction (Windows) or symbolic link (elsewhere) at `link`, pointing to the folder `to`.
fn link_dir(link: &Path, to: &Path) {
    // mklink wants `\` separators.
    #[cfg(windows)]
    let backslashes = |p: &Path| p.to_string_lossy().replace('/', "\\");
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(backslashes(link))
        .arg(backslashes(to))
        .output()
        .is_ok_and(|o| o.status.success());
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(to, link).is_ok();
    assert!(made, "couldn't make a junction at {}", link.display());
}

#[test]
fn a_source_folder_in_the_target_that_is_a_junction_is_refused() {
    let f = Fixture::new();
    f.write("src/a.txt", "new");
    f.write("victim/a.txt", "precious");
    fs::create_dir_all(f.path("target")).unwrap();
    link_dir(&f.path("target/src"), &f.path("victim"));
    let err = f.run(&[source(f.path("src"), &[])], 100).unwrap_err();
    assert!(
        matches!(err, MirrorError::DestinationIsLink { .. }),
        "{err:?}"
    );
    assert_eq!(
        f.read("victim/a.txt"),
        "precious",
        "nothing written through the junction"
    );
}

#[test]
fn a_junction_inside_the_target_is_replaced_not_followed() {
    let f = Fixture::new();
    f.write("src/d/a.txt", "new");
    f.write("src/e", "a file where a junction is");
    f.write("victim/a.txt", "precious");
    f.write("victim/other.txt", "also precious");
    fs::create_dir_all(f.path("target/src")).unwrap();
    link_dir(&f.path("target/src/d"), &f.path("victim"));
    link_dir(&f.path("target/src/e"), &f.path("victim"));
    link_dir(&f.path("target/src/extra"), &f.path("victim"));
    let out = f.run(&[source(f.path("src"), &[])], 100).unwrap();
    assert!(!out.partial(), "{:?}", out.failed);
    assert_eq!(f.read("target/src/d/a.txt"), "new");
    assert!(
        !fs::symlink_metadata(f.path("target/src/d"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(f.read("target/src/e"), "a file where a junction is");
    assert!(
        !f.exists("target/src/extra"),
        "an extra junction is removed"
    );
    assert_eq!(f.read("victim/a.txt"), "precious");
    assert_eq!(f.read("victim/other.txt"), "also precious");
}

#[test]
fn staging_never_reuses_an_existing_file_or_hard_link() {
    let f = Fixture::new();
    f.write("src/a.txt", "new content");
    // Someone's file, hard-linked under the old fixed staging name.
    f.write("victim/valuable.txt", "valuable");
    fs::create_dir_all(f.path("target/src")).unwrap();
    fs::hard_link(
        f.path("victim/valuable.txt"),
        f.path("target/src/.a.txt.replica-tmp"),
    )
    .unwrap();
    // And a real source file with a staging-like name, copied alongside.
    f.write("src/.a.txt.replica-tmp", "a real file");
    let out = f.run(&[source(f.path("src"), &[])], 100).unwrap();
    assert!(!out.partial(), "{:?}", out.failed);
    assert_eq!(f.read("victim/valuable.txt"), "valuable");
    assert_eq!(f.read("target/src/a.txt"), "new content");
    assert_eq!(f.read("target/src/.a.txt.replica-tmp"), "a real file");
    let names: Vec<String> = fs::read_dir(f.path("target/src"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "no staging files left: {names:?}");
}

#[test]
fn many_parallel_copies_of_one_name_in_many_folders() {
    let f = Fixture::new();
    for i in 0..64 {
        f.write(&format!("src/d{i}/same.txt"), &format!("{i}"));
    }
    let out = f.run(&[source(f.path("src"), &[])], 100).unwrap();
    assert_eq!(out.copied, 64, "{:?}", out.failed);
    for i in 0..64 {
        assert_eq!(f.read(&format!("target/src/d{i}/same.txt")), format!("{i}"));
    }
}

#[test]
fn excluded_files_below_an_obsolete_folder_are_kept() {
    let f = Fixture::new();
    f.write("src/keep.txt", "k");
    let sources = [source(f.path("src"), &["*.tmp"])];
    f.run(&sources, 100).unwrap();
    f.write(
        "target/src/obsolete/preserve.tmp",
        "excluded, so the mirror's to leave",
    );
    f.write("target/src/obsolete/deeper/gone.txt", "an extra");
    let out = f.run(&sources, 100).unwrap();
    assert!(!out.partial(), "{:?}", out.failed);
    assert_eq!(
        f.read("target/src/obsolete/preserve.tmp"),
        "excluded, so the mirror's to leave"
    );
    assert!(
        !f.exists("target/src/obsolete/deeper"),
        "the extras beside it go"
    );
}

#[test]
fn a_folder_in_the_way_of_a_file_is_kept_if_it_holds_excluded_files() {
    let f = Fixture::new();
    f.write("src/z", "a file");
    let sources = [source(f.path("src"), &["*.tmp"])];
    f.write("target/src/z/protected.tmp", "excluded");
    let out = f.run(&sources, 100).unwrap();
    assert!(out.partial());
    assert_eq!(f.read("target/src/z/protected.tmp"), "excluded");
}
