//! The exclusion rules mean the same to the pinned restic as to the mirror walker.
//!
//! Builds a fixture tree in a temp folder, asks restic which files a backup would take
//! (`backup --dry-run`, into a temp repository) with `ExcludeRules::restic_args`, and compares
//! that with the files the walker's `is_excluded` keeps.
//!
//! It needs the pinned restic: see `common::restic`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use replica_core::exclude::{COMMON_EXCLUDES, ExcludeRules};

mod common;

/// Files under `root`, as `/`-separated paths relative to it.
fn files(root: &Path, filter: &dyn Fn(&Path) -> bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if filter(&path) {
                out.insert(relative(root, &path));
            }
        }
    }
    out
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

/// restic reports Windows paths as `/C/Users/…` in some versions and `C:\Users\…` in others.
fn local_path(item: &str) -> PathBuf {
    let b = item.as_bytes();
    if cfg!(windows) && b.len() > 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        PathBuf::from(format!("{}:{}", &item[1..2], &item[2..]))
    } else {
        PathBuf::from(item)
    }
}

#[test]
fn restic_and_the_walker_exclude_the_same_files() {
    let Some(restic) = common::restic() else {
        return;
    };

    let temp = tempfile::tempdir().unwrap();
    // Glob characters in the source's own path must stay literal for both engines.
    let source = temp.path().join("src [x] {y}");
    let fixture = [
        // Kept.
        "a.txt",
        "sub/b.txt",
        "docs/sub/c.pdf",
        "x.tmpl",
        "distant/d.txt",
        "output/o.txt",
        "build/other/e.txt",
        "cx.txt",
        "55.log",
        "notes [draft].md",
        // Excluded.
        "node_modules/m.js",
        "app/node_modules/n.js",
        "NODE_MODULES/q.js",
        "game/.godot/g.cfg",
        "dist/f.js",
        "web/out/o.js",
        "t.tmp",
        "deep/er/UP.TMP",
        "docs/a.pdf",
        "build/cache/z.bin",
        "ax.txt",
        "bx.txt",
        "5.log",
    ];
    for file in fixture {
        let path = source.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, file).unwrap();
    }
    let mut patterns: Vec<&str> = COMMON_EXCLUDES.to_vec();
    patterns.extend(["docs/*.pdf", "build/cache", "[ab]x.txt", "[0-9].log"]);
    let rules = ExcludeRules::new([(source.as_path(), patterns.iter().copied())], true).unwrap();

    let walker = files(&source, &|p| !rules.is_excluded(p));
    assert_eq!(walker.len(), 10, "the fixture's kept files: {walker:?}");

    let repo = temp.path().join("repo");
    let run = |args: &[&str], extra: &[String]| {
        let out = Command::new(&restic)
            .args(args)
            .args(extra)
            .env("RESTIC_REPOSITORY", &repo)
            .env("RESTIC_PASSWORD", "fixture-only-password")
            .env_remove("RESTIC_PASSWORD_FILE")
            .env_remove("RESTIC_PASSWORD_COMMAND")
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "restic {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    run(&["init", "--no-cache"], &[]);
    let source_arg = source.to_string_lossy().to_string();
    let mut extra = rules.restic_args();
    extra.push(source_arg);
    let output = run(
        &["backup", "--dry-run", "--json", "--verbose=2", "--no-cache"],
        &extra,
    );

    let mut from_restic = BTreeSet::new();
    for line in output.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg["message_type"] != "verbose_status" {
            continue;
        }
        let Some(item) = msg["item"].as_str() else {
            continue;
        };
        let path = local_path(item);
        if path.is_file() {
            from_restic.insert(relative(&source, &path));
        }
    }
    assert_eq!(
        from_restic, walker,
        "restic took different files from the walker"
    );
}
