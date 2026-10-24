//! Replica's strings, compiled into the exe so the portable copy needs no extra files.
//!
//! The only app-specific part of [`super`]. To add a file or a language, see "Strings and
//! languages" in `docs/ARCHITECTURE.md`.

use super::Locale;

/// The language used when a key or a language is missing.
pub const FALLBACK: &str = "en";

macro_rules! ftl {
    ($lang:literal, $file:literal) => {
        (
            $file,
            include_str!(concat!("../../../../locales/", $lang, "/", $file)),
        )
    };
}

pub static LOCALES: &[Locale] = &[Locale {
    tag: "en",
    files: &[
        ftl!("en", "common.ftl"),
        ftl!("en", "cli.ftl"),
        ftl!("en", "tray.ftl"),
        ftl!("en", "notifications.ftl"),
        ftl!("en", "window.ftl"),
        ftl!("en", "errors.ftl"),
        ftl!("en", "tasks.ftl"),
        ftl!("en", "installer.ftl"),
    ],
}];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Localizer;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn english() -> Localizer {
        Localizer::new(LOCALES, FALLBACK, FALLBACK)
    }

    /// The message keys a locale defines (terms, starting with `-`, and comments left out).
    fn keys(locale: &Locale) -> BTreeSet<String> {
        locale
            .files
            .iter()
            .flat_map(|(_, source)| source.lines())
            .filter(|line| line.starts_with(|c: char| c.is_ascii_alphabetic()))
            .filter_map(|line| line.split_once('=').map(|(key, _)| key.trim().to_owned()))
            .collect()
    }

    #[test]
    fn every_file_parses_without_duplicates() {
        for locale in LOCALES {
            let (_, errors) = Localizer::build(std::slice::from_ref(locale), locale.tag, FALLBACK);
            let report: String = errors.iter().map(|e| format!("{e}\n")).collect();
            assert!(errors.is_empty(), "{report}");
        }
    }

    #[test]
    fn every_key_has_a_value() {
        for locale in LOCALES {
            let localizer = Localizer::new(std::slice::from_ref(locale), locale.tag, locale.tag);
            for key in keys(locale) {
                assert!(localizer.has(&key), "{}: {key} has no value", locale.tag);
            }
        }
    }

    /// Git may check the files out with CRLF line ends; multi-line strings mustn't keep the CR.
    #[test]
    fn no_string_carries_a_carriage_return() {
        for locale in LOCALES {
            let localizer = Localizer::new(std::slice::from_ref(locale), locale.tag, locale.tag);
            for key in keys(locale) {
                assert!(!localizer.tr(&key).contains('\r'), "{}: {key}", locale.tag);
            }
        }
    }

    #[test]
    fn every_ftl_file_on_disk_is_embedded() {
        for dir in std::fs::read_dir(workspace().join("locales")).unwrap() {
            let dir = dir.unwrap().path();
            let tag = dir.file_name().unwrap().to_string_lossy().into_owned();
            let locale = LOCALES.iter().find(|l| l.tag == tag);
            assert!(locale.is_some(), "locales/{tag} isn't in LOCALES");
            for file in std::fs::read_dir(&dir).unwrap() {
                let name = file.unwrap().file_name().to_string_lossy().into_owned();
                if name.ends_with(".ftl") {
                    let listed = locale.unwrap().files.iter().any(|(f, _)| *f == name);
                    assert!(listed, "locales/{tag}/{name} isn't in LOCALES");
                }
            }
        }
    }

    #[test]
    fn other_languages_only_translate_keys_english_has() {
        let english = keys(LOCALES.iter().find(|l| l.tag == FALLBACK).unwrap());
        for locale in LOCALES.iter().filter(|l| l.tag != FALLBACK) {
            let extra: Vec<_> = keys(locale).difference(&english).cloned().collect();
            assert!(
                extra.is_empty(),
                "{} has keys English doesn't: {extra:?}",
                locale.tag
            );
        }
    }

    /// Every key the code asks for, as `tr("…")`, `tr_args("…"` or `tr!("…"`, exists in English.
    #[test]
    fn every_key_used_in_the_code_exists() {
        let english = english();
        let mut missing = Vec::new();
        let mut stack = vec![workspace().join("crates")];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            } else if path.extension().is_some_and(|e| e == "rs")
                // This module's own tests use made-up keys.
                && !path.parent().is_some_and(|p| p.ends_with("i18n"))
            {
                let source = std::fs::read_to_string(&path).unwrap();
                for key in used_keys(&source) {
                    if !english.has(&key) {
                        missing.push(format!("{}: {key}", path.display()));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "missing keys:\n{}", missing.join("\n"));
    }

    /// The literal keys passed to `tr(`, `tr_args(` and `tr!(`. Keys built at run time aren't
    /// seen, so prefer literals.
    fn used_keys(source: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for call in ["tr(\"", "tr_args(\"", "tr!(\""] {
            let mut rest = source;
            while let Some(start) = rest.find(call) {
                let after_ident = rest[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
                rest = &rest[start + call.len()..];
                if let Some(end) = rest.find('"')
                    && !after_ident
                    && !rest[..end].contains(char::is_whitespace)
                {
                    keys.push(rest[..end].to_owned());
                }
            }
        }
        keys
    }

    #[test]
    fn finds_used_keys() {
        let source =
            r#"tr("a-b"); i18n::tr_args("c", &x); tr!("d", job = j); attr("no"); xtr("no")"#;
        let mut found = used_keys(source);
        found.sort();
        assert_eq!(found, ["a-b", "c", "d"]);
    }

    #[test]
    fn the_core_strings_exist() {
        let english = english();
        for key in [
            "result-ok",
            "result-failed",
            "result-partly",
            "result-skipped",
            "skip-target-not-available",
            "tray-run-all",
            "tray-run-job",
            "tray-open",
            "tray-pause-schedules",
            "tray-start-with-windows",
            "tray-quit",
            "notify-run-failed-title",
            "tab-jobs",
            "tab-history",
            "tab-restore",
            "tab-settings",
            "mirror-warning-body",
            "password-save-body",
            "error-target-not-available",
            "error-wrong-password",
            "error-settings-corrupt",
        ] {
            assert!(english.has(key), "{key}");
        }
    }
}
