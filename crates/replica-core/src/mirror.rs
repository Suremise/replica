//! The mirror engine: an exact copy of each source, deletions included.
//!
//! A mirror is only for targets the user marks as such (the job's `mode = "mirror"`, with
//! `mirror_deletes_confirmed`), so it is careful:
//!
//! - **Layout:** each source is mirrored into `<target>\<the source's name>`. The engine only
//!   ever writes and deletes inside those folders; anything else in the target is left alone.
//! - **Refused before anything changes** ([`MirrorError`]): a missing source; a source with no
//!   files (a disconnected drive must never wipe its mirror); a source or target that is a drive
//!   root; a target that is a source or inside or around one; a source's folder in the target
//!   that is a link or junction; two sources with the same name; and a run that would delete
//!   more than the job's `mirror_delete_limit_percent` of the target's files (100 allows any).
//! - **Links and junctions are never followed**, in a source (skipped, and counted) or in the
//!   target (removed as links, never what they point to). If one in the target can't be
//!   removed, nothing is written beneath it.
//! - **Copying** goes to a staging file of its own, beside the file: a new, uniquely named file
//!   created for this copy alone, so it can never be an existing file or a hard link to one.
//!   Only once it's complete is it renamed into place, so a half-copied file never replaces a
//!   good one. The modified time is copied; a read-only file stays read-only.
//! - **A type conflict** (a file in the source where the target has a folder, or the other way
//!   round, at any depth) is reported as "type conflict: resolve by hand" and the target's item
//!   is left exactly as it is (deliberately, for v1). The rest of the tree still
//!   mirrors; the run is "partly done".
//! - **Cancelling** stops between files, and in a large file between chunks. Each file already
//!   in place is complete; staging files are removed, and so are new folders still empty.
//! - **Comparing:** size and modified time (within [`TIME_TOLERANCE`], for FAT and exFAT drives),
//!   or the content too with [`Compare::Content`].
//! - **Deleting** comes after copying, one entry at a time, deepest first: files and links, then
//!   folders once empty. Paths the source's exclusions match are left alone in the target, and
//!   so is every folder above them, and anything under a folder that couldn't be read.
//! - **Problems with single files** (in use, access denied) are reported in
//!   [`MirrorOutcome::failed`] and the run carries on: a "partly" result.
//! - Long paths work: Rust's standard library adds the `\\?\` prefix when a path needs it.
//!
//! The engine is native Rust rather than robocopy: see "The mirror engine" in
//! `docs/ARCHITECTURE.md` for the measurements behind that choice.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use crate::cancel::CancelToken;
use crate::exclude::{ExcludeError, ExcludeRules};
use crate::i18n::tr;
use crate::job::Source;
use crate::paths;
use crate::tr;

/// Modified times this close count as the same: FAT and exFAT store them to 2 seconds.
pub const TIME_TOLERANCE: Duration = Duration::from_secs(2);

/// The copy buffer for large files, and how often they report progress and check for a cancel.
const CHUNK: usize = 1 << 20;

/// Files this size and larger are copied a chunk at a time, for progress and cancelling.
const LARGE_FILE: u64 = 4 << 20;

/// The most files copied at once.
const MAX_WORKERS: usize = 8;

/// How files are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compare {
    /// Same size and modified time means unchanged (the default).
    #[default]
    SizeAndTime,
    /// As well, read both files and compare their content.
    Content,
}

/// What to mirror, and where.
#[derive(Debug, Clone)]
pub struct MirrorRequest<'a> {
    pub sources: &'a [Source],
    /// The target folder (resolved: see [`crate::platform::resolve_target`]).
    pub target: &'a Path,
    pub compare: Compare,
    /// The job's `mirror_delete_limit_percent`.
    pub delete_limit_percent: u32,
    /// Whether exclusions ignore case (Windows: yes).
    pub case_insensitive: bool,
    /// Exact files or folders to leave out as well (absolute paths), such as a live database
    /// whose consistent copy is mirrored instead.
    pub exclude_paths: &'a [PathBuf],
}

