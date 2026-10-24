//! The restic driver against the pinned restic, in temp folders only.
//!
//! It needs the pinned restic: see `common::restic`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use replica_core::cancel::CancelToken;
use replica_core::exclude::ExcludeRules;
use replica_core::job::Retention;
use replica_core::platform::Secret;
use replica_core::restic::{
    BackupRequest, Repository, Restic, ResticError, RestoreRequest, snapshot_path,
};

mod common;

const PASSWORD: &str = "correct-horse-fixture-0f3a9c";

struct Fixture {
    restic: Restic,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Option<Self> {
        let restic = Restic::new(common::restic()?).unwrap();
        assert!(
            restic.is_pinned(),
            "restic {} isn't the pinned version",
            restic.version()
        );
        Some(Fixture {
            restic,
            temp: tempfile::tempdir().unwrap(),
        })
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn repo(&self, name: &str, password: &str) -> Repository {
        Repository::new(self.path(name).to_string_lossy(), Secret::new(password))
            .with_cache_dir(self.path("cache"))
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn backup(
        &self,
        repo: &Repository,
        job: &str,
        sources: &[PathBuf],
    ) -> Result<replica_core::restic::BackupOutcome, ResticError> {
        let excludes =
            ExcludeRules::new(sources.iter().map(|s| (s.as_path(), ["*.tmp"])), true).unwrap();
        let request = BackupRequest {
            job,
            sources,
            excludes: &excludes,
        };
        self.restic
            .backup(repo, &request, &mut |_| {}, &CancelToken::new())
    }

    fn lock_files(&self, repo_name: &str) -> usize {
        fs::read_dir(self.path(repo_name).join("locks"))
            .map(|d| d.count())
            .unwrap_or(0)
    }
}

/// Polls until `cond` holds, failing after a generous bound.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn back_up_change_back_up_restore_forget_prune_check() {
    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    let src = f.path("src");
    f.write("src/a.txt", "a version 1");
    f.write("src/dir/b.txt", "b version 1");
    f.write("src/dir/c.txt", "c version 1");
    f.write("src/skip.tmp", "excluded");

    f.restic.init(&repo).unwrap();
    let mut progress_events = 0;
    let excludes = ExcludeRules::new([(src.as_path(), ["*.tmp"])], true).unwrap();
    let first = f
        .restic
        .backup(
            &repo,
            &BackupRequest {
                job: "hive",
                sources: std::slice::from_ref(&src),
                excludes: &excludes,
            },
            &mut |_| progress_events += 1,
            &CancelToken::new(),
        )
        .unwrap();
    assert!(!first.partial);
    assert_eq!(first.summary.files_new, 3, "the .tmp file is excluded");
    assert!(!first.summary.snapshot_id.is_empty());

    f.write("src/a.txt", "a version 2");
    f.write("src/dir/b.txt", "b version 2");
    fs::remove_file(f.path("src/dir/c.txt")).unwrap();
    let second = f.backup(&repo, "hive", std::slice::from_ref(&src)).unwrap();
    assert_eq!(second.summary.files_changed, 2);

    // Another job's snapshot in the same repository isn't the job's.
    f.write("other/x.txt", "x");
    f.backup(&repo, "other", &[f.path("other")]).unwrap();

    let snaps = f.restic.snapshots(&repo, Some("hive")).unwrap();
    assert_eq!(snaps.len(), 2);
    assert!(snaps.iter().all(|s| s.tags == ["hive"]));
    let older = &snaps[0];
    assert_eq!(older.id, first.summary.snapshot_id);

    let nodes = f
        .restic
        .ls(&repo, &older.id, Some(&snapshot_path(&src)))
        .unwrap();
    let a_path = snapshot_path(&src.join("a.txt"));
    assert!(
        nodes.iter().any(|n| n.path == a_path && n.kind == "file"),
        "{nodes:?}"
    );

    // An older version of a file, then of a folder, each into a new folder.
    let sources = [src.clone()];
    let restore = |item: &str, target: &Path| {
        f.restic.restore(
            &repo,
            &RestoreRequest {
                job: "hive",
                snapshot: &older.id,
                item: Some(item),
                target,
                sources: &sources,
                allow_over_source: false,
            },
            &CancelToken::new(),
        )
    };
    let r1 = f.path("restored-file");
    let summary = restore(&a_path, &r1).unwrap();
    assert_eq!(summary.files_restored, 1);
    assert_eq!(fs::read_to_string(r1.join("a.txt")).unwrap(), "a version 1");
    let r2 = f.path("restored-folder");
    restore(&snapshot_path(&src.join("dir")), &r2).unwrap();
    assert_eq!(
        fs::read_to_string(r2.join("dir/b.txt")).unwrap(),
        "b version 1"
    );
    assert_eq!(
        fs::read_to_string(r2.join("dir/c.txt")).unwrap(),
        "c version 1"
    );
    // Never over the source by default, and never into a folder with files in it.
    assert!(matches!(
        restore(&a_path, &src.join("dir")),
        Err(ResticError::RestoreTargetNotEmpty { .. })
    ));
    assert!(matches!(
        restore(&a_path, &src.join("new")),
        Err(ResticError::RestoreOverSource { .. })
    ));
    assert_eq!(
        fs::read_to_string(src.join("a.txt")).unwrap(),
        "a version 2",
        "the source is untouched"
    );

    // Both snapshots are from today: keeping one daily keeps the newer.
    let forgot = f
        .restic
        .forget(
            &repo,
            "hive",
            &Retention {
                daily: 1,
                weekly: 0,
                monthly: 0,
                keep_forever: false,
            },
        )
        .unwrap();
    assert_eq!(forgot.removed.len(), 1);
    assert_eq!(forgot.removed[0].id, older.id);
    assert_eq!(forgot.kept.len(), 1);
    f.restic.prune(&repo, &CancelToken::new()).unwrap();
    let left = f.restic.snapshots(&repo, Some("hive")).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, second.summary.snapshot_id);
    assert_eq!(
        f.restic.snapshots(&repo, Some("other")).unwrap().len(),
        1,
        "another job's snapshots are kept"
    );

