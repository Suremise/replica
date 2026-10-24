//! Exclusions: one set of rules for both engines.
//!
//! A job's sources each have their own exclude patterns. [`ExcludeRules`] turns them into
//! absolute globs, once, and both engines use those same globs: the mirror walker matches paths
//! against them, and the restic driver passes them as `--iexclude` (or `--exclude`) arguments.
//! So the two exclude exactly the same things.
//!
//! # Patterns
//!
//! - A pattern **without** a `/` matches a file or folder name at any depth under its source:
//!   `node_modules`, `dist`, `*.tmp`.
//! - A pattern **with** a `/` is a path from the source's root: `build/cache`, `docs/*.pdf`. A
//!   leading `**/` matches at any depth: `**/.godot`.
//! - `*` and `?` don't cross a `/`; `**` does. `[abc]` and `[a-z0-9]` are character classes;
//!   negated classes (`[!a]`, `[^a]`) aren't allowed, because restic and the walker read them
//!   differently.
//! - An excluded folder excludes everything in it.
//! - Matching ignores case by default, as Windows paths do.
//! - Not allowed: `{a,b}` alternation (restic doesn't support it), `..`, and absolute paths.
//!   A `\` is taken as a path separator.

use std::fmt;
use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::i18n::tr;
use crate::tr;

/// Patterns worth excluding from most development folders. A new source starts with these.
pub const COMMON_EXCLUDES: &[&str] = &["node_modules", "**/.godot", "dist", "out", "*.tmp"];

/// An exclude pattern that can't be used. Its text comes from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludeError {
    pub pattern: String,
    pub reason: ExcludeProblem,
}

/// Why a pattern can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcludeProblem {
    Empty,
    Absolute,
    Braces,
    Dots,
    UnclosedBracket,
    /// A character class beyond the plain characters and ranges both engines share.
    Class,
    /// The glob library refused it, with its own message.
    Glob(String),
}

impl fmt::Display for ExcludeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match &self.reason {
            ExcludeProblem::Empty => tr("error-exclude-empty"),
            ExcludeProblem::Absolute => tr("error-exclude-absolute"),
            ExcludeProblem::Braces => tr("error-exclude-braces"),
            ExcludeProblem::Dots => tr("error-exclude-dots"),
            ExcludeProblem::UnclosedBracket => tr("error-exclude-bracket"),
            ExcludeProblem::Class => tr("error-exclude-class"),
            ExcludeProblem::Glob(detail) => tr!("error-exclude-glob", detail = detail.as_str()),
        };
        f.write_str(&tr!(
            "error-exclude-invalid",
            pattern = self.pattern.as_str(),
            reason = reason
        ))
    }
}

impl std::error::Error for ExcludeError {}

/// Checks a pattern as [`ExcludeRules::new`] will build it. Used by settings validation.
pub fn check_pattern(pattern: &str) -> Result<(), ExcludeError> {
    relative_glob(pattern).map(|_| ())
}

/// A job's exclusions as absolute globs, ready for the walker and for restic.
#[derive(Debug, Clone)]
pub struct ExcludeRules {
    globs: Vec<String>,
    set: GlobSet,
    case_insensitive: bool,
}

impl ExcludeRules {
    /// Rules for a set of sources, each a root folder with its own patterns.
    pub fn new<'a, P>(
        sources: impl IntoIterator<Item = (&'a Path, P)>,
        case_insensitive: bool,
    ) -> Result<Self, ExcludeError>
    where
        P: IntoIterator,
        P::Item: AsRef<str>,
    {
        let mut globs = Vec::new();
        for (root, patterns) in sources {
            let root = escape_literal(&normalise(root));
            let root = root.trim_end_matches('/');
            for pattern in patterns {
                globs.push(format!("{root}/{}", relative_glob(pattern.as_ref())?));
            }
        }
        Self::build(globs, case_insensitive)
    }

    /// The same rules, also excluding these exact files or folders (absolute paths), such as a
    /// live database whose consistent copy is backed up instead.
    pub fn with_exact_paths<'a>(
        self,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<Self, ExcludeError> {
        let mut globs = self.globs;
        globs.extend(paths.into_iter().map(|p| escape_literal(&normalise(p))));
        Self::build(globs, self.case_insensitive)
    }