/// How far a mirror has got, for the tray.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirrorProgress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// The file copied last.
    pub current: Option<PathBuf>,
}

/// A file or folder the run couldn't copy or delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorFailure {
    pub path: PathBuf,
    pub message: String,
}

/// What a mirror run did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MirrorOutcome {
    pub copied: u64,
    pub deleted: u64,
    pub unchanged: u64,
    /// Links and junctions in the sources, which aren't followed.
    pub skipped: u64,
    pub failed: Vec<MirrorFailure>,
    pub bytes_copied: u64,
    pub duration: Duration,
}

impl MirrorOutcome {
    /// Some files couldn't be copied or deleted: the run is "partly done".
    pub fn partial(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// Why a mirror run was refused or stopped. Nothing is changed when it's refused. Its text comes
/// from Fluent (`errors.ftl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorError {
    SourceMissing {
        path: PathBuf,
    },
    /// The source has no files: perhaps a drive that isn't connected.
    SourceEmpty {
        path: PathBuf,
    },
    SourceIsRoot {
        path: PathBuf,
    },
    TargetIsRoot {
        path: PathBuf,
    },
    TargetOverlapsSource {
        target: PathBuf,
        source: PathBuf,
    },
    /// A source's folder in the target is a link or junction, which a mirror never writes
    /// through.
    DestinationIsLink {
        path: PathBuf,
    },
    DuplicateName {
        name: String,
    },
    /// It would delete `would_delete` of the target's `of` files, over the job's limit.
    TooManyDeletions {
        would_delete: u64,
        of: u64,
        limit_percent: u32,
    },
    BadExclude(ExcludeError),
    /// A folder couldn't be read or made before the run started.
    Io {
        path: PathBuf,
        detail: String,
    },
    Cancelled,
}

impl fmt::Display for MirrorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = |p: &PathBuf| p.display().to_string();
        let text = match self {
            MirrorError::SourceMissing { path } => {
                tr!("error-mirror-source-missing", path = p(path))
            }
            MirrorError::SourceEmpty { path } => tr!("error-mirror-source-empty", path = p(path)),
            MirrorError::SourceIsRoot { path } => tr!("error-mirror-source-root", path = p(path)),
            MirrorError::TargetIsRoot { path } => tr!("error-mirror-target-root", path = p(path)),
            MirrorError::TargetOverlapsSource { target, source } => tr!(
                "error-mirror-target-overlaps",
                target = p(target),
                source = p(source)
            ),
            MirrorError::DestinationIsLink { path } => {
                tr!("error-mirror-destination-link", path = p(path))
            }
            MirrorError::DuplicateName { name } => {
                tr!("error-mirror-duplicate-name", name = name.as_str())
            }
            MirrorError::TooManyDeletions {
                would_delete,
                of,
                limit_percent,
            } => tr!(
                "error-mirror-too-many-deletions",
                count = *would_delete,
                total = *of,
                limit = *limit_percent
            ),
            MirrorError::BadExclude(e) => e.to_string(),
            MirrorError::Io { path, detail } => {
                tr!("error-mirror-io", path = p(path), detail = detail.as_str())
            }
            MirrorError::Cancelled => tr("error-cancelled"),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for MirrorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File {
        size: u64,
        modified: Option<SystemTime>,
    },
    Dir,
    /// A link or junction: never followed.
    Link,
}

/// A walked tree: entries by path relative to its root, entries left out because the
/// exclusions match them, and folders that couldn't be read.
#[derive(Default)]
struct Tree {
    entries: BTreeMap<PathBuf, Kind>,
    excluded: Vec<PathBuf>,
    unreadable: Vec<(PathBuf, String)>,
}

impl Tree {
    fn files(&self) -> u64 {
        self.entries
            .values()
            .filter(|k| matches!(k, Kind::File { .. }))
            .count() as u64
    }