    let check = f
        .restic
        .check(&repo, Some("100%"), &CancelToken::new())
        .unwrap();
    assert_eq!(check.num_errors, 0);
}

#[test]
fn a_wrong_password_and_a_missing_repository() {
    let Some(f) = Fixture::new() else { return };
    let missing = f.repo("nothing-here", PASSWORD);
    assert!(matches!(
        f.restic.snapshots(&missing, None),
        Err(ResticError::RepoNotFound { .. })
    ));

    f.restic.init(&f.repo("repo", PASSWORD)).unwrap();
    let wrong = f.repo("repo", "not-the-password");
    let err = f.restic.snapshots(&wrong, None).unwrap_err();
    assert!(matches!(err, ResticError::WrongPassword { .. }), "{err:?}");
    assert!(
        matches!(
            f.restic.init(&f.repo("repo", PASSWORD)),
            Err(ResticError::Failed { .. })
        ),
        "init twice fails"
    );
}

/// `opens` is the check `replica key set` makes before saving a password someone typed.
#[test]
fn opens_tells_the_right_password_from_a_wrong_one() {
    let Some(f) = Fixture::new() else { return };
    assert!(matches!(
        f.restic.opens(&f.repo("nothing-here", PASSWORD)),
        Err(ResticError::RepoNotFound { .. })
    ));
    f.restic.init(&f.repo("repo", PASSWORD)).unwrap();
    f.restic.opens(&f.repo("repo", PASSWORD)).unwrap();
    let err = f
        .restic
        .opens(&f.repo("repo", "not-the-password"))
        .unwrap_err();
    assert!(matches!(err, ResticError::WrongPassword { .. }), "{err:?}");
}

#[test]
fn a_locked_repository() {
    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();

    // Another restic holds a lock: a backup from stdin, kept open.
    let mut holder = Command::new(f.restic.path())
        .args([
            "backup",
            "--stdin",
            "--stdin-filename",
            "held.txt",
            "--no-cache",
        ])
        .env("RESTIC_REPOSITORY", &repo.location)
        .env("RESTIC_PASSWORD", PASSWORD)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the holder's lock", || f.lock_files("repo") > 0);

    let err = f.restic.prune(&repo, &CancelToken::new()).unwrap_err();
    let ResticError::Locked { detail, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(detail.contains("locked"), "{detail}");
    // unlock only removes stale locks: the live one stays.
    f.restic.unlock_stale(&repo).unwrap();
    assert!(matches!(
        f.restic.prune(&repo, &CancelToken::new()),
        Err(ResticError::Locked { .. })
    ));

    let mut stdin = holder.stdin.take().unwrap();
    stdin.write_all(b"held").unwrap();
    drop(stdin);
    assert!(holder.wait().unwrap().success());
    f.restic.prune(&repo, &CancelToken::new()).unwrap();
}

/// Writes `megabytes` of data that doesn't compress, and that differs for each `seed`, so it
/// doesn't deduplicate either.
fn write_noise(path: &Path, megabytes: usize, seed: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::io::BufWriter::new(fs::File::create(path).unwrap());
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut block = vec![0u8; 1 << 20];
    for _ in 0..megabytes {
        for chunk in block.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
        }
        file.write_all(&block).unwrap();
    }
}