    fn build(globs: Vec<String>, case_insensitive: bool) -> Result<Self, ExcludeError> {
        let mut builder = GlobSetBuilder::new();
        for glob in &globs {
            let built = build_glob(glob, case_insensitive).map_err(|e| ExcludeError {
                pattern: glob.clone(),
                reason: ExcludeProblem::Glob(e.kind().to_string()),
            })?;
            builder.add(built);
        }
        let set = builder.build().map_err(|e| ExcludeError {
            pattern: String::new(),
            reason: ExcludeProblem::Glob(e.to_string()),
        })?;
        Ok(ExcludeRules {
            globs,
            set,
            case_insensitive,
        })
    }

    /// The absolute globs, with `/` separators.
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    /// Whether this exact path matches a rule. The walker calls this on each entry and doesn't
    /// descend into a matched folder.
    pub fn matches(&self, path: &Path) -> bool {
        self.set.is_match(normalise(path))
    }

    /// Whether a path is excluded: it, or a folder above it, matches a rule.
    pub fn is_excluded(&self, path: &Path) -> bool {
        // By `/`-separated text, so a Windows path is split the same on every OS.
        let text = normalise(path);
        Path::new(&text)
            .ancestors()
            .any(|p| !p.as_os_str().is_empty() && self.matches(p))
    }

    /// The same rules as restic arguments: a flag and a glob per rule, as separate arguments
    /// (no shell quoting needed).
    pub fn restic_args(&self) -> Vec<String> {
        let flag = if self.case_insensitive {
            "--iexclude"
        } else {
            "--exclude"
        };
        self.globs
            .iter()
            .flat_map(|g| [flag.to_string(), g.clone()])
            .collect()
    }
}

/// A path as text with `/` separators and no `\\?\` prefix.
fn normalise(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    match text.strip_prefix("//?/UNC/") {
        Some(rest) => format!("//{rest}"),
        None => text
            .strip_prefix("//?/")
            .map(str::to_string)
            .unwrap_or(text),
    }
}

/// Root paths are literal text: each glob character is wrapped in a one-character class, which
/// both globset and restic read the same way. A `]` outside a class is already literal to both.
pub(crate) fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '*' | '?' | '[' | '{' | '}' => {
                out.push('[');
                out.push(c);
                out.push(']');
            }
            c => out.push(c),
        }
    }
    out
}

/// Builds one glob with the options both engines share.
fn build_glob(glob: &str, case_insensitive: bool) -> Result<globset::Glob, globset::Error> {
    GlobBuilder::new(glob)
        .literal_separator(true)
        .case_insensitive(case_insensitive)
        .backslash_escape(false)
        .build()
}

/// A user's pattern as a glob relative to its source's root, checked to mean the same to
/// globset and to restic, and to compile.
fn relative_glob(pattern: &str) -> Result<String, ExcludeError> {
    let err = |reason| ExcludeError {
        pattern: pattern.to_string(),
        reason,
    };
    let p = pattern.trim().replace('\\', "/");
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        return Err(err(ExcludeProblem::Empty));
    }
    if p.starts_with('/') || p.contains(':') {
        return Err(err(ExcludeProblem::Absolute));
    }
    if p.contains(['{', '}']) {
        return Err(err(ExcludeProblem::Braces));
    }
    if p.split('/')
        .any(|part| part == ".." || part == "." || part.is_empty())
    {
        return Err(err(ExcludeProblem::Dots));
    }
    check_classes(p).map_err(err)?;
    let glob = if p.contains('/') {
        p.to_string()
    } else {
        format!("**/{p}")
    };
    build_glob(&format!("/root/{glob}"), true)
        .map_err(|e| err(ExcludeProblem::Glob(e.kind().to_string())))?;
    Ok(glob)
}