    /// Whether something the run must keep is at or under `dir`: an excluded entry, or a folder
    /// that couldn't be read.
    fn protects(&self, dir: &Path) -> bool {
        self.excluded.iter().any(|p| p.starts_with(dir))
            || self.unreadable.iter().any(|(p, _)| p.starts_with(dir))
    }
}

/// One source's part of the plan.
struct Part<'a> {
    source: &'a Path,
    dest: PathBuf,
    is_file: bool,
    tree: Tree,
    dest_tree: Tree,
}

/// How a planned folder comes about in the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MakeDir {
    New,
    /// A link or junction is there: it's removed (never what it points to), then the folder made.
    OverLink,
}

/// What a copy replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replaces {
    Nothing,
    File,
    Link,
}

struct Copy {
    from: PathBuf,
    to: PathBuf,
    size: u64,
    replaces: Replaces,
}

struct Delete {
    path: PathBuf,
    kind: Kind,
}

#[derive(Default)]
struct Plan {
    dirs: Vec<(PathBuf, MakeDir)>,
    copies: Vec<Copy>,
    deletes: Vec<Delete>,
    /// Type conflicts: left untouched, and reported.
    conflicts: Vec<MirrorFailure>,
    unchanged: u64,
    delete_files: u64,
    /// The files in the target this part could delete from: the denominator of the guard.
    dest_files: u64,
}

/// Mirrors each source into `<target>\<its name>`.
pub fn mirror(
    request: &MirrorRequest<'_>,
    on_progress: &mut dyn FnMut(&MirrorProgress),
    cancel: &CancelToken,
) -> Result<MirrorOutcome, MirrorError> {
    let start = Instant::now();
    let parts = prepare(request)?;

    // The plan and the deletion guard, before anything changes.
    let mut all = Plan::default();
    let mut dest_files = 0;
    for part in &parts {
        let plan = plan(part, request.compare);
        all.unchanged += plan.unchanged;
        all.delete_files += plan.delete_files;
        all.dirs.extend(plan.dirs);
        all.copies.extend(plan.copies);
        all.deletes.extend(plan.deletes);
        all.conflicts.extend(plan.conflicts);
        dest_files += plan.dest_files;
    }
    // 100 allows any deletion; below it, the share of the target's files is checked.
    let limit = request.delete_limit_percent.min(100);
    if limit < 100 && all.delete_files > 0 && all.delete_files * 100 > dest_files * u64::from(limit)
    {
        return Err(MirrorError::TooManyDeletions {
            would_delete: all.delete_files,
            of: dest_files,
            limit_percent: limit,
        });
    }

    let mut outcome = MirrorOutcome {
        unchanged: all.unchanged,
        skipped: parts
            .iter()
            .map(|p| {
                p.tree
                    .entries
                    .values()
                    .filter(|k| **k == Kind::Link)
                    .count() as u64
            })
            .sum(),
        failed: all.conflicts,
        ..Default::default()
    };
    for part in &parts {
        for (dir, why) in &part.tree.unreadable {
            outcome.failed.push(MirrorFailure {
                path: part.source.join(dir),
                message: why.clone(),
            });
        }
    }

    // Folders, shallowest first. A folder that can't be made blocks everything beneath it.
    let mut blocked: Vec<PathBuf> = Vec::new();
    let mut created: Vec<PathBuf> = Vec::new();
    for part in &parts {
        if let Err(e) = fs::create_dir_all(part.dest.parent().unwrap_or(&part.dest)) {
            outcome.failed.push(failure(&part.dest, &e));
            blocked.push(part.dest.clone());
        }
    }
    let mut progress = MirrorProgress::default();
    all.dirs.sort_by_key(|(p, _)| p.components().count());
    for (dir, how) in &all.dirs {
        if cancel.is_cancelled() {
            remove_created(&created);
            return Err(MirrorError::Cancelled);
        }
        if is_blocked(&blocked, dir) {
            continue;
        }
        let made = match how {
            MakeDir::New => fs::create_dir(dir),
            MakeDir::OverLink => remove_link(dir).and_then(|()| fs::create_dir(dir)),
        };
        match made {
            Ok(()) => created.push(dir.clone()),
            Err(e) => {
                outcome.failed.push(failure(dir, &e));
                blocked.push(dir.clone());
            }
        }
        progress.current = Some(dir.clone());
        on_progress(&progress);
    }

    // Copies, beneath folders that were made.
    let (copies, cut_off): (Vec<Copy>, Vec<Copy>) = all
        .copies
        .into_iter()
        .partition(|c| !is_blocked(&blocked, c.to.parent().unwrap_or(&c.to)));
    for copy in &cut_off {
        outcome.failed.push(MirrorFailure {
            path: copy.from.clone(),
            message: tr("error-mirror-parent-not-made"),
        });
    }
    progress = MirrorProgress {
        files_total: copies.len() as u64,
        bytes_total: copies.iter().map(|c| c.size).sum(),
        ..Default::default()
    };
    on_progress(&progress);
    if copy_all(&copies, &mut outcome, &mut progress, on_progress, cancel) {
        // Each copy already in place is complete; staging files are gone, and so are the new
        // folders still empty.
        remove_created(&created);
        return Err(MirrorError::Cancelled);
    }

    // Extras last, one at a time, deepest first, so a folder is empty when its turn comes.
    all.deletes
        .sort_by_key(|d| std::cmp::Reverse(d.path.components().count()));
    for extra in &all.deletes {
        check_cancel(cancel)?;
        let removed = match extra.kind {
            Kind::File { .. } => remove_file(&extra.path),
            Kind::Link => remove_link(&extra.path),
            Kind::Dir => fs::remove_dir(&extra.path),
        };
        match removed {
            Ok(()) => outcome.deleted += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => outcome.failed.push(failure(&extra.path, &e)),
        }
    }
    outcome.failed.sort_by(|a, b| a.path.cmp(&b.path));
    outcome.duration = start.elapsed();
    progress.current = None;
    on_progress(&progress);
    Ok(outcome)
}