#[test]
fn a_cancelled_backup_leaves_no_lock_and_a_sound_repository() {
    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();
    for i in 0..4 {
        write_noise(&f.path(&format!("big/part{i}.bin")), 128, i + 1);
    }
    let sources = [f.path("big")];
    let excludes = ExcludeRules::new([(sources[0].as_path(), Vec::<&str>::new())], true).unwrap();
    let cancel = CancelToken::new();
    let mut saw_progress = false;
    let mut locked_when_cancelled = false;
    let result = f.restic.backup(
        &repo,
        &BackupRequest {
            job: "big",
            sources: &sources,
            excludes: &excludes,
        },
        &mut |p| {
            if p.bytes_done > 0 {
                saw_progress = true;
                locked_when_cancelled = f.lock_files("repo") > 0;
                cancel.cancel();
            }
        },
        &cancel,
    );
    assert_eq!(result.unwrap_err(), ResticError::Cancelled);
    assert!(saw_progress);
    assert!(
        locked_when_cancelled,
        "restic held a lock when it was cancelled"
    );
    assert_eq!(
        f.lock_files("repo"),
        0,
        "the killed restic's lock is removed"
    );

    // The next exclusive command runs at once, and the repository checks clean.
    f.restic.prune(&repo, &CancelToken::new()).unwrap();
    assert_eq!(
        f.restic
            .check(&repo, None, &CancelToken::new())
            .unwrap()
            .num_errors,
        0
    );
    assert!(f.restic.snapshots(&repo, Some("big")).unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn unreadable_files_make_a_partial_snapshot() {
    use std::os::windows::fs::OpenOptionsExt;

    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();
    f.write("src/ok.txt", "fine");
    f.write("src/busy.txt", "in use");
    // No sharing, so restic can't open it while this handle is open; and locked whole, so even
    // a restic that gets a handle all the same (as an administrator on CI did) can't read it.
    let busy = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(f.path("src/busy.txt"))
        .unwrap();
    busy.lock().unwrap();
    let outcome = f.backup(&repo, "partial", &[f.path("src")]).unwrap();
    drop(busy);
    assert!(outcome.partial);
    assert_eq!(outcome.unreadable.len(), 1, "{:?}", outcome.unreadable);
    assert!(
        outcome.unreadable[0].item.ends_with("busy.txt"),
        "{:?}",
        outcome.unreadable
    );
    assert!(
        !outcome.summary.snapshot_id.is_empty(),
        "the snapshot is still made"
    );
    assert_eq!(f.restic.snapshots(&repo, Some("partial")).unwrap().len(), 1);
}

/// A `tracing` writer into a shared buffer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Everything this test binary logs, through one global subscriber. A subscriber set for one
/// thread can miss events while other tests run (tracing caches whether a call site is wanted),
/// so the log test can't rely on one.
fn captured_log() -> &'static Captured {
    static LOG: OnceLock<Captured> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Captured::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
        log
    })
}

#[test]
fn the_password_is_never_on_a_command_line_or_in_the_log() {
    let log = captured_log();
    let Some(f) = Fixture::new() else { return };
    let password = "log-test-password-7c41e9";
    let repo = f.repo("repo", password);
    f.restic.init(&repo).unwrap();
    f.write("src/a.txt", "a");
    f.backup(&repo, "log", &[f.path("src")]).unwrap();
    f.restic.snapshots(&repo, Some("log")).unwrap();
    f.restic
        .forget(&repo, "log", &Retention::default())
        .unwrap();
    f.restic
        .check(&repo, Some("10%"), &CancelToken::new())
        .unwrap();
    let wrong = f.repo("repo", &format!("{password}-wrong"));
    let _ = f.restic.snapshots(&wrong, None);

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    // This repository's own command lines, among whatever the other tests logged.
    let ours: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(&repo.location))
        .collect();
    for command in [
        "restic init",
        "restic backup",
        "restic snapshots",
        "restic forget",
        "restic check",
    ] {
        assert!(
            ours.iter().any(|l| l.contains(command)),
            "the log has every command line: {command} missing from\n{text}"
        );
    }
    for secret in [password, PASSWORD] {
        assert!(!text.contains(secret), "a password is in the log:\n{text}");
    }
}

