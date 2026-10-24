//! The Jobs page's Export… and Import… dialogs, over core's jobs files: this module only shows
//! what [`jobs_file`] decides and saves what its plan gives.
//!
//! - **Export:** every job with a check box (a filter, Select all and Select none above, how
//!   many are chosen), then a Save dialog. Passwords are never in the file.
//! - **Import:** a file, then its plan as a preview: each job with a check box and its status
//!   as badges (New, Name exists, Needs a password, Drive not found, Invalid with the reasons,
//!   which can't be chosen), and for a taken name Skip, Import as a copy or Replace. Imported
//!   jobs arrive turned off, by design.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, Ui};
use replica_core::i18n::tr;
use replica_core::job::{Job, Mode, VolumeRef};
use replica_core::jobs_file::{self, Action, Choice, JobsFile, OnConflict, Plan, Planned, Status};
use replica_core::platform::{Credential, PlatformError, ShareStatus, TargetResolver};
use replica_core::settings::Settings;
use replica_core::tr;
use replica_win::files::rename_no_clobber;
use replica_win::targets::{VolumeInfo, match_volume};

use super::{dialog, mode_text, targets_text};
use crate::ui::backend::{Backend, change_settings};
use crate::ui::theme::{self, space};
use crate::ui::widgets::{
    InfoKind, Tone, dropdown, dropdown_value, flow, heading, leaving_room, message_bar,
    primary_button, secondary_button, status_chip, status_chip_width, tag, tag_width,
};

/// How much of the window's height a dialog's list leaves for the rest of the dialog, and the
/// least and most it takes before it scrolls.
const LIST_ROOM_AROUND: f32 = 380.0;
const LIST_HEIGHT: (f32, f32) = (160.0, 560.0);
/// How wide the dialogs are.
const DIALOG_WIDTH: f32 = 600.0;

/// What a dialog did this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closed {
    /// Still open.
    Open,
    Cancelled,
    /// Done: what to say on the page, and the job to select in the table.
    Done {
        message: String,
        select: Option<String>,
    },
}

// --- Export ------------------------------------------------------------------------------------

pub struct ExportDialog {
    jobs: Vec<Job>,
    ticked: Vec<bool>,
    filter: String,
    error: Option<String>,
}

impl ExportDialog {
    /// Every job, those `ticked` says chosen.
    pub fn new(jobs: &[Job], ticked: impl Fn(&Job) -> bool) -> Self {
        Self {
            ticked: jobs.iter().map(ticked).collect(),
            jobs: jobs.to_vec(),
            filter: String::new(),
            error: None,
        }
    }

    /// Whether a job shows with the filter: by its name or its targets, ignoring case.
    fn shown(&self, i: usize) -> bool {
        let filter = self.filter.trim().to_lowercase();
        filter.is_empty()
            || self.jobs[i].name.to_lowercase().contains(&filter)
            || targets_text(&self.jobs[i]).to_lowercase().contains(&filter)
    }

    fn chosen(&self) -> Vec<Job> {
        self.jobs
            .iter()
            .zip(&self.ticked)
            .filter(|(_, t)| **t)
            .map(|(j, _)| j.clone())
            .collect()
    }

    pub fn ui(&mut self, ctx: &egui::Context, backend: &dyn Backend) -> Closed {
        let mut closed = Closed::Open;
        let modal = dialog(ctx, "export-jobs").show(ctx, |ui| {
            ui.set_width(DIALOG_WIDTH);
            heading(ui, &tr("export-title"));
            ui.add_space(space::S);
            ui.label(flow(&tr("export-intro")));
            ui.add_space(space::M);
            self.choosing_bar(ui);
            ui.add_space(space::S);
            self.list(ui);
            ui.add_space(space::S);
            let p = theme::of(ui);
            ui.label(RichText::new(tr("export-no-passwords")).color(p.text_secondary));
            if let Some(error) = &self.error {
                ui.add_space(space::S);
                message_bar(ui, InfoKind::Error, &flow(error));
            }
            ui.add_space(space::L);
            let count = self.ticked.iter().filter(|t| **t).count();
            ui.horizontal(|ui| {
                let export = ui
                    .add_enabled_ui(count > 0, |ui| {
                        primary_button(ui, &tr!("export-confirm", count = count))
                    })
                    .inner;
                if export.clicked() {
                    closed = self.export(backend);
                }
                if secondary_button(ui, &tr("button-cancel")).clicked() {
                    closed = Closed::Cancelled;
                }
            });
        });
        if modal.should_close() && closed == Closed::Open {
            closed = Closed::Cancelled;
        }
        closed
    }