fn is_blocked(blocked: &[PathBuf], path: &Path) -> bool {
    blocked.iter().any(|b| path.starts_with(b))
}

/// Removes the folders this run made that are still empty, deepest first.
fn remove_created(created: &[PathBuf]) {
    for dir in created.iter().rev() {
        let _ = fs::remove_dir(dir);
    }
}

fn check_cancel(cancel: &CancelToken) -> Result<(), MirrorError> {
    if cancel.is_cancelled() {
        Err(MirrorError::Cancelled)
    } else {
        Ok(())
    }
}

fn failure(path: &Path, e: &io::Error) -> MirrorFailure {
    MirrorFailure {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// The safety checks, the exclusions and both trees, before anything is planned.
fn prepare<'a>(request: &MirrorRequest<'a>) -> Result<Vec<Part<'a>>, MirrorError> {
    let io_err = |path: &Path, e: io::Error| MirrorError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    };
    let target = paths::real_path(request.target).map_err(|e| io_err(request.target, e))?;
    if paths::is_root(&target) {
        return Err(MirrorError::TargetIsRoot {
            path: request.target.to_path_buf(),
        });
    }
    let mut names = HashSet::new();
    let mut parts = Vec::new();
    for source in request.sources {
        let path = source.path.as_path();
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(MirrorError::SourceMissing {
                    path: path.to_path_buf(),
                });
            }
            Err(e) => return Err(io_err(path, e)),
        };
        let real = paths::real_path(path).map_err(|e| io_err(path, e))?;
        let Some(name) = real.file_name().filter(|_| !paths::is_root(&real)) else {
            return Err(MirrorError::SourceIsRoot {
                path: path.to_path_buf(),
            });
        };
        if paths::overlap(&target, &real) {
            return Err(MirrorError::TargetOverlapsSource {
                target: request.target.to_path_buf(),
                source: path.to_path_buf(),
            });
        }
        if !names.insert(name.to_string_lossy().to_lowercase()) {
            return Err(MirrorError::DuplicateName {
                name: name.to_string_lossy().into_owned(),
            });
        }
        let dest = target.join(name);
        // The source's own folder in the target is never a link: the mirror would write
        // wherever it points.
        if let Ok(m) = fs::symlink_metadata(&dest)
            && m.file_type().is_symlink()
        {
            return Err(MirrorError::DestinationIsLink { path: dest });
        }
        let rules = ExcludeRules::new([(path, source.exclude.iter())], request.case_insensitive)
            .and_then(|r| r.with_exact_paths(request.exclude_paths.iter().map(PathBuf::as_path)))
            .map_err(MirrorError::BadExclude)?;
        if !meta.is_dir() && rules.matches(path) {
            // A file source that's excluded by exact path (a live database) isn't mirrored.
            continue;
        }
        let (tree, dest_tree) = if meta.is_dir() {
            // Both trees at once: walking is mostly waiting on the disk.
            let (tree, dest_tree) = std::thread::scope(|scope| {
                let dest_walk = scope.spawn(|| match fs::symlink_metadata(&dest) {
                    Ok(m) if m.is_dir() => walk(&dest, Some((&rules, path))),
                    _ => Tree::default(),
                });
                let tree = walk(path, Some((&rules, path)));
                (tree, dest_walk.join().unwrap_or_default())
            });
            if tree.files() == 0 && tree.unreadable.is_empty() {
                return Err(MirrorError::SourceEmpty {
                    path: path.to_path_buf(),
                });
            }
            if tree.files() == 0 {
                let (dir, why) = &tree.unreadable[0];
                return Err(io_err(&path.join(dir), io::Error::other(why.clone())));
            }
            (tree, dest_tree)
        } else {
            let mut tree = Tree::default();
            tree.entries.insert(PathBuf::new(), kind_of(&meta));
            (tree, Tree::default())
        };
        parts.push(Part {
            source: path,
            dest,
            is_file: !meta.is_dir(),
            tree,
            dest_tree,
        });
    }
    Ok(parts)
}