#[test]
fn a_restore_brings_back_exactly_the_named_file_or_folder() {
    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();
    // Names with glob characters, beside names a glob would also match.
    for (rel, text) in [
        ("src/notes [draft].txt", "draft"),
        ("src/notes d.txt", "d"),
        ("src/x{1}.txt", "braces"),
        ("src/x1.txt", "plain"),
        ("src/dir [old]/a.txt", "old a"),
        ("src/dir o/b.txt", "other b"),
    ] {
        f.write(rel, text);
    }
    let src = f.path("src");
    let snapshot = f
        .backup(&repo, "names", std::slice::from_ref(&src))
        .unwrap()
        .summary
        .snapshot_id;
    let sources = [src.clone()];
    let restore = |item: &str, target: &Path| {
        f.restic.restore(
            &repo,
            &RestoreRequest {
                job: "names",
                snapshot: &snapshot,
                item: Some(item),
                target,
                sources: &sources,
                allow_over_source: false,
            },
            &CancelToken::new(),
        )
    };
    let listing = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    for (i, (name, check_file, text)) in [
        ("notes [draft].txt", "notes [draft].txt", "draft"),
        ("x{1}.txt", "x{1}.txt", "braces"),
        ("dir [old]", "dir [old]/a.txt", "old a"),
    ]
    .into_iter()
    .enumerate()
    {
        let target = f.path(&format!("restored-{i}"));
        restore(&snapshot_path(&src.join(name)), &target).unwrap();
        assert_eq!(listing(&target), [name], "only {name} is restored");
        assert_eq!(fs::read_to_string(target.join(check_file)).unwrap(), text);
    }

    // A name that isn't in the snapshot restores nothing, and says so.
    let err = restore(
        &snapshot_path(&src.join("missing.txt")),
        &f.path("restored-missing"),
    )
    .unwrap_err();
    assert!(matches!(err, ResticError::RestoreNothing { .. }), "{err:?}");

    // `..` can't take a restore into the source.
    let alias = f.path("alias").join("..").join("src").join("restored");
    let err = restore(&snapshot_path(&src.join("x1.txt")), &alias).unwrap_err();
    assert!(
        matches!(err, ResticError::RestoreOverSource { .. }),
        "{err:?}"
    );
    assert!(!src.join("restored").exists());
}

/// A snapshot keeps its source as it was typed. Backed up by its short (8.3) name, as Temp is on
/// GitHub's runners, a file asked for by its long name, in another case or through a junction is
/// still found and restored.
#[cfg(windows)]
#[test]
fn a_restore_finds_an_item_by_another_of_its_names() {
    use std::os::windows::process::CommandExt;

    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();
    f.write("Long Source Folder/a.txt", "alpha");
    f.write("Long Source Folder/Sub Folder/b.txt", "beta");
    let long = f.path("Long Source Folder");
    // cmd's `%~s` is the short name; the long name again where the volume makes none.
    let out = Command::new("cmd")
        .raw_arg(format!(
            "/C for %I in (\"{}\") do @echo %~sI",
            long.display()
        ))
        .output()
        .unwrap();
    assert!(out.status.success());
    let short = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    assert!(short.is_dir(), "{}", short.display());
    if short == long {
        eprintln!("this volume makes no short names: only case and a junction are tried");
    }
    let snapshot = f
        .backup(&repo, "names", std::slice::from_ref(&short))
        .unwrap()
        .summary
        .snapshot_id;
    let link = f.path("link");
    let made = Command::new("cmd")
        .raw_arg(format!(
            "/C mklink /J \"{}\" \"{}\"",
            link.display(),
            long.display()
        ))
        .output()
        .unwrap();
    assert!(made.status.success(), "couldn't make a junction");

    let sources = [short.clone()];
    let restore = |item: &Path, target: &Path| {
        f.restic.restore(
            &repo,
            &RestoreRequest {
                job: "names",
                snapshot: &snapshot,
                item: Some(&snapshot_path(item)),
                target,
                sources: &sources,
                allow_over_source: false,
            },
            &CancelToken::new(),
        )
    };
    let upper = PathBuf::from(long.to_string_lossy().to_uppercase());
    for (i, (item, check, text)) in [
        (long.join("a.txt"), "a.txt", "alpha"),
        (upper.join("SUB FOLDER"), "Sub Folder/b.txt", "beta"),
        (upper.join("sub folder").join("B.TXT"), "b.txt", "beta"),
        (link.join("Sub Folder"), "Sub Folder/b.txt", "beta"),
        (short.join("a.txt"), "a.txt", "alpha"),
    ]
    .into_iter()
    .enumerate()
    {
        let target = f.path(&format!("restored-{i}"));
        restore(&item, &target).unwrap_or_else(|e| panic!("{}: {e}", item.display()));
        assert_eq!(
            fs::read_to_string(target.join(check)).unwrap(),
            text,
            "{}",
            item.display()
        );
    }

    // Still nothing for a name that isn't there by any name.
    let err = restore(&long.join("missing.txt"), &f.path("restored-missing")).unwrap_err();
    assert!(matches!(err, ResticError::RestoreNothing { .. }), "{err:?}");
}