    /// The filter, Select all and Select none (for the jobs the filter shows), and the count.
    fn choosing_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr("export-filter-hint"))
                    .desired_width(220.0)
                    .min_size(egui::vec2(0.0, theme::CONTROL_HEIGHT))
                    .align(egui::Align2::LEFT_CENTER),
            )
            .accessible_name(tr("export-filter"));
            for (key, on) in [("export-select-all", true), ("export-select-none", false)] {
                if secondary_button(ui, &tr(key)).clicked() {
                    for i in 0..self.jobs.len() {
                        if self.shown(i) {
                            self.ticked[i] = on;
                        }
                    }
                }
            }
            let count = self.ticked.iter().filter(|t| **t).count();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(tr!(
                        "export-count",
                        selected = count,
                        total = self.jobs.len()
                    ))
                    .color(theme::of(ui).text_secondary),
                );
            });
        });
    }

    fn list(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
        list_frame(ui, |ui| {
            let shown: Vec<usize> = (0..self.jobs.len()).filter(|&i| self.shown(i)).collect();
            if shown.is_empty() {
                ui.label(
                    RichText::new(tr!("export-none-match", filter = self.filter.trim()))
                        .color(p.text_secondary),
                );
            }
            for i in shown {
                let job = &self.jobs[i];
                let forever = job.mode == Mode::Versioned && job.retention.keep_forever;
                let mode = mode_text(job.mode, forever);
                let targets = targets_text(job);
                ui.horizontal(|ui| {
                    ui.set_min_height(28.0);
                    let name = tr!(
                        "export-job-for",
                        job = job.name.as_str(),
                        mode = mode.as_str(),
                        targets = targets.as_str()
                    );
                    let mut on = self.ticked[i];
                    let check = ui.checkbox(&mut on, "").accessible_name(name);
                    // The name toggles it too; the mode's tag and the targets after it.
                    let targets_room = (ui.available_width() * 0.4).min(220.0);
                    let label = leaving_room(ui, tag_width(ui, &mode) + targets_room, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&job.name).color(p.text))
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                    });
                    tag(ui, &mode);
                    ui.add(
                        egui::Label::new(RichText::new(&targets).color(p.text_secondary))
                            .truncate(),
                    );
                    if check.changed() || label.clicked() {
                        self.ticked[i] = if check.changed() { on } else { !on };
                    }
                });
            }
        });
    }

    /// Asks where, and writes the file: replacing one only once the Save dialog has asked.
    fn export(&mut self, backend: &dyn Backend) -> Closed {
        let Some(path) = backend.save_jobs_file() else {
            return Closed::Open;
        };
        let jobs = self.chosen();
        match jobs_file::write(&path, &jobs, true, rename_no_clobber) {
            Ok(()) => {
                tracing::info!(file = %path.display(), jobs = jobs.len(), "exported jobs");
                Closed::Done {
                    message: tr!(
                        "export-done",
                        count = jobs.len(),
                        path = path.display().to_string()
                    ),
                    select: None,
                }
            }
            Err(e) => {
                self.error = Some(e.to_string());
                Closed::Open
            }
        }
    }
}

// --- Import ------------------------------------------------------------------------------------

pub struct ImportDialog {
    path: PathBuf,
    file: JobsFile,
    choices: Vec<Choice>,
    plan: Plan,
    /// The volumes connected when it opened, for "Drive not found".
    volumes: Vec<VolumeInfo>,
    error: Option<String>,
}

/// Where volumes are, from a list of them: the import plan only asks that.
struct Volumes<'a>(&'a [VolumeInfo]);

impl TargetResolver for Volumes<'_> {
    fn locate_volume(&self, volume: &VolumeRef) -> Result<Option<PathBuf>, PlatformError> {
        match_volume(self.0, volume)
    }

    fn connect_share(&self, _: &str, _: Option<&Credential>) -> Result<ShareStatus, PlatformError> {
        // A preview never connects anything.
        Ok(ShareStatus::NotAvailable(String::new()))
    }
}

impl ImportDialog {
    /// The file's plan against the settings, every job chosen that can be. A file that isn't a
    /// jobs file, or is from a newer Replica, is the reason it's refused.
    pub fn open(backend: &dyn Backend, path: &Path) -> Result<Self, String> {
        let file = jobs_file::read(path).map_err(|e| {
            tr!(
                "import-refused",
                path = path.display().to_string(),
                reason = e.to_string()
            )
        })?;
        let mut dialog = Self {
            path: path.to_path_buf(),
            choices: vec![Choice::default(); file.entries.len()],
            plan: Plan {
                jobs: Vec::new(),
                settings: Settings::default(),
                warnings: Vec::new(),
            },
            file,
            volumes: backend.volumes().unwrap_or_default(),
            error: None,
        };
        dialog.replan(backend);
        for (choice, planned) in dialog.choices.iter_mut().zip(&dialog.plan.jobs) {
            if locked(planned) {
                choice.import = false;
            }
        }
        dialog.replan(backend);
        Ok(dialog)
    }