fn kind_of(meta: &fs::Metadata) -> Kind {
    if meta.file_type().is_symlink() {
        Kind::Link
    } else if meta.is_dir() {
        Kind::Dir
    } else {
        Kind::File {
            size: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

/// Walks a tree without following links. What `rules` exclude is recorded, not walked. For a
/// target tree, the rules are the source's, applied to each target path's place in the source.
fn walk(root: &Path, rules: Option<(&ExcludeRules, &Path)>) -> Tree {
    let mut tree = Tree::default();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let entries = match fs::read_dir(root.join(&rel)) {
            Ok(entries) => entries,
            Err(e) => {
                tree.unreadable.push((rel, e.to_string()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    tree.unreadable.push((rel.clone(), e.to_string()));
                    continue;
                }
            };
            let child = rel.join(entry.file_name());
            if let Some((rules, source_root)) = rules
                && rules.matches(&source_root.join(&child))
            {
                tree.excluded.push(child);
                continue;
            }
            let kind = match entry.metadata() {
                Ok(meta) => kind_of(&meta),
                Err(e) => {
                    tree.unreadable.push((child, e.to_string()));
                    continue;
                }
            };
            if kind == Kind::Dir {
                stack.push(child.clone());
            }
            tree.entries.insert(child, kind);
        }
    }
    tree
}

fn plan(part: &Part<'_>, compare: Compare) -> Plan {
    let mut plan = Plan {
        dest_files: part.dest_tree.files(),
        ..Default::default()
    };
    if part.is_file {
        let kind = part.tree.entries[&PathBuf::new()];
        let dest_kind = fs::symlink_metadata(&part.dest).ok().map(|m| kind_of(&m));
        plan.dest_files = u64::from(matches!(dest_kind, Some(Kind::File { .. })));
        plan_file(&mut plan, part, &PathBuf::new(), kind, dest_kind, compare);
        return plan;
    }
    match fs::symlink_metadata(&part.dest) {
        Err(_) => plan.dirs.push((part.dest.clone(), MakeDir::New)),
        Ok(m) if !m.is_dir() => {
            plan.conflicts.push(conflict(&part.dest));
            return plan;
        }
        Ok(_) => {}
    }
    // A file in one and a folder in the other is a type conflict: the target's
    // item and everything under it are left as they are, and nothing beneath the source's item
    // is mirrored.
    let mut conflicted: Vec<&Path> = Vec::new();
    for (rel, &kind) in &part.tree.entries {
        if conflicted.iter().any(|c| rel.starts_with(c)) {
            continue;
        }
        let dest_kind = part.dest_tree.entries.get(rel).copied();
        let swapped = matches!(
            (kind, dest_kind),
            (Kind::Dir, Some(Kind::File { .. })) | (Kind::File { .. }, Some(Kind::Dir))
        );
        if swapped {
            conflicted.push(rel);
            plan.conflicts.push(conflict(&part.dest.join(rel)));
            continue;
        }
        match kind {
            Kind::Link => {}
            Kind::Dir => match dest_kind {
                None => plan.dirs.push((part.dest.join(rel), MakeDir::New)),
                Some(Kind::Link) => plan.dirs.push((part.dest.join(rel), MakeDir::OverLink)),
                _ => {}
            },
            Kind::File { .. } => plan_file(&mut plan, part, rel, kind, dest_kind, compare),
        }
    }
    // Nothing is deleted under a source folder that couldn't be read.
    let unknown: BTreeSet<&PathBuf> = part.tree.unreadable.iter().map(|(p, _)| p).collect();
    // An extra is anything in the target that isn't in the source, whose folders above it are
    // folders in the source too or extras themselves. Under a source file or link (a type
    // conflict, or a link that's skipped), the target is left alone.
    for (rel, &kind) in &part.dest_tree.entries {
        if part.tree.entries.contains_key(rel) || unknown.iter().any(|u| rel.starts_with(u)) {
            continue;
        }
        let above_ok = rel.ancestors().skip(1).all(|a| {
            a.as_os_str().is_empty() || matches!(part.tree.entries.get(a), Some(Kind::Dir) | None)
        });
        if !above_ok {
            continue;
        }
        if kind == Kind::Dir && part.dest_tree.protects(rel) {
            continue;
        }
        if matches!(kind, Kind::File { .. }) {
            plan.delete_files += 1;
        }
        plan.deletes.push(Delete {
            path: part.dest.join(rel),
            kind,
        });
    }
    plan
}

fn conflict(path: &Path) -> MirrorFailure {
    MirrorFailure {
        path: path.to_path_buf(),
        message: tr("error-mirror-type-conflict"),
    }
}

fn plan_file(
    plan: &mut Plan,
    part: &Part<'_>,
    rel: &Path,
    kind: Kind,
    dest_kind: Option<Kind>,
    compare: Compare,
) {
    let from = if rel.as_os_str().is_empty() {
        part.source.to_path_buf()
    } else {
        part.source.join(rel)
    };
    let to = if rel.as_os_str().is_empty() {
        part.dest.clone()
    } else {
        part.dest.join(rel)
    };
    let replaces = match dest_kind {
        None => Replaces::Nothing,
        Some(Kind::File { .. }) => {
            if !needs_copy(&from, &to, kind, dest_kind, compare) {
                plan.unchanged += 1;
                return;
            }
            Replaces::File
        }
        Some(Kind::Link) => Replaces::Link,
        Some(Kind::Dir) => {
            plan.conflicts.push(conflict(&to));
            return;
        }
    };
    plan.copies.push(Copy {
        from,
        to,
        size: size(kind),
        replaces,
    });
}

fn size(kind: Kind) -> u64 {
    match kind {
        Kind::File { size, .. } => size,
        _ => 0,
    }
}

fn needs_copy(from: &Path, to: &Path, kind: Kind, dest: Option<Kind>, compare: Compare) -> bool {
    let (
        Kind::File { size, modified },
        Some(Kind::File {
            size: dsize,
            modified: dmod,
        }),
    ) = (kind, dest)
    else {
        return true;
    };
    if size != dsize {
        return true;
    }
    let same_time = match (modified, dmod) {
        (Some(a), Some(b)) => {
            let diff = a
                .duration_since(b)
                .or_else(|_| b.duration_since(a))
                .unwrap_or_default();
            diff <= TIME_TOLERANCE
        }
        _ => false,
    };
    if !same_time {
        return true;
    }
    compare == Compare::Content && !same_content(from, to).unwrap_or(false)
}

fn same_content(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

enum CopyError {
    Cancelled,
    Io(io::Error),
}

impl From<io::Error> for CopyError {
    fn from(e: io::Error) -> Self {
        CopyError::Io(e)
    }
}

/// What a copy worker tells the run.
enum Msg {
    Bytes(u64),
    Done(usize, Result<(), CopyError>),
}

/// Copies every file on up to [`MAX_WORKERS`] threads; progress and results come back to this
/// thread. Returns whether the run was cancelled.
fn copy_all(
    copies: &[Copy],
    outcome: &mut MirrorOutcome,
    progress: &mut MirrorProgress,
    on_progress: &mut dyn FnMut(&MirrorProgress),
    cancel: &CancelToken,
) -> bool {
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, MAX_WORKERS)
        .min(copies.len().max(1));
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut cancelled = false;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (tx, next) = (tx.clone(), &next);
            scope.spawn(move || {
                while !cancel.is_cancelled() {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(copy) = copies.get(i) else { break };
                    let report = |n| {
                        let _ = tx.send(Msg::Bytes(n));
                    };
                    let result = copy_file(copy, cancel, &report);
                    let _ = tx.send(Msg::Done(i, result));
                }
            });
        }
        drop(tx);
        for msg in rx {
            match msg {
                Msg::Bytes(n) => progress.bytes_done += n,
                Msg::Done(i, result) => {
                    let copy = &copies[i];
                    match result {
                        Ok(()) => {
                            outcome.copied += 1;
                            outcome.bytes_copied += copy.size;
                        }
                        Err(CopyError::Cancelled) => cancelled = true,
                        Err(CopyError::Io(e)) => outcome.failed.push(failure(&copy.from, &e)),
                    }
                    progress.files_done += 1;
                    progress.current = Some(copy.from.clone());
                }
            }
            on_progress(progress);
        }
    });
    cancelled || cancel.is_cancelled()
}

