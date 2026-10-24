//! The Restore tab: pick a job, one of its targets and a snapshot; browse the snapshot's files
//! (each folder listed when it's opened, so a large snapshot isn't read whole); select files
//! and folders; restore them to a new folder, or, only after a confirmation, where they were.
//! Also "Test restore": a few random files restored and compared with their sources.
//!
//! restic runs off the UI thread, through [`Backend`], with the same core calls as
//! `replica restore`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use chrono::Local;
use eframe::egui::{self, Id, Modal, RichText, Ui};
use replica_core::cancel::CancelToken;
use replica_core::history::{HistoryFilter, TestRestoreRecord};
use replica_core::i18n::tr;
use replica_core::job::{Job, Mode, Target};
use replica_core::restic::{Node, PINNED_TAG, RestoreSummary, Snapshot, local_path, snapshot_path};
use replica_core::run::Outcome;
use replica_core::settings::Settings;
use replica_core::tr;

use super::backend::{Backend, RestoreJob, RestoreTo};
use super::history::outcome_tone;
use super::icons::Icon;
use super::jobs::outcome_text;
use super::theme::{self, space};
use super::widgets::{
    InfoKind, TableColumn, TableState, Tone, danger_button_filled, dialog_frame, dropdown,
    dropdown_choice, dropdown_value, dropdown_width, empty_state, error_text, flow, heading,
    info_bar, info_row, leaving_room, primary_button, quiet_text, relative_time, secondary_button,
    section_card, section_card_with_actions, size_text, status_chip, table, tag, tree_folder,
};

/// The job and backup pickers' widest, closed: a longer name is cut with "…".
const PICKER_WIDTH: f32 = 320.0;

/// Something being fetched off the UI thread.
pub enum Load<T> {
    Loading(Receiver<Result<T, String>>),
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    /// Takes the result if it has come.
    pub fn poll(&mut self) {
        if let Load::Loading(rx) = self {
            match rx.try_recv() {
                Ok(Ok(value)) => *self = Load::Ready(value),
                Ok(Err(e)) => *self = Load::Failed(e),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => *self = Load::Failed(tr("restore-stopped")),
            }
        }
    }
}

/// Runs `work` on a thread of its own; the window repaints when it's done.
pub fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    backend: &Arc<dyn Backend>,
    work: impl FnOnce(&dyn Backend) -> Result<T, String> + Send + 'static,
) -> Load<T> {
    let (tx, rx) = channel();
    let backend = Arc::clone(backend);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(work(backend.as_ref()));
        ctx.request_repaint();
    });
    Load::Loading(rx)
}

/// A restore or test restore under way.
enum Work {
    Restoring(Load<RestoreSummary>, CancelToken, RestoreTo),
    Testing(Load<TestRestoreRecord>, CancelToken),
}

/// Pinning or unpinning the chosen snapshot: its new ID once done.
struct Pinning {
    load: Load<String>,
    pinned: bool,
    /// The job and target (by name) it's for: its result is only applied to them.
    for_view: (String, String),
}

/// How the last restore or test restore ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Restored { summary: String, open: PathBuf },
    Tested(TestRestoreRecord),
    Failed(String),
}

pub struct RestoreTab {
    backend: Arc<dyn Backend>,
    jobs: Vec<Job>,
    jobs_read: Option<std::time::Instant>,
    pub job: Option<String>,
    pub target: usize,
    snapshots: Option<Load<Vec<Snapshot>>>,
    pub snapshot: Option<Snapshot>,
    folders: HashMap<String, Load<Vec<Node>>>,
    /// The chosen files and folders, by their paths in the snapshot.
    pub selected: BTreeSet<String>,
    pub to_original: bool,
    pub folder: String,
    confirm_original: bool,
    work: Option<Work>,
    pub done: Option<Done>,
    /// The job and target (by name) the snapshots are for: when either changes, they're
    /// fetched again, and an answer still coming for the old one is dropped with its load.
    snapshots_for: Option<(String, String)>,
    snapshot_table: TableState,
    /// The last work was a test restore: its result shows in the test's card.
    tested_last: bool,
    /// The target's last test restore in the history, for the job and target (by name) it
    /// was read for.
    last_test: Option<((String, String), Option<TestRestoreRecord>)>,
    /// A pin or unpin under way.
    pinning: Option<Pinning>,
    /// Unpinning the chosen snapshot asks first.
    confirm_unpin: bool,
    /// The snapshot to choose once the list is read again: a pinned or unpinned one has a new
    /// ID.
    choose_next: Option<String>,
    /// How the last pin or unpin went.
    pub pin_note: Option<Result<String, String>>,
}

