//! No user-visible text is written into the code: it all comes from Fluent (`locales/en/`).
//!
//! A heuristic over the three crates' sources (their non-test code): in a call that shows text to
//! the user (printing, the window's labels and buttons, notifications, errors meant for display),
//! a string literal with a word in it, once its `{placeholders}` are taken out, is a finding.
//! Logs (tracing, run logs) are diagnostics in English and aren't checked. A rare literal that
//! isn't prose can be allowed below, with its reason.

use std::path::{Path, PathBuf};

/// Calls that put their text in front of the user.
const SHOWS_TEXT: &[&str] = &[
    "println!(",
    "eprintln!(",
    "print!(",
    "eprint!(",
    ".label(",
    ".button(",
    ".heading(",
    ".link(",
    ".strong(",
    "RichText::new(",
    "Label::new(",
    "Button::new(",
    ".checkbox(",
    ".radio(",
    ".radio_value(",
    ".selectable_label(",
    ".selectable_value(",
    ".selected_text(",
    ".on_hover_text(",
    ".accessible_name(",
    ".balloon(",
    "notify_alone(",
    "error_text(",
    "banner(",
    "UsageError(",
    "PlatformError(",
    "title:",
    "body:",
];

/// (file, the line's text) pairs allowed, and why.
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "replica-core/src/platform.rs",
        "the share refused the login",
        "platform::fake, the test double's own message",
    ),
    (
        "replica/src/cli/mod.rs",
        "Replica",
        "the app's folder name, beside a UsageError",
    ),
    (
        "replica/src/cli/mod.rs",
        "history.db",
        "a file name, beside a UsageError",
    ),
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The string literals on a line (plain `"…"` ones; raw strings aren't used for prose here).
fn literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            // A char literal ('"') or a lifetime: skip to its end.
            let mut taken = 0;
            while let Some(&n) = chars.peek() {
                chars.next();
                taken += 1;
                if n == '\'' || taken > 3 {
                    break;
                }
            }
            continue;
        }
        if c != '"' {
            continue;
        }
        let mut text = String::new();
        while let Some(n) = chars.next() {
            match n {
                '\\' => {
                    chars.next();
                }
                '"' => break,
                _ => text.push(n),
            }
        }
        out.push(text);
    }
    out
}

/// Whether a literal holds a word once its `{placeholders}` are out.
fn has_words(literal: &str) -> bool {
    let mut bare = String::new();
    let mut depth = 0;
    for c in literal.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => bare.push(c),
            _ => {}
        }
    }
    bare.as_bytes()
        .windows(2)
        .any(|w| w[0].is_ascii_alphabetic() && w[1].is_ascii_alphabetic())
}

fn findings() -> Vec<String> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = ["replica", "replica-core", "replica-win"]
        .iter()
        .map(|c| workspace().join("crates").join(c).join("src"))
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
            .strip_prefix(workspace().join("crates"))
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let source = std::fs::read_to_string(&path).unwrap();
        for (n, line) in source.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            let code = line.split("//").next().unwrap_or("");
            if !SHOWS_TEXT.iter().any(|call| code.contains(call)) {
                continue;
            }
            if code.contains("tr(\"") || code.contains("tr!(") || code.contains("tracing::") {
                continue;
            }
            for literal in literals(code) {
                if !has_words(&literal) {
                    continue;
                }
                if ALLOWED
                    .iter()
                    .any(|(file, text, _)| rel == *file && literal == *text)
                {
                    continue;
                }
                found.push(format!("{rel}:{}: \"{literal}\"", n + 1));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn no_user_visible_text_is_hard_coded() {
    let found = findings();
    assert!(
        found.is_empty(),
        "user-visible text outside Fluent (move it to locales/en/):\n{}",
        found.join("\n")
    );
}

#[test]
fn the_check_finds_what_it_should() {
    assert_eq!(literals(r#"ui.label("Hello", '"', x)"#), ["Hello"]);
    assert!(has_words("Save the file"));
    assert!(!has_words("{text}"));
    assert!(!has_words("  | {line}"));
    assert!(!has_words("{}: {}"));
    assert!(has_words("{job}: done"));
}