/// Character classes are limited to what globset and restic (Go's `filepath.Match`) agree on:
/// characters and ascending ranges, as in `[abc]` or `[a-z0-9]`. Not allowed: negation (`[!a]`
/// negates in globset but is a class of `!` and `a` in Go), an empty class, a leading or
/// trailing `-`, a `]` that closes nothing, and `[` or `/` inside a class.
fn check_classes(p: &str) -> Result<(), ExcludeProblem> {
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            ']' => return Err(ExcludeProblem::UnclosedBracket),
            '[' => {
                let mut class = Vec::new();
                loop {
                    match chars.next() {
                        None => return Err(ExcludeProblem::UnclosedBracket),
                        Some(']') => break,
                        Some(c) => class.push(c),
                    }
                }
                check_class(&class)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_class(class: &[char]) -> Result<(), ExcludeProblem> {
    let bad = Err(ExcludeProblem::Class);
    if class.is_empty() || matches!(class[0], '!' | '^') {
        return bad;
    }
    let mut i = 0;
    while i < class.len() {
        let c = class[i];
        if matches!(c, '[' | '/' | '-') {
            return bad;
        }
        if class.get(i + 1) == Some(&'-') {
            match class.get(i + 2) {
                Some(&hi) if !matches!(hi, '[' | '/' | '-') && c <= hi => i += 3,
                _ => return bad,
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(patterns: &[&str]) -> ExcludeRules {
        ExcludeRules::new(
            [(Path::new("D:/Work/Hive"), patterns.iter().copied())],
            true,
        )
        .unwrap()
    }

    #[test]
    fn name_patterns_match_at_any_depth() {
        let r = rules(COMMON_EXCLUDES);
        for p in [
            "D:/Work/Hive/node_modules",
            "D:/Work/Hive/app/node_modules",
            "D:/Work/Hive/game/.godot",
            "D:/Work/Hive/a/b/.godot",
            "D:/Work/Hive/dist",
            "D:/Work/Hive/web/out",
            "D:/Work/Hive/x.tmp",
            "D:/Work/Hive/a/b/c.TMP",
        ] {
            assert!(r.matches(Path::new(p)), "{p}");
        }
        for p in [
            "D:/Work/Hive/src/main.rs",
            "D:/Work/Hive/distant",
            "D:/Work/Hive/output",
            "D:/Work/Hive/x.tmpl",
            "D:/Work/Other/node_modules",
        ] {
            assert!(!r.matches(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn excluded_folders_exclude_their_contents() {
        let r = rules(&["node_modules"]);
        assert!(r.is_excluded(Path::new("D:/Work/Hive/app/node_modules/react/index.js")));
        assert!(!r.matches(Path::new("D:/Work/Hive/app/node_modules/react/index.js")));
        assert!(!r.is_excluded(Path::new("D:/Work/Hive/app/src/index.js")));
    }

    #[test]
    fn path_patterns_are_anchored_at_the_source() {
        let r = rules(&["build/cache", "docs/*.pdf"]);
        assert!(r.matches(Path::new("D:/Work/Hive/build/cache")));
        assert!(!r.matches(Path::new("D:/Work/Hive/sub/build/cache")));
        assert!(r.matches(Path::new("D:/Work/Hive/docs/a.pdf")));
        assert!(
            !r.matches(Path::new("D:/Work/Hive/docs/sub/a.pdf")),
            "* doesn't cross /"
        );
    }

    #[test]
    fn windows_paths_and_case() {
        let r = ExcludeRules::new([(Path::new(r"D:\Work\Hive"), ["Node_Modules"])], true).unwrap();
        assert!(r.matches(Path::new(r"d:\work\hive\APP\node_modules")));
        assert!(r.matches(Path::new(r"\\?\D:\Work\Hive\node_modules")));
        let exact =
            ExcludeRules::new([(Path::new("D:/Work/Hive"), ["Node_Modules"])], false).unwrap();
        assert!(!exact.matches(Path::new("D:/Work/Hive/node_modules")));
    }

    #[test]
    fn source_paths_with_glob_characters_are_literal() {
        let r = ExcludeRules::new([(Path::new("D:/Work [old]/{x}"), ["*.tmp"])], true).unwrap();
        assert!(r.matches(Path::new("D:/Work [old]/{x}/a.tmp")));
        assert!(!r.matches(Path::new("D:/Work o/{x}/a.tmp")));
    }

    #[test]
    fn exact_paths_exclude_only_themselves() {
        let r = rules(&["*.tmp"])
            .with_exact_paths([
                Path::new("D:/Work/Hive/data/app.db"),
                Path::new("D:/Work/Hive/a [1].db"),
            ])
            .unwrap();
        assert!(r.matches(Path::new("D:/Work/Hive/data/app.db")));
        assert!(
            r.matches(Path::new("D:/Work/Hive/a [1].db")),
            "glob characters are literal"
        );
        assert!(!r.matches(Path::new("D:/Work/Hive/a 1.db")));
        assert!(!r.matches(Path::new("D:/Work/Hive/other/app.db")));
        assert!(!r.matches(Path::new("D:/Work/Hive/data/app.db-backup")));
        assert!(
            r.matches(Path::new("D:/Work/Hive/x.tmp")),
            "the patterns still apply"
        );
        assert_eq!(r.restic_args().len(), 6);
    }

    #[test]
    fn unc_sources() {
        let r = ExcludeRules::new([(Path::new(r"\\nas\share\photos"), ["*.tmp"])], true).unwrap();
        assert_eq!(r.globs(), ["//nas/share/photos/**/*.tmp"]);
        assert!(r.matches(Path::new(r"\\nas\share\photos\2026\a.tmp")));
    }

    #[test]
    fn each_source_has_its_own_patterns() {
        let r = ExcludeRules::new(
            [
                (Path::new("D:/A"), vec!["dist"]),
                (Path::new("D:/B"), vec!["*.log"]),
            ],
            true,
        )
        .unwrap();
        assert!(r.matches(Path::new("D:/A/dist")));
        assert!(!r.matches(Path::new("D:/B/dist")));
        assert!(r.matches(Path::new("D:/B/x.log")));
        assert!(!r.matches(Path::new("D:/A/x.log")));
    }

    #[test]
    fn rejects_unusable_patterns() {
        for p in [
            "", "  ", "/abs", "C:/x", "../up", "a/../b", "a//b", "{a,b}", "[abc",
        ] {
            assert!(check_pattern(p).is_err(), "{p:?}");
        }
        for p in ["a", "a/", r"a\b", "**/.godot", "*.tmp", "[ab]c"] {
            assert!(check_pattern(p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn classes_are_limited_to_what_both_engines_agree_on() {
        let class = |p: &str| check_pattern(p).map_err(|e| e.reason);
        for p in [
            "[!a].txt", "[^a].txt", "[!]", "[]", "[-a]", "[a-]", "[a--]", "[a[b]", "[a/b]",
        ] {
            assert_eq!(class(p), Err(ExcludeProblem::Class), "{p:?}");
        }
        for p in ["a]", "[a", "x[a-z"] {
            assert_eq!(class(p), Err(ExcludeProblem::UnclosedBracket), "{p:?}");
        }
        assert_eq!(
            class("[z-a]"),
            Err(ExcludeProblem::Class),
            "a descending range"
        );
        for p in ["[abc].txt", "[a-z0-9]x", "file[12].txt"] {
            assert_eq!(class(p), Ok(()), "{p:?}");
        }
    }

    #[test]
    fn every_pattern_that_checks_also_builds() {
        let patterns = [
            "a",
            "*.tmp",
            "**/.godot",
            "[a-z]x",
            "docs/*.pdf",
            "a?c",
            "x/**/y",
            "[0-9][0-9].log",
        ];
        for p in patterns {
            check_pattern(p).unwrap();
            ExcludeRules::new([(Path::new("D:/W"), [p])], true).unwrap();
            ExcludeRules::new([(Path::new("D:/W"), [p])], false).unwrap();
        }
    }

    #[test]
    fn restic_args_are_the_walker_globs() {
        let r = rules(COMMON_EXCLUDES);
        let args = r.restic_args();
        assert_eq!(args.len(), 2 * COMMON_EXCLUDES.len());
        assert_eq!(args[0], "--iexclude");
        assert_eq!(args[1], "D:/Work/Hive/**/node_modules");
        assert_eq!(
            args[3], "D:/Work/Hive/**/.godot",
            "a leading **/ isn't doubled"
        );

        let insensitive = rules(&["x"]);
        assert_eq!(insensitive.restic_args()[0], "--iexclude");
        let sensitive = ExcludeRules::new([(Path::new("D:/W"), ["x"])], false).unwrap();
        assert_eq!(sensitive.restic_args()[0], "--exclude");
    }
}
