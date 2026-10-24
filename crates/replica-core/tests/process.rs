//! Every child process goes through `replica_core::process`, so none opens a console
//! window. A heuristic over the three crates' sources, as `crates/replica/tests/strings.rs` reads
//! them: in non-test code (a file up to its `#[cfg(test)]`), no `Command::new` outside the
//! helper itself. Examples and tests may start what they like; they aren't shipped.

use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `file:line: text` for each `Command::new` in shipped code outside the helper.
fn findings() -> Vec<String> {
    let crates = workspace().join("crates");
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = ["replica", "replica-core", "replica-win"]
        .iter()
        .map(|c| crates.join(c).join("src"))
        .collect();
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "replica-core/src/process.rs" {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for (n, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("#[cfg(test)]") {
                break;
            }
            if code.starts_with("//") {
                continue;
            }
            if code.contains("Command::new(") {
                found.push(format!("{rel}:{}: {code}", n + 1));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_child_process_is_started_through_the_helper() {
    let found = findings();
    assert!(
        found.is_empty(),
        "start these with replica_core::process::command, so they open no console window:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_check_sees_the_files_it_should() {
    // Sanity: the sources are where this looks, and the helper itself does call it.
    let helper = std::fs::read_to_string(workspace().join("crates/replica-core/src/process.rs"))
        .expect("the helper is where the check expects it");
    assert!(helper.contains("Command::new("));
    assert!(workspace().join("crates/replica-win/src/jobs.rs").is_file());
}