/// A new file beside `path`, created for this run alone (`.<name>.<unique>.<suffix>`): it can
/// never be an existing file or a hard link to one. Returns its path; the file is empty.
fn create_staging(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    loop {
        // create_new fails if anything is there already, so no separate check is needed.
        let candidate = unique_name(path, suffix);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// `.<name>.<unique>.<suffix>` beside `path`, unique to this process and call.
fn unique_name(path: &Path, suffix: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unique = format!(
        "{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    path.with_file_name(format!(".{name}.{unique}.{suffix}"))
}

/// Copies to a staging file of its own beside the target, then puts it in place of whatever
/// the target is: nothing, a file, a link (removed, never followed) or a folder (set aside,
/// replaced, then removed; put back if the replacing fails). `report` gets the bytes copied.
fn copy_file(copy: &Copy, cancel: &CancelToken, report: &dyn Fn(u64)) -> Result<(), CopyError> {
    let staging = create_staging(&copy.to, "replica-tmp")?;
    let staged = if copy.size < LARGE_FILE {
        // The platform's own copy (CopyFileEx on Windows), into the staging file just made: fast,
        // and it keeps the modified time and the read-only attribute.
        fs::copy(&copy.from, &staging)
            .map_err(CopyError::from)
            .and_then(|_| {
                keep_times(&copy.from, &staging)?;
                report(copy.size);
                Ok(())
            })
    } else {
        copy_chunked(&copy.from, &staging, cancel, report)
    };
    let placed =
        staged.and_then(|()| place(&staging, &copy.to, copy.replaces).map_err(CopyError::from));
    if placed.is_err() {
        let _ = remove_file(&staging);
    }
    placed
}

/// Moves a complete staging file into place.
fn place(staging: &Path, to: &Path, replaces: Replaces) -> io::Result<()> {
    match replaces {
        Replaces::Nothing => fs::rename(staging, to),
        Replaces::File => {
            if fs::symlink_metadata(to)?.permissions().readonly() {
                set_readonly(to, false)?;
            }
            fs::rename(staging, to)
        }
        Replaces::Link => {
            remove_link(to)?;
            fs::rename(staging, to)
        }
    }
}

/// Gives the copy the source's modified time, where the platform's copy didn't.
fn keep_times(from: &Path, copy: &Path) -> io::Result<()> {
    let modified = fs::metadata(from)?.modified()?;
    let meta = fs::metadata(copy)?;
    if meta.modified().ok() == Some(modified) {
        return Ok(());
    }
    let readonly = meta.permissions().readonly();
    if readonly {
        set_readonly(copy, false)?;
    }
    OpenOptions::new()
        .write(true)
        .open(copy)?
        .set_modified(modified)?;
    if readonly {
        set_readonly(copy, true)?;
    }
    Ok(())
}

/// A large file, a chunk at a time, so it reports progress and stops when cancelled.
fn copy_chunked(
    from: &Path,
    staging: &Path,
    cancel: &CancelToken,
    report: &dyn Fn(u64),
) -> Result<(), CopyError> {
    let mut src = File::open(from)?;
    let meta = src.metadata()?;
    // The staging file was just created for this copy: truncating it touches nothing else.
    let mut dst = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(staging)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(CopyError::Cancelled);
        }
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        report(n as u64);
    }
    if let Ok(modified) = meta.modified() {
        dst.set_modified(modified)?;
    }
    drop(dst);
    if meta.permissions().readonly() {
        set_readonly(staging, true)?;
    }
    Ok(())
}

fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut perms = fs::symlink_metadata(path)?.permissions();
    if perms.readonly() != readonly {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(readonly);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Removes a file, read-only or not.
fn remove_file(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.permissions().readonly() {
        set_readonly(path, false)?;
    }
    fs::remove_file(path)
}

/// Removes a link or junction itself, never what it points to.
fn remove_link(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_symlink() {
        return Err(io::Error::other("not a link"));
    }
    // A link to a folder (and a junction) is removed as a folder on Windows.
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(_) => fs::remove_dir(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_within_the_tolerance_are_the_same() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let file = |s, m| Kind::File {
            size: s,
            modified: Some(m),
        };
        let p = Path::new("x");
        let same = Compare::SizeAndTime;
        assert!(!needs_copy(
            p,
            p,
            file(5, t),
            Some(file(5, t + Duration::from_secs(2))),
            same
        ));
        assert!(needs_copy(
            p,
            p,
            file(5, t),
            Some(file(5, t + Duration::from_secs(3))),
            same
        ));
        assert!(needs_copy(p, p, file(5, t), Some(file(6, t)), same));
        assert!(needs_copy(p, p, file(5, t), None, same));
    }

    #[test]
    fn staging_files_are_new_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("a.txt");
        let first = create_staging(&to, "replica-tmp").unwrap();
        let second = create_staging(&to, "replica-tmp").unwrap();
        assert_ne!(first, second);
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(".a.txt.") && name.ends_with(".replica-tmp"),
            "{name}"
        );
    }

    #[test]
    fn errors_read_well() {
        let e = MirrorError::TooManyDeletions {
            would_delete: 40,
            of: 100,
            limit_percent: 30,
        };
        let text = e.to_string();
        assert!(
            text.contains("40") && text.contains("100") && text.contains("30"),
            "{text}"
        );
        let empty = MirrorError::SourceEmpty {
            path: "D:/Photos".into(),
        };
        assert!(empty.to_string().contains("D:/Photos"));
    }
}