impl RestoreTab {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            jobs: Vec::new(),
            jobs_read: None,
            job: None,
            target: 0,
            snapshots: None,
            snapshot: None,
            folders: HashMap::new(),
            selected: BTreeSet::new(),
            to_original: false,
            folder: String::new(),
            confirm_original: false,
            work: None,
            done: None,
            snapshots_for: None,
            snapshot_table: TableState::default(),
            tested_last: false,
            last_test: None,
            pinning: None,
            confirm_unpin: false,
            choose_next: None,
            pin_note: None,
        }
    }

    fn current(&self) -> Option<(&Job, &Target)> {
        let job = self
            .jobs
            .iter()
            .find(|j| Some(j.name.as_str()) == self.job.as_deref())?;
        Some((job, job.targets.get(self.target)?))
    }

    /// Whether a restore, test restore, pin or unpin is running: the page's choices and actions
    /// wait for it (a pin can wait its turn for the repository a long while, and changes the
    /// chosen snapshot's ID).
    pub fn busy(&self) -> bool {
        self.work.is_some() || self.pinning.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        if self
            .jobs_read
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(2))
        {
            self.jobs_read = Some(std::time::Instant::now());
            self.jobs = Settings::load_or_default(&self.backend.settings_file())
                .map(|l| l.settings.jobs)
                .unwrap_or_default();
        }
        self.poll(ui.ctx());
        if self.jobs.is_empty() {
            empty_state(
                ui,
                Icon::Restore,
                &tr("restore-no-jobs-title"),
                &tr("restore-no-jobs-body"),
                None,
            );
            return;
        }
        let busy = self.busy();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let ready = ui.add_enabled_ui(!busy, |ui| self.choose_card(ui)).inner;
                // As the pickers left it.
                let current = self.current().map(|(j, t)| (j.clone(), t.clone()));
                let Some((job, target)) = current.as_ref() else {
                    return;
                };
                if job.mode == Mode::Mirror {
                    return;
                }
                if ready && self.snapshot.is_some() {
                    ui.add_enabled_ui(!busy, |ui| self.browse_card(ui, job, target));
                    self.restore_card(ui, job, target);
                }
                self.test_card(ui, job, target);
            });
        if let Some((job, target)) = self.current().map(|(j, t)| (j.clone(), t.clone())) {
            self.confirm_window(ui.ctx(), &job, &target);
            self.confirm_unpin_window(ui.ctx(), &job, &target);
        }
    }

    fn fetch_snapshots(
        &self,
        ctx: &egui::Context,
        job: &Job,
        target: &Target,
    ) -> Load<Vec<Snapshot>> {
        let (job, target) = (job.clone(), target.clone());
        spawn(ctx, &self.backend, move |b| {
            let mut list = b.snapshots(&job, &target)?;
            list.sort_by_key(|s| std::cmp::Reverse(s.time));
            Ok(list)
        })
    }

    /// Picks up what the threads finished.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(s) = &mut self.snapshots {
            s.poll();
        }
        if let Some(p) = &mut self.pinning {
            p.load.poll();
            let pinned = p.pinned;
            match std::mem::replace(&mut p.load, Load::Failed(String::new())) {
                Load::Loading(rx) => p.load = Load::Loading(rx),
                Load::Ready(id) => {
                    let for_view = self.pinning.take().map(|p| p.for_view);
                    self.pin_note = Some(Ok(tr(if pinned {
                        "restore-pinned-done"
                    } else {
                        "restore-unpinned-done"
                    })));
                    // Read again, and the same snapshot chosen under its new ID, if the page
                    // still shows that job and target.
                    if for_view.is_some() && for_view == self.snapshots_for {
                        self.snapshots = None;
                        self.choose_next = Some(id);
                    }
                    ctx.request_repaint();
                }
                Load::Failed(e) => {
                    self.pinning = None;
                    self.pin_note = Some(Err(e));
                }
            }
        }
        for folder in self.folders.values_mut() {
            folder.poll();
        }
        let finished = match &mut self.work {
            Some(Work::Restoring(load, _, to)) => {
                load.poll();
                match load {
                    Load::Loading(_) => None,
                    Load::Ready(summary) => Some(Done::Restored {
                        summary: tr!(
                            "restore-done",
                            files = summary.files_restored,
                            size = size_text(summary.bytes_restored)
                        ),
                        open: match to {
                            RestoreTo::Folder(f) => f.clone(),
                            RestoreTo::Original => self
                                .selected
                                .iter()
                                .next()
                                .map(|i| {
                                    let p = local_path(i);
                                    p.parent().map(Path::to_path_buf).unwrap_or(p)
                                })
                                .unwrap_or_default(),
                        },
                    }),
                    Load::Failed(e) => Some(Done::Failed(e.clone())),
                }
            }
            Some(Work::Testing(load, _)) => {
                load.poll();
                match load {
                    Load::Loading(_) => None,
                    Load::Ready(record) => {
                        // It's in the history now: read the last result again.
                        self.last_test = None;
                        Some(Done::Tested(record.clone()))
                    }
                    Load::Failed(e) => Some(Done::Failed(e.clone())),
                }
            }
            None => None,
        };
        if let Some(done) = finished {
            self.work = None;
            self.done = Some(done);
            ctx.request_repaint();
        }
    }

    fn reset_snapshots(&mut self) {
        self.snapshots = None;
        self.snapshot = None;
        self.reset_browse();
    }

    fn reset_browse(&mut self) {
        self.folders.clear();
        self.selected.clear();
        self.done = None;
    }

    /// Step 1: the job, its target and a snapshot. True once the snapshots are listed.
    fn choose_card(&mut self, ui: &mut Ui) -> bool {
        let mut again = false;
        let versioned = self
            .current()
            .is_some_and(|(j, _)| j.mode == Mode::Versioned);
        section_card_with_actions(
            ui,
            &tr("restore-step-choose"),
            Some(&tr("restore-step-choose-help")),
            |ui| {
                if versioned {
                    again = secondary_button(ui, &tr("history-refresh")).clicked();
                }
            },
            |ui| {
                self.pickers(ui);
                // What's chosen now, after the pickers: never the job or target before.
                let Some((job, target)) = self.current().map(|(j, t)| (j.clone(), t.clone()))
                else {
                    return false;
                };
                ui.add_space(space::S);
                if job.mode == Mode::Mirror {
                    info_bar(ui, InfoKind::Info, None, &tr("restore-mirror"));
                    return false;
                }
                self.snapshots_ui(ui, &job, &target)
            },
        );
        if again {
            self.reset_snapshots();
        }
        self.snapshots
            .as_ref()
            .is_some_and(|s| matches!(s, Load::Ready(_)))
    }

    /// The job and, if it has more than one, the target.
    fn pickers(&mut self, ui: &mut Ui) {
        let jobs: Vec<Job> = self.jobs.clone();
        ui.horizontal_wrapped(|ui| {
            let l = ui.label(tr("restore-job"));
            let before = (self.job.clone(), self.target);
            let value = self.job.clone().unwrap_or_else(|| tr("restore-pick-job"));
            dropdown(ui, "restore-job", None, &value, PICKER_WIDTH, |ui| {
                for job in &jobs {
                    if dropdown_choice(ui, self.job.as_deref() == Some(&job.name), &job.name)
                        .clicked()
                    {
                        self.job = Some(job.name.clone());
                        self.target = 0;
                    }
                }
            })
            .labelled_by(l.id);
            if let Some((job, _)) = self.current().map(|(j, t)| (j.clone(), t.clone()))
                && job.targets.len() > 1
            {
                let name = tr("restore-target");
                let value = job.targets[self.target].display_name();
                // The label and its picker together, on the next row if they don't fit here.
                let font = egui::TextStyle::Body.resolve(ui.style());
                let gap = ui.spacing().item_spacing.x;
                let needed = space::S
                    + ui.painter()
                        .layout_no_wrap(name.clone(), font, theme::of(ui).text)
                        .size()
                        .x
                    + gap
                    + dropdown_width(ui, None, &value, PICKER_WIDTH);
                // What's left of this row (a wrapping row's available width is all of it).
                let left = ui.max_rect().right() - ui.cursor().min.x;
                if needed > left {
                    ui.end_row();
                } else {
                    ui.add_space(space::S);
                }
                let l = ui.label(name);
                dropdown(ui, "restore-target", None, &value, PICKER_WIDTH, |ui| {
                    for (i, t) in job.targets.iter().enumerate() {
                        dropdown_value(ui, &mut self.target, i, t.display_name());
                    }
                })
                .labelled_by(l.id);
            }
            if (self.job.clone(), self.target) != before {
                self.reset_snapshots();
            }
        });
    }

    /// The snapshots: fetched off the UI thread, then a table, newest first. True once listed.
    fn snapshots_ui(&mut self, ui: &mut Ui, job: &Job, target: &Target) -> bool {
        let key = (job.name.clone(), target.display_name());
        if self.snapshots_for.as_ref() != Some(&key) {
            self.reset_snapshots();
            self.snapshots_for = Some(key);
        }
        let snapshots = match self.snapshots.as_ref() {
            None => {
                self.snapshots = Some(self.fetch_snapshots(ui.ctx(), job, target));
                return false;
            }
            Some(Load::Loading(_)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("restore-loading-snapshots"));
                });
                return false;
            }
            Some(Load::Failed(e)) => {
                info_bar(ui, InfoKind::Error, None, e);
                return false;
            }
            Some(Load::Ready(list)) => list.clone(),
        };
        if snapshots.is_empty() {
            empty_state(
                ui,
                Icon::Empty,
                &tr("restore-no-snapshots-title"),
                &tr("restore-no-snapshots"),
                None,
            );
            return true;
        }
        if let Some(id) = self.choose_next.take() {
            self.snapshot = snapshots.iter().find(|s| s.id == id).cloned();
        }
        self.snapshot_table.selected = self
            .snapshot
            .as_ref()
            .and_then(|c| snapshots.iter().position(|s| s.id == c.id));
        let columns = [
            TableColumn::new(tr("restore-col-date"), Some(160.0)),
            TableColumn::new(tr("restore-col-pinned"), Some(90.0)),
            TableColumn::new(tr("restore-col-host"), Some(140.0)),
            TableColumn::new(tr("restore-col-tags"), Some(160.0)),
            TableColumn::new(tr("restore-col-size"), Some(100.0)),
            TableColumn::new(tr("restore-col-id"), Some(90.0)),
        ];
        let response = table(
            ui,
            &tr("restore-snapshot"),
            &columns,
            snapshots.len(),
            Some(220.0),
            &mut self.snapshot_table,
            |i| snapshot_line(&snapshots[i]),
            |_| {},
            |ui, row, col| {
                let s = &snapshots[row];
                if col == 1 {
                    if s.is_pinned() {
                        tag(ui, &tr("restore-pinned")).on_hover_text(tr("restore-pinned-hint"));
                    }
                    return;
                }
                let text = match col {
                    0 => snapshot_time(s),
                    2 => s.hostname.clone(),
                    3 => shown_tags(s),
                    4 => snapshot_size(s),
                    _ => s.short_id.clone(),
                };
                quiet_text(ui, &text, None);
            },
        );
        self.pin_row(ui, job, target);
        if response.selection_changed
            && let Some(s) = self.snapshot_table.selected.and_then(|i| snapshots.get(i))
            && self.snapshot.as_ref().is_none_or(|c| c.id != s.id)
        {
            self.snapshot = Some(s.clone());
            self.reset_browse();
            if let Some(job) = &self.job {
                self.folder = default_folder(job, s).to_string_lossy().into_owned();
            }
        }
        true
    }

    /// Step 2: the snapshot's files, as a tree, each folder listed when it's opened.
    fn browse_card(&mut self, ui: &mut Ui, job: &Job, target: &Target) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let p = theme::of(ui);
        let path = tr!(
            "restore-breadcrumb",
            job = job.name.as_str(),
            when = snapshot_time(&snapshot),
            host = snapshot.hostname.as_str()
        );
        let chosen = tr!("restore-chosen", count = self.selected.len());
        section_card(ui, &tr("restore-step-files"), None, |ui| {
            // Where in the backups, on one line: a long job's name is cut with "…".
            ui.add(egui::Label::new(RichText::new(path).color(p.text_secondary)).truncate());
            ui.label(RichText::new(chosen).small().color(p.text_secondary));
            ui.add_space(space::XS);
            egui::Frame::new()
                .fill(p.control)
                .stroke(egui::Stroke::new(1.0, p.border))
                .corner_radius(theme::CONTROL_RADIUS)
                .inner_margin(space::S)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("restore-tree")
                        .max_height(340.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for root in &snapshot.paths {
                                let path = snapshot_path(Path::new(root));
                                self.folder_ui(ui, target, &snapshot.id, &path, root);
                            }
                        });
                });
        });
    }

    /// A folder: its row in the tree (a box to choose it whole), and, opened, what's in it,
    /// listed then.
    fn folder_ui(&mut self, ui: &mut Ui, target: &Target, snapshot: &str, path: &str, name: &str) {
        let mut chosen = self.selected.contains(path);
        let id = ui.make_persistent_id(("folder", snapshot, path));
        let check_name = tr!("restore-whole-folder", name = name);
        let changed = tree_folder(ui, id, name, &mut chosen, &check_name, |ui| {
            if !self.folders.contains_key(path) {
                let (target, snapshot, folder) =
                    (target.clone(), snapshot.to_owned(), path.to_owned());
                let load = spawn(ui.ctx(), &self.backend, move |b| {
                    let mut nodes = b.list(&target, &snapshot, &folder)?;
                    nodes.sort_by(|a, b| {
                        (a.kind != "dir", a.name.to_lowercase())
                            .cmp(&(b.kind != "dir", b.name.to_lowercase()))
                    });
                    Ok(nodes)
                });
                self.folders.insert(path.to_owned(), load);
            }
            let nodes = match self.folders.get(path) {
                Some(Load::Ready(nodes)) => nodes.clone(),
                Some(Load::Failed(e)) => {
                    error_text(ui, e);
                    return;
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("restore-loading-folder"));
                    });
                    return;
                }
            };
            if nodes.is_empty() {
                ui.label(
                    RichText::new(tr("restore-empty-folder")).color(theme::of(ui).text_secondary),
                );
            }
            for node in &nodes {
                if node.kind == "dir" {
                    self.folder_ui(ui, target, snapshot, &node.path, &node.name);
                } else {
                    self.file_row(ui, node);
                }
            }
        });
        if changed {
            toggle(&mut self.selected, path, chosen);
        }
    }

    /// A file in the tree: a box to choose it, its icon, its name and size.
    fn file_row(&mut self, ui: &mut Ui, node: &Node) {
        let p = theme::of(ui);
        let mut chosen = self.selected.contains(&node.path);
        ui.horizontal(|ui| {
            ui.set_min_height(28.0);
            // Where a folder's arrow is, so files line up with folders.
            ui.add_space(20.0 + ui.spacing().item_spacing.x);
            let name = tr!(
                "restore-file-line",
                name = node.name.as_str(),
                size = size_text(node.size)
            );
            if ui.checkbox(&mut chosen, "").accessible_name(name).changed() {
                toggle(&mut self.selected, &node.path, chosen);
            }
            let (icon, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            Icon::File.paint(ui.painter(), icon, p.text_secondary);
            // A long name is cut, its size kept after it.
            let size = size_text(node.size);
            let font = egui::TextStyle::Body.resolve(ui.style());
            let size_width = ui
                .painter()
                .layout_no_wrap(size.clone(), font, p.text_secondary)
                .size()
                .x;
            leaving_room(ui, size_width, |ui| quiet_text(ui, &node.name, None));
            quiet_text(ui, &size, Some(p.text_secondary));
        });
    }

    /// Step 3: where to, then Restore, its progress and how it ended.
    fn restore_card(&mut self, ui: &mut Ui, job: &Job, target: &Target) {
        let p = theme::of(ui);
        let busy = self.busy();
        section_card(ui, &tr("restore-step-to"), None, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.radio_value(&mut self.to_original, false, tr("restore-to-folder"));
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
                    let l = ui.label(tr("restore-folder"));
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    // The row's width, less Browse: a long path shows whole on hover.
                    let width = (ui.available_width() - 110.0).max(160.0);
                    let hover = self.folder.clone();
                    ui.add_enabled(
                        !self.to_original,
                        egui::TextEdit::singleline(&mut self.folder)
                            .font(font)
                            .desired_width(width),
                    )
                    .labelled_by(l.id)
                    .on_hover_text(hover);
                    if ui
                        .add_enabled_ui(!self.to_original, |ui| {
                            secondary_button(ui, &tr("button-browse"))
                        })
                        .inner
                        .clicked()
                        && let Some(folder) = self.backend.pick_folder()
                    {
                        self.folder = folder.to_string_lossy().into_owned();
                    }
                });
                ui.add_space(space::S);
                // The one that replaces files: in red, and it asks first.
                ui.radio_value(
                    &mut self.to_original,
                    true,
                    RichText::new(tr("restore-to-original")).color(p.failed),
                );
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
                    ui.label(
                        RichText::new(tr("restore-to-original-help"))
                            .small()
                            .color(p.text_secondary),
                    );
                });
            });
            ui.add_space(space::M);
            if let Some(Work::Restoring(_, cancel, _)) = &self.work {
                let cancel = cancel.clone();
                ui.horizontal(|ui| {
                    ui.spinner();
                    // Waiting its turn for the repository, if another job is using it.
                    let doing = self
                        .backend
                        .waiting()
                        .unwrap_or_else(|| tr("restore-running"));
                    ui.label(doing);
                    if secondary_button(ui, &tr("button-cancel")).clicked() {
                        cancel.cancel();
                    }
                });
                return;
            }
            let count = self.selected.len();
            let ready = !busy && count > 0 && (self.to_original || !self.folder.trim().is_empty());
            let start = ui
                .add_enabled_ui(ready, |ui| {
                    primary_button(ui, &tr!("restore-start", count = count))
                })
                .inner
                .clicked();
            if start {
                if self.to_original {
                    self.confirm_original = true;
                } else {
                    let to = RestoreTo::Folder(PathBuf::from(self.folder.trim()));
                    self.start(ui.ctx(), job, target, to);
                }
            }
            if !self.tested_last {
                self.done_ui(ui);
            }
        });
    }

    /// The test restore: what it does, the last result, and Run.
    fn test_card(&mut self, ui: &mut Ui, job: &Job, target: &Target) {
        let key = (job.name.clone(), target.display_name());
        if self.last_test.as_ref().is_none_or(|(k, _)| *k != key) {
            let last = last_test_of(self.backend.as_ref(), &job.name, &key.1);
            self.last_test = Some((key, last));
        }
        let last = self.last_test.as_ref().and_then(|(_, t)| t.clone());
        let busy = self.busy();
        section_card(
            ui,
            &tr("restore-test-title"),
            Some(&tr("restore-test-hint")),
            |ui| {
                match &last {
                    Some(t) => {
                        let when = t.started_at.with_timezone(&Local).naive_local();
                        let now = Local::now().naive_local();
                        info_row(
                            ui,
                            &tr("restore-test-last"),
                            Some(&relative_time(when, now)),
                            |ui| status_chip(ui, outcome_tone(t.outcome), &outcome_text(t.outcome)),
                        );
                    }
                    None => {
                        info_row(ui, &tr("restore-test-last"), None, |ui| {
                            status_chip(ui, Tone::Neutral, &tr("restore-test-never"))
                        });
                    }
                }
                ui.add_space(space::S);
                if let Some(Work::Testing(_, cancel)) = &self.work {
                    let cancel = cancel.clone();
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let doing = self
                            .backend
                            .waiting()
                            .unwrap_or_else(|| tr("restore-test-running"));
                        ui.label(doing);
                        if secondary_button(ui, &tr("button-cancel")).clicked() {
                            cancel.cancel();
                        }
                    });
                    return;
                }
                let run = ui
                    .add_enabled_ui(!busy, |ui| secondary_button(ui, &tr("restore-test")))
                    .inner
                    .clicked();
                if run {
                    let (job, target) = (job.clone(), target.clone());
                    let cancel = CancelToken::new();
                    let token = cancel.clone();
                    self.done = None;
                    self.tested_last = true;
                    let load = spawn(ui.ctx(), &self.backend, move |b| {
                        b.test_restore(&job, &target, &token)
                    });
                    self.work = Some(Work::Testing(load, cancel));
                }
                if self.tested_last {
                    self.done_ui(ui);
                }
            },
        );
    }

    fn done_ui(&mut self, ui: &mut Ui) {
        let Some(done) = self.done.clone() else {
            return;
        };
        ui.add_space(space::S);
        match done {
            Done::Restored { summary, open } => {
                info_bar(ui, InfoKind::Info, None, &summary);
                ui.add_space(space::XS);
                if secondary_button(ui, &tr("restore-open-folder")).clicked()
                    && let Err(e) = self.backend.open_path(&open)
                {
                    self.done = Some(Done::Failed(e));
                }
            }
            Done::Tested(record) => {
                let text = match record.outcome {
                    Outcome::Ok => tr!("test-restore-same", checked = record.checked),
                    Outcome::Skipped => tr("test-restore-nothing-compared"),
                    _ => tr!("test-restore-differs", differing = record.differing),
                };
                if record.outcome == Outcome::Failed {
                    info_bar(ui, InfoKind::Error, None, &text);
                    for line in record.detail.as_deref().unwrap_or("").lines() {
                        error_text(ui, line);
                    }
                } else {
                    info_bar(ui, InfoKind::Info, None, &text);
                }
            }
            Done::Failed(e) => {
                info_bar(ui, InfoKind::Error, None, &e);
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, job: &Job, target: &Target, to: RestoreTo) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let restore = RestoreJob {
            snapshot: snapshot.id.clone(),
            items: self.selected.iter().cloned().collect(),
            to: to.clone(),
        };
        let (job, target) = (job.clone(), target.clone());
        let cancel = CancelToken::new();
        let token = cancel.clone();
        self.done = None;
        self.tested_last = false;
        let load = spawn(ctx, &self.backend, move |b| {
            b.restore(&job, &target, &restore, &token)
        });
        self.work = Some(Work::Restoring(load, cancel, to));
    }

    /// Pin or Unpin for the chosen snapshot, and how the last one went.
    fn pin_row(&mut self, ui: &mut Ui, job: &Job, target: &Target) {
        let Some(chosen) = self.snapshot.clone() else {
            return;
        };
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            if self.pinning.is_some() {
                ui.spinner();
                ui.label(tr(if chosen.is_pinned() {
                    "restore-unpinning"
                } else {
                    "restore-pinning"
                }));
                return;
            }
            let busy = self.busy();
            if chosen.is_pinned() {
                let unpin = ui
                    .add_enabled_ui(!busy, |ui| secondary_button(ui, &tr("restore-unpin")))
                    .inner;
                if unpin.clicked() {
                    self.confirm_unpin = true;
                }
            } else {
                let pin = ui
                    .add_enabled_ui(!busy, |ui| secondary_button(ui, &tr("restore-pin")))
                    .inner
                    .on_hover_text(tr("restore-pin-hint"));
                if pin.clicked() {
                    self.start_pin(ui.ctx(), job, target, true);
                }
            }
            match &self.pin_note {
                Some(Ok(done)) => {
                    ui.label(
                        RichText::new(done)
                            .small()
                            .color(theme::of(ui).text_secondary),
                    );
                }
                Some(Err(e)) => error_text(ui, e),
                None => {}
            }
        });
    }

    fn start_pin(&mut self, ctx: &egui::Context, job: &Job, target: &Target, pinned: bool) {
        let Some(id) = self.snapshot.as_ref().map(|s| s.id.clone()) else {
            return;
        };
        self.pin_note = None;
        let (job, target) = (job.clone(), target.clone());
        let for_view = (job.name.clone(), target.display_name());
        let load = spawn(ctx, &self.backend, move |b| {
            b.set_pinned(&job, &target, &id, pinned)
        });
        self.pinning = Some(Pinning {
            load,
            pinned,
            for_view,
        });
    }

    /// Unpinning needs this "yes": the job's retention can then delete the backup.
    fn confirm_unpin_window(&mut self, ctx: &egui::Context, job: &Job, target: &Target) {
        if !self.confirm_unpin {
            return;
        }
        let mut answer = None;
        Modal::new(Id::unique("restore-unpin"))
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                heading(ui, &tr("restore-unpin-title"));
                ui.add_space(space::S);
                ui.label(flow(&tr("restore-unpin-body")));
                ui.add_space(space::L);
                ui.horizontal(|ui| {
                    if danger_button_filled(ui, &tr("restore-unpin-confirm")).clicked() {
                        answer = Some(true);
                    }
                    if secondary_button(ui, &tr("button-cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                self.confirm_unpin = false;
                self.start_pin(ctx, job, target, false);
            }
            Some(false) => self.confirm_unpin = false,
            None => {}
        }
    }

    /// Restoring over the originals needs this "yes".
    fn confirm_window(&mut self, ctx: &egui::Context, job: &Job, target: &Target) {
        if !self.confirm_original {
            return;
        }
        let mut answer = None;
        Modal::new(Id::unique("restore-over-original"))
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                heading(ui, &tr("restore-original-title"));
                ui.add_space(space::S);
                ui.label(flow(&tr!(
                    "restore-original-body",
                    count = self.selected.len()
                )));
                for item in self.selected.iter().take(10) {
                    ui.label(local_path(item).to_string_lossy());
                }
                ui.add_space(space::L);
                ui.horizontal(|ui| {
                    if danger_button_filled(ui, &tr("restore-original-confirm")).clicked() {
                        answer = Some(true);
                    }
                    if secondary_button(ui, &tr("button-cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                self.confirm_original = false;
                self.start(ctx, job, target, RestoreTo::Original);
            }
            Some(false) => self.confirm_original = false,
            None => {}
        }
    }
}

fn toggle(selected: &mut BTreeSet<String>, path: &str, on: bool) {
    if on {
        selected.insert(path.to_owned());
    } else {
        selected.remove(path);
    }
}

/// The newest test restore of `job` on the target named `target` (as the history names it,
/// `Target::display_name`). The history filters by job only, so it reads more of the job's
/// until it finds one of the target's, or there are no more.
pub fn last_test_of(backend: &dyn Backend, job: &str, target: &str) -> Option<TestRestoreRecord> {
    let filter = HistoryFilter {
        job: Some(job.to_owned()),
        outcome: None,
    };
    for limit in [50, 1_000, 100_000] {
        let tests = backend.test_restores(&filter, limit).ok()?;
        if let Some(found) = tests.iter().find(|t| t.target == target) {
            return Some(found.clone());
        }
        if tests.len() < limit {
            return None;
        }
    }
    None
}

/// A snapshot's time, in the user's time zone.
fn snapshot_time(s: &Snapshot) -> String {
    s.time
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// The size of what it backed up, if restic said.
fn snapshot_size(s: &Snapshot) -> String {
    s.summary
        .as_ref()
        .map(|x| size_text(x.total_bytes_processed))
        .unwrap_or_default()
}

/// Its tags as the Tags column shows them: the pin has a column of its own.
fn shown_tags(s: &Snapshot) -> String {
    s.tags
        .iter()
        .filter(|t| *t != PINNED_TAG)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A snapshot as one line: a table row's name for a screen reader.
fn snapshot_line(s: &Snapshot) -> String {
    let line = tr!(
        "restore-snapshot-line",
        when = snapshot_time(s),
        host = s.hostname.as_str(),
        tags = shown_tags(s),
        size = snapshot_size(s),
        id = s.short_id.as_str()
    );
    if s.is_pinned() {
        format!("{line}, {}", tr("restore-pinned"))
    } else {
        line
    }
}

/// Where a restore goes unless the user picks somewhere: a new folder under their profile,
/// named after the job and the snapshot's time.
pub fn default_folder(job: &str, snapshot: &Snapshot) -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(tr("restore-default-folder")).join(format!(
        "{job} {}",
        snapshot.time.with_timezone(&Local).format("%Y-%m-%d %H%M")
    ))
}