    /// The plan for the choices, against the settings as they are on disk now.
    fn replan(&mut self, backend: &dyn Backend) {
        let settings = match Settings::load_or_default(&backend.settings_file()) {
            Ok(loaded) => loaded.settings,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        self.plan = self.plan_on(backend, &settings);
    }

    fn plan_on(&self, backend: &dyn Backend, settings: &Settings) -> Plan {
        jobs_file::plan(
            &self.file,
            settings,
            &self.choices,
            backend.secrets(),
            &Volumes(&self.volumes),
        )
    }

    /// How many jobs importing adds or replaces.
    fn count(&self) -> usize {
        self.plan
            .jobs
            .iter()
            .filter(|j| matches!(j.action, Action::Add(_) | Action::Replace(_)))
            .count()
    }

    pub fn ui(&mut self, ctx: &egui::Context, backend: &dyn Backend, editable: bool) -> Closed {
        let mut closed = Closed::Open;
        let mut changed = false;
        let modal = dialog(ctx, "import-jobs").show(ctx, |ui| {
            ui.set_width(DIALOG_WIDTH);
            let p = theme::of(ui);
            heading(ui, &tr("import-title"));
            ui.add_space(space::S);
            ui.label(flow(&tr!(
                "import-intro",
                path = self.path.display().to_string(),
                count = self.file.entries.len()
            )));
            for warning in &self.plan.warnings {
                ui.label(RichText::new(warning).small().color(p.text_secondary));
            }
            ui.add_space(space::M);
            list_frame(ui, |ui| {
                if self.plan.jobs.is_empty() {
                    ui.label(RichText::new(tr("import-empty")).color(p.text_secondary));
                }
                for i in 0..self.plan.jobs.len() {
                    if i > 0 {
                        ui.separator();
                    }
                    changed |= self.row(ui, i);
                }
            });
            ui.add_space(space::S);
            message_bar(ui, InfoKind::Info, &flow(&tr("import-note")));
            if let Some(error) = &self.error {
                ui.add_space(space::S);
                message_bar(ui, InfoKind::Error, &flow(error));
            }
            ui.add_space(space::L);
            let count = self.count();
            ui.horizontal(|ui| {
                let import = ui
                    .add_enabled_ui(editable && count > 0, |ui| {
                        primary_button(ui, &tr!("import-confirm", count = count))
                    })
                    .inner;
                if import.clicked() {
                    closed = self.import(backend);
                }
                if secondary_button(ui, &tr("button-cancel")).clicked() {
                    closed = Closed::Cancelled;
                }
            });
        });
        if changed {
            self.replan(backend);
        }
        if modal.should_close() && closed == Closed::Open {
            closed = Closed::Cancelled;
        }
        closed
    }

    /// One job of the file: its check box and name, its badges, the choice for a taken name,
    /// and under them what importing it does. Whether a choice changed.
    fn row(&mut self, ui: &mut Ui, i: usize) -> bool {
        let p = theme::of(ui);
        let planned = self.plan.jobs[i].clone();
        // Invalid in itself: never chosen. Invalid only as the choice for its taken name has
        // it (a copy that would overlap): another choice can still import it.
        let invalid = locked(&planned);
        let badges = badges(&planned);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.set_min_height(28.0);
            let mut on = self.choices[i].import && !invalid;
            let check = ui
                .add_enabled_ui(!invalid, |ui| ui.checkbox(&mut on, ""))
                .inner
                .accessible_name(tr!("import-job-for", job = planned.label.as_str()));
            let badges_room: f32 = badges
                .iter()
                .map(|(_, text, _)| status_chip_width(ui, text) + ui.spacing().item_spacing.x)
                .sum();
            let choice_room = if planned.conflict.is_some() {
                170.0
            } else {
                0.0
            };
            let label = leaving_room(ui, badges_room + choice_room, |ui| {
                ui.add_enabled(
                    !invalid,
                    egui::Label::new(RichText::new(&planned.label).color(p.text))
                        .truncate()
                        .sense(egui::Sense::click()),
                )
            });
            if check.changed() || (label.clicked() && !invalid) {
                self.choices[i].import = if check.changed() { on } else { !on };
                changed = true;
            }
            for (tone, text, hint) in &badges {
                let chip = status_chip(ui, *tone, text);
                if let Some(hint) = hint {
                    chip.on_hover_text(hint);
                }
            }
            if planned.conflict.is_some() {
                changed |= self.conflict_choice(ui, i, &planned.label);
            }
        });
        // What importing it does, and what it runs, in small type under its name.
        let indent = ui.spacing().interact_size.y + ui.spacing().item_spacing.x;
        let mut notes: Vec<(String, egui::Color32)> = Vec::new();
        match (&planned.status, &planned.action) {
            (Status::Invalid(reasons), _) => {
                notes.extend(reasons.iter().map(|r| (r.clone(), p.failed)));
            }
            (Status::NameExists(_), Action::Add(name)) => {
                notes.push((
                    tr!("import-as-copy", name = name.as_str()),
                    p.text_secondary,
                ));
            }
            (_, Action::Replace(name)) => notes.push((
                tr!("import-replaces", name = name.as_str()),
                p.text_secondary,
            )),
            _ => {}
        }
        for cmd in &planned.commands {
            notes.push((tr!("import-runs", cmd = cmd.as_str()), p.text_secondary));
        }
        for warning in &planned.warnings {
            notes.push((warning.clone(), p.text_secondary));
        }
        for (text, colour) in notes {
            ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.add(egui::Label::new(RichText::new(text).small().color(colour)).wrap());
            });
        }
        changed
    }

    /// Skip, Import as a copy or Replace, for a job whose name a job here has.
    fn conflict_choice(&mut self, ui: &mut Ui, i: usize, job: &str) -> bool {
        let label = |c: OnConflict| {
            tr(match c {
                OnConflict::Skip => "import-skip",
                OnConflict::Copy => "import-copy",
                OnConflict::Replace => "import-replace",
            })
        };
        let before = self.choices[i].on_conflict;
        let mut choice = before;
        dropdown(
            ui,
            ("import-conflict", i),
            None,
            &label(choice),
            170.0,
            |ui| {
                for c in [OnConflict::Skip, OnConflict::Copy, OnConflict::Replace] {
                    dropdown_value(ui, &mut choice, c, label(c));
                }
            },
        )
        .accessible_name(tr!("import-conflict-for", job = job));
        self.choices[i].on_conflict = choice;
        choice != before
    }

    /// Plans again against the settings as they are now, and saves the result.
    fn import(&mut self, backend: &dyn Backend) -> Closed {
        let mut done = None;
        let result = change_settings(backend, |settings| {
            let plan = self.plan_on(backend, settings);
            *settings = plan.settings.clone();
            done = Some(plan);
        });
        let plan = match (result, done) {
            (Ok(_), Some(plan)) => plan,
            (Err(e), _) => {
                self.error = Some(tr!("import-failed", error = e));
                self.replan(backend);
                return Closed::Open;
            }
            // change_settings always runs the edit before it saves.
            (Ok(_), None) => unreachable!("the settings were changed without the plan"),
        };
        let imported: Vec<&str> = plan
            .jobs
            .iter()
            .filter_map(|j| match &j.action {
                Action::Add(name) | Action::Replace(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let skipped = plan
            .jobs
            .iter()
            .filter(|j| j.action == Action::Skip)
            .count();
        tracing::info!(file = %self.path.display(), imported = imported.len(), skipped, "imported jobs");
        Closed::Done {
            message: tr!("import-done", count = imported.len(), skipped = skipped),
            select: imported.first().map(|n| n.to_string()),
        }
    }
}

/// Whether a job can't be chosen at all: it's invalid in itself, not only as its taken name's
/// choice has it.
fn locked(planned: &Planned) -> bool {
    matches!(planned.status, Status::Invalid(_)) && planned.conflict.is_none()
}

/// A job's badges: (tone, text, hover text).
fn badges(planned: &Planned) -> Vec<(Tone, String, Option<String>)> {
    let mut out = Vec::new();
    if let Some(existing) = &planned.conflict {
        out.push((
            Tone::Warning,
            tr("import-status-exists"),
            Some(tr!("import-exists-hint", existing = existing.as_str())),
        ));
    }
    match &planned.status {
        Status::New => out.push((Tone::Ok, tr("import-status-new"), None)),
        Status::NameExists(_) => {}
        Status::Invalid(_) => {
            out.push((Tone::Failed, tr("import-status-invalid"), None));
            return out;
        }
    }
    if !planned.needs_password.is_empty() {
        out.push((
            Tone::Warning,
            tr("import-status-needs-password"),
            Some(tr!(
                "import-needs-password-hint",
                targets = planned.needs_password.join(", ")
            )),
        ));
    }
    if !planned.drive_not_found.is_empty() {
        out.push((
            Tone::Neutral,
            tr("import-status-drive-not-found"),
            Some(tr!(
                "import-drive-hint",
                targets = planned.drive_not_found.join(", ")
            )),
        ));
    }
    out
}

/// A list's frame: a hairline border round rows that scroll once they fill what the window
/// leaves the list ([`LIST_HEIGHT`]).
fn list_frame(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = theme::of(ui);
    let height =
        (ui.ctx().content_rect().height() - LIST_ROOM_AROUND).clamp(LIST_HEIGHT.0, LIST_HEIGHT.1);
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(space::M as i8, space::S as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, add);
        });
}