#[test]
fn a_restore_uses_only_its_own_jobs_snapshots_in_a_shared_repository() {
    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();
    let src = f.path("src");
    f.write("src/a.txt", "alpha's version");
    let alpha = f
        .backup(&repo, "alpha", std::slice::from_ref(&src))
        .unwrap()
        .summary
        .snapshot_id;
    f.write("src/a.txt", "beta's newer version");
    let beta = f
        .backup(&repo, "beta", std::slice::from_ref(&src))
        .unwrap()
        .summary
        .snapshot_id;
    let sources = [src.clone()];
    let item = snapshot_path(&src.join("a.txt"));
    let restore = |job: &str, snapshot: &str, target: &Path| {
        f.restic.restore(
            &repo,
            &RestoreRequest {
                job,
                snapshot,
                item: Some(&item),
                target,
                sources: &sources,
                allow_over_source: false,
            },
            &CancelToken::new(),
        )
    };

    // "latest" is alpha's latest, though beta's is newer in the same repository.
    restore("alpha", "latest", &f.path("out-latest")).unwrap();
    assert_eq!(
        fs::read_to_string(f.path("out-latest/a.txt")).unwrap(),
        "alpha's version"
    );
    // Alpha's own ID, whole or its start, works; beta's doesn't, whole or short.
    restore("alpha", &alpha[..8], &f.path("out-short")).unwrap();
    assert_eq!(
        fs::read_to_string(f.path("out-short/a.txt")).unwrap(),
        "alpha's version"
    );
    for wrong in [beta.as_str(), &beta[..8]] {
        let err = restore("alpha", wrong, &f.path("out-wrong")).unwrap_err();
        assert!(
            matches!(err, ResticError::SnapshotNotFound { .. }),
            "{err:?}"
        );
    }
    assert!(!f.path("out-wrong").exists());
    // A job with no snapshot has no "latest".
    assert!(matches!(
        restore("gamma", "latest", &f.path("out-none")),
        Err(ResticError::SnapshotNotFound { .. })
    ));
}

#[test]
fn an_item_missing_from_the_snapshot_is_not_restored_even_over_an_existing_file() {
    let Some(f) = Fixture::new() else { return };
    let repo = f.repo("repo", PASSWORD);
    f.restic.init(&repo).unwrap();
    let src = f.path("src");
    f.write("src/a.txt", "a");
    f.backup(&repo, "job", std::slice::from_ref(&src)).unwrap();
    // missing.txt is made after the backup: in the source folder, not in the snapshot.
    f.write("src/missing.txt", "the original, not in any snapshot");
    let sources = [src.clone()];
    let item = snapshot_path(&src.join("missing.txt"));
    let err = f
        .restic
        .restore(
            &repo,
            &RestoreRequest {
                job: "job",
                snapshot: "latest",
                item: Some(&item),
                target: &src,
                sources: &sources,
                allow_over_source: true,
            },
            &CancelToken::new(),
        )
        .unwrap_err();
    assert!(matches!(err, ResticError::RestoreNothing { .. }), "{err:?}");
    assert_eq!(
        fs::read_to_string(src.join("missing.txt")).unwrap(),
        "the original, not in any snapshot"
    );
}
