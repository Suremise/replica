//! The job editor: every field of a job in section cards (General, Sources, Targets, Before
//! and after steps, Schedule and retention), checked by core's own validation, saved to
//! `settings.toml` and taking effect at once (the tray reloads it).
//!
//! It's a page in place of the Jobs table, not a dialog: it's long, it opens dialogs of its
//! own (the mirror warning, the password, "Enter a saved password…") that would otherwise
//! stack on it, and as a page it has the window's whole height, with its Save and Cancel in a
//! footer that stays in view while the sections scroll.
//!
//! Two rules can't be skipped:
//! - choosing **mirror** shows the warning that deletions are copied too, and only "Use a
//!   mirror anyway" makes the job a mirror (a mirror job then says so in a warning bar);
//! - a **new versioned target** is set up when the job is saved, with its password shown once
//!   ([`super::setup`]).
//!
//! A versioned target's "Enter a saved password…" puts back the copy of its password the user
//! kept, once it's checked against the repository ([`super::key_entry`]).
//!
//! Core's messages are shown next to what they're about, and counted in a bar above Save;
//! those about nothing in this job (another job in the file) are in that bar themselves.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use eframe::egui::{self, Id, Response, RichText, Ui};
use replica_core::i18n::tr;
use replica_core::job::{Job, Mode, Problem, ValidationError};
use replica_core::platform::{Credential, Secret};
use replica_core::restic::password_secret_name;
use replica_core::schedule::Schedule;
use replica_core::settings::Settings;
use replica_core::tr;
use replica_win::targets::VolumeInfo;

use super::form::{
    AfterForm, BeforeForm, JobForm, TargetForm, TargetKind, login_name, targets_to_set_up,
};
use super::key_entry::{KeyEntry, KeyEntryEnd};
use super::schedule::SchedulePicker;
use super::setup::{Setup, SetupEnd};
use crate::ui::backend::{Backend, UiRequest};
use crate::ui::icons::Icon;
use crate::ui::theme::{self, space};
use crate::ui::widgets::{
    InfoKind, Tone, danger_button, danger_button_filled, dropdown_choice, error_text, flow,
    heading, info_bar, message_bar, page_title, primary_button, removable_chip, secondary_button,
    secondary_button_with_icon, section_card, segmented, setting_row, size_text, status_chip,
    toggle,
};

/// What the editor came to this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorResult {
    Open,
    Cancelled,
    /// Saved, under this name.
    Saved(String),
}

/// What a message is about, so it's shown next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Mode,
    Sources,
    Source(usize),
    Targets,
    Target(usize),
    Steps,
    Schedule,
    Retention,
    /// Nothing in this job: shown above Save.
    General,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Issue {
    at: Field,
    text: String,
}

impl Issue {
    fn general(text: impl Into<String>) -> Self {
        Self {
            at: Field::General,
            text: text.into(),
        }
    }
}

pub struct Editor {
    backend: Arc<dyn Backend>,
    /// The job as it was, or `None` for a new one.
    original: Option<Job>,
    pub form: JobForm,
    mirror_prompt: bool,
    issues: Vec<Issue>,
    notes: Vec<String>,
    volumes: Option<Result<Vec<VolumeInfo>, String>>,
    setup: Option<(Setup, bool)>,
    /// "Enter a saved password…", while it's open.
    key_entry: Option<KeyEntry>,
    /// "Save without setting it up" is offered for exactly this form: the one whose targets
    /// were checked. Any change takes the offer away, so Save checks the new targets.
    offer_save_anyway: Option<JobForm>,
    schedule: SchedulePicker,
    /// A pattern being typed to leave out of each source.
    exclude_drafts: Vec<String>,
    /// Whether each versioned target's password is saved, by its Credential Manager name:
    /// read once, and again after a password is entered or a target set up.
    keys: HashMap<String, bool>,
    /// The saved share logins' user names, by their Credential Manager name.
    login_users: HashMap<String, Option<String>>,
    /// Each target's saved login is being replaced: its fields are shown.
    changing_login: Vec<bool>,
    /// The form as it was opened: leaving with changes asks first.
    initial: JobForm,
    discard_prompt: bool,
}

impl Editor {
    pub fn new(backend: Arc<dyn Backend>, original: Option<Job>) -> Self {
        let form = match &original {
            Some(job) => JobForm::from_job(job),
            None => JobForm::default(),
        };
        Self {
            backend,
            original,
            schedule: SchedulePicker::from_text(&form.schedule),
            initial: form.clone(),
            form,
            login_users: HashMap::new(),
            changing_login: Vec::new(),
            discard_prompt: false,
            mirror_prompt: false,
            issues: Vec::new(),
            notes: Vec::new(),
            volumes: None,
            setup: None,
            key_entry: None,
            offer_save_anyway: None,
            exclude_drafts: Vec::new(),
            keys: HashMap::new(),
        }
    }

    pub fn title(&self) -> String {
        match &self.original {
            Some(job) => tr!("editor-edit-title", job = job.name.as_str()),
            None => tr("editor-new-title"),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> EditorResult {
        let busy = self.setup.is_some();
        let mut result = EditorResult::Open;
        if self.offer_save_anyway.as_ref() != Some(&self.form) {
            self.offer_save_anyway = None;
        }
        let p = theme::of(ui);
        let footer = egui::Panel::bottom("editor-footer")
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.background)
                    .inner_margin(egui::Margin::symmetric(0, space::M as i8)),
            )
            .show(ui, |ui| {
                if let Some(r) = self.footer(ui, busy) {
                    result = r;
                }
            });
        // The panel is sized by last frame's contents: when a message comes or goes, lay the
        // frame out again, so its buttons are where they're drawn before anyone clicks them.
        let id = Id::unique("editor-footer-height");
        let height = footer.response.rect.height();
        let before: Option<f32> = ui.data(|d| d.get_temp(id));
        if before.is_some_and(|h| (h - height).abs() > 0.5) {
            ui.ctx()
                .request_discard("the editor's footer changed height");
        }
        ui.data_mut(|d| d.insert_temp(id, height));
        // The page's header, in place of the Jobs page's: back to Jobs, and the job's name.
        ui.add_space(space::XS);
        let back = ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new(format!("← {}", tr("tab-jobs"))).color(p.accent))
                    .frame(false),
            )
            .accessible_name(tr("editor-back"));
        if back.clicked() {
            self.leave(&mut result);
        }
        page_title(ui, &self.title());
        ui.add_space(space::L);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_enabled_ui(!busy, |ui| self.fields(ui));
            });
        if let Some(r) = self.run_setup(ui.ctx()) {
            result = r;
        }
        if let Some(entry) = &mut self.key_entry
            && let Some(end) = entry.update(ui.ctx())
        {
            self.key_entry = None;
            self.keys.clear();
            if let KeyEntryEnd::Saved(note) = end {
                self.notes = vec![note];
            }
        }
        self.mirror_warning(ui.ctx());
        self.discard_window(ui.ctx(), &mut result);
        result
    }

    /// Whether anything was changed since the editor opened.
    pub fn dirty(&self) -> bool {
        self.form != self.initial
    }

    /// Cancel or Back: at once if nothing changed, else after "Discard changes?".
    fn leave(&mut self, result: &mut EditorResult) {
        if self.dirty() {
            self.discard_prompt = true;
        } else {
            *result = EditorResult::Cancelled;
        }
    }

    fn discard_window(&mut self, ctx: &egui::Context, result: &mut EditorResult) {
        if !self.discard_prompt {
            return;
        }
        let modal = super::dialog(ctx, "discard-changes").show(ctx, |ui| {
            ui.set_max_width(420.0);
            heading(ui, &tr("editor-discard-title"));
            ui.add_space(space::S);
            ui.label(tr("editor-discard-body"));
            ui.add_space(space::L);
            ui.horizontal(|ui| {
                if danger_button_filled(ui, &tr("editor-discard-confirm")).clicked() {
                    self.discard_prompt = false;
                    *result = EditorResult::Cancelled;
                }
                if secondary_button(ui, &tr("editor-discard-keep")).clicked() {
                    self.discard_prompt = false;
                }
            });
        });
        if modal.should_close() {
            self.discard_prompt = false;
        }
    }

    /// Core's messages, the notes, and Save and Cancel.
    fn footer(&mut self, ui: &mut Ui, busy: bool) -> Option<EditorResult> {
        let mut result = None;
        let marked = self
            .issues
            .iter()
            .filter(|i| i.at != Field::General)
            .count();
        if marked > 0 {
            message_bar(ui, InfoKind::Error, &tr!("editor-problems", count = marked));
            ui.add_space(space::XS);
        }
        for issue in self.issues.iter().filter(|i| i.at == Field::General) {
            message_bar(ui, InfoKind::Error, &sentence(&issue.text));
            ui.add_space(space::XS);
        }
        for note in &self.notes {
            message_bar(ui, InfoKind::Info, &flow(note));
            ui.add_space(space::XS);
        }
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            let can_save = self.backend.writable() && !busy;
            if ui
                .add_enabled_ui(can_save, |ui| primary_button(ui, &tr("button-save")))
                .inner
                .clicked()
                && let Some(r) = self.save_clicked(ui.ctx())
            {
                result = Some(r);
            }
            if self.offer_save_anyway.is_some()
                && ui
                    .add_enabled_ui(!busy, |ui| {
                        secondary_button(ui, &tr("editor-setup-save-anyway"))
                    })
                    .inner
                    .clicked()
                && self.offer_save_anyway.as_ref() == Some(&self.form)
            {
                result = Some(self.commit_or_show());
            }
            if ui
                .add_enabled_ui(!busy, |ui| secondary_button(ui, &tr("button-cancel")))
                .inner
                .clicked()
            {
                let mut leaving = EditorResult::Open;
                self.leave(&mut leaving);
                if leaving != EditorResult::Open {
                    result = Some(leaving);
                }
            }
            if let Some(activity) = self.setup.as_ref().and_then(|(s, _)| s.activity()) {
                ui.add_space(space::S);
                ui.spinner();
                ui.label(activity);
            }
        });
        result
    }

    // --- Saving ---------------------------------------------------------------------------

    fn save_clicked(&mut self, ctx: &egui::Context) -> Option<EditorResult> {
        self.issues.clear();
        self.notes.clear();
        self.offer_save_anyway = None;
        let job = match self.validated() {
            Ok(job) => job,
            Err(issues) => {
                self.issues = issues;
                return None;
            }
        };
        if let Err(e) = self.store_logins() {
            self.issues = vec![Issue::general(e)];
            return None;
        }
        let to_set_up = targets_to_set_up(&job, self.original.as_ref());
        if to_set_up.is_empty() {
            return Some(self.commit_or_show());
        }
        let setup = Setup::new(Arc::clone(&self.backend), to_set_up);
        self.setup = Some((setup, true));
        ctx.request_repaint();
        None
    }

    fn run_setup(&mut self, ctx: &egui::Context) -> Option<EditorResult> {
        let (setup, then_save) = self.setup.as_mut()?;
        let then_save = *then_save;
        let end = setup.update(ctx)?;
        self.setup = None;
        self.keys.clear();
        match end {
            SetupEnd::Done { notes } => {
                self.notes = notes;
                if then_save {
                    return Some(self.commit_or_show());
                }
            }
            SetupEnd::NotAvailable { notes } => {
                self.notes = notes;
                self.offer_save_anyway = then_save.then(|| self.form.clone());
            }
            SetupEnd::Cancelled => self.notes = vec![tr("editor-not-saved")],
            SetupEnd::Failed(e) => self.issues = vec![Issue::general(e)],
        }
        None
    }

    /// The job, checked against the other jobs as they are on disk now.
    fn validated(&self) -> Result<Job, Vec<Issue>> {
        let job = self.form.to_job().map_err(|errors| {
            errors
                .into_iter()
                .map(|text| Issue {
                    at: self.schedule_error_place(&text),
                    text,
                })
                .collect::<Vec<_>>()
        })?;
        let settings = self.with_job(&job).map_err(|e| vec![Issue::general(e)])?;
        let issues: Vec<Issue> = settings
            .validate()
            .iter()
            .map(|e| self.issue(e, &job))
            .collect();
        if issues.is_empty() {
            Ok(job)
        } else {
            Err(issues)
        }
    }

    /// A schedule that doesn't parse: the job's own, or a verify step's.
    fn schedule_error_place(&self, text: &str) -> Field {
        match self.form.schedule.trim().parse::<Schedule>() {
            Err(e) if e.to_string() == text => Field::Schedule,
            _ => Field::Steps,
        }
    }

    /// Where core's `error` is shown: next to what it's about if it's about `job`.
    fn issue(&self, error: &ValidationError, job: &Job) -> Issue {
        let ours = match (&error.job, &error.problem) {
            // The other job of the two, by the name typed here.
            (_, Problem::NameDuplicate(n)) => n.eq_ignore_ascii_case(&job.name),
            (Some(label), _) if job.name.is_empty() => label.starts_with("jobs["),
            (Some(label), _) => label.eq_ignore_ascii_case(&job.name),
            (None, _) => false,
        };
        if !ours {
            return Issue::general(error.to_string());
        }
        let target_at = |pred: &dyn Fn(&TargetForm) -> bool| {
            self.form
                .targets
                .iter()
                .position(pred)
                .map_or(Field::Targets, Field::Target)
        };
        let at = match &error.problem {
            Problem::NameMissing | Problem::NameInvalid(_) | Problem::NameDuplicate(_) => {
                Field::Name
            }
            Problem::NoSources | Problem::BadExclude(_) => Field::Sources,
            Problem::SourceNotAbsolute(path) => self
                .form
                .sources
                .iter()
                .position(|s| std::path::Path::new(s.path.trim()) == path)
                .map_or(Field::Sources, Field::Source),
            Problem::NoTargets
            | Problem::CredentialEmpty
            | Problem::CredentialReserved(_)
            | Problem::MirrorOverlapsMirror { .. }
            | Problem::MirrorOverlapsRepository { .. } => Field::Targets,
            Problem::DriveWithoutVolume => target_at(&|t| {
                t.kind == TargetKind::Drive
                    && t.volume.volume_id.is_none()
                    && t.volume.label.is_none()
            }),
            Problem::DrivePathOutside(path) => target_at(&|t| {
                t.kind == TargetKind::Drive && std::path::Path::new(t.path.trim()) == path
            }),
            Problem::UncPathInvalid(path) => {
                target_at(&|t| t.kind == TargetKind::Share && t.path.trim() == path)
            }
            Problem::MirrorNotConfirmed => Field::Mode,
            Problem::VerifyOnMirror | Problem::EmptyCommand | Problem::ZeroTimeout => Field::Steps,
            Problem::RetentionKeepsNothing | Problem::DeleteLimitOver100 => Field::Retention,
        };
        Issue {
            at,
            text: error.problem.to_string(),
        }
    }

    /// The settings on disk now, with `job` in place of the one edited (or added).
    fn with_job(&self, job: &Job) -> Result<Settings, String> {
        let mut settings = Settings::load_or_default(&self.backend.settings_file())
            .map_err(|e| e.to_string())?
            .settings;
        let at = self.original.as_ref().and_then(|o| {
            settings
                .jobs
                .iter()
                .position(|j| j.name.eq_ignore_ascii_case(&o.name))
        });
        match at {
            Some(i) => settings.jobs[i] = job.clone(),
            None => settings.jobs.push(job.clone()),
        }
        Ok(settings)
    }

    /// Saves the shares' logins being typed to Credential Manager.
    fn store_logins(&mut self) -> Result<(), String> {
        for i in 0..self.form.targets.len() {
            self.store_login(i)?;
        }
        Ok(())
    }

    /// Saves target `i`'s login, if one is being typed, to Credential Manager.
    fn store_login(&mut self, i: usize) -> Result<(), String> {
        let target = &mut self.form.targets[i];
        if target.kind != TargetKind::Share || !target.has_new_login() {
            return Ok(());
        }
        let name = login_name(&target.path);
        let user = target.login_user.trim();
        let credential = Credential {
            username: (!user.is_empty()).then(|| user.to_owned()),
            secret: Secret::new(target.login_password.as_str()),
        };
        self.backend
            .secrets()
            .set(&name, &credential)
            .map_err(|e| e.0)?;
        self.login_users.remove(&name);
        target.credential = Some(name);
        target.login_user.clear();
        target.login_password.clear();
        if let Some(changing) = self.changing_login.get_mut(i) {
            *changing = false;
        }
        Ok(())
    }

    /// The user name of the login saved as `name`, from Credential Manager the first time.
    fn login_user(&mut self, name: &str) -> Option<String> {
        if let Some(user) = self.login_users.get(name) {
            return user.clone();
        }
        let user = self
            .backend
            .secrets()
            .get(name)
            .ok()
            .flatten()
            .and_then(|c| c.username);
        self.login_users.insert(name.to_owned(), user.clone());
        user
    }

    /// A share target's path and login: a saved one as a chip with its user name (never its
    /// password), with Change… and Remove; otherwise, or while changing, the fields and
    /// "Save login".
    fn share(&mut self, ui: &mut Ui, i: usize) {
        self.changing_login.resize(self.form.targets.len(), false);
        let target = &mut self.form.targets[i];
        setting_row(ui, &tr("editor-share-path"), None, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut target.path)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(280.0),
            )
        });
        let saved = target.credential.clone();
        let changing = self.changing_login[i] || target.has_new_login();
        if let Some(name) = saved.as_ref().filter(|_| !changing) {
            let user = self.login_user(name);
            let mut change = false;
            let mut remove = false;
            setting_row(ui, &tr("editor-share-login"), None, |ui| {
                remove = secondary_button(ui, &tr("editor-share-login-remove-saved"))
                    .accessible_name(tr("editor-share-login-remove"))
                    .clicked();
                change = secondary_button(ui, &tr("editor-share-login-change")).clicked();
                // Which account is saved: its user name, never its password.
                let text = match &user {
                    Some(user) => tr!("editor-share-login-saved-as", user = user.as_str()),
                    None => tr("editor-share-login-saved-chip"),
                };
                status_chip(ui, Tone::Ok, &text)
            });
            if change {
                self.changing_login[i] = true;
            }
            if remove {
                self.form.targets[i].credential = None;
            }
            return;
        }
        let status = if saved.is_some() {
            tr("editor-share-login-replacing")
        } else {
            tr("editor-share-login-none")
        };
        let mut save = false;
        let mut cancel = false;
        // A row each, so each field is named by its own label.
        setting_row(ui, &tr("editor-share-user"), Some(&status), |ui| {
            ui.add(egui::TextEdit::singleline(&mut target.login_user).desired_width(200.0))
        });
        let can_save = target.has_new_login() && !target.path.trim().is_empty();
        setting_row(ui, &tr("editor-share-password"), None, |ui| {
            save = ui
                .add_enabled_ui(can_save, |ui| {
                    secondary_button(ui, &tr("editor-share-login-save"))
                })
                .inner
                .clicked();
            if saved.is_some() {
                cancel = secondary_button(ui, &tr("button-cancel"))
                    .accessible_name(tr("editor-share-login-keep"))
                    .clicked();
            }
            ui.add(
                egui::TextEdit::singleline(&mut target.login_password)
                    .password(true)
                    .desired_width(200.0),
            )
        });
        if cancel {
            let target = &mut self.form.targets[i];
            target.login_user.clear();
            target.login_password.clear();
            self.changing_login[i] = false;
        }
        if save && let Err(e) = self.store_login(i) {
            self.issues = vec![Issue::general(e)];
        }
    }

    fn commit_or_show(&mut self) -> EditorResult {
        match self.commit() {
            Ok(name) => EditorResult::Saved(name),
            Err(issues) => {
                self.issues = issues;
                EditorResult::Open
            }
        }
    }

    /// Writes the job to the settings file and tells the tray.
    fn commit(&mut self) -> Result<String, Vec<Issue>> {
        let job = self.validated()?;
        let settings = self.with_job(&job).map_err(|e| vec![Issue::general(e)])?;
        settings
            .save(&self.backend.settings_file())
            .map_err(|e| vec![Issue::general(e.to_string())])?;
        self.backend.send(UiRequest::Reload);
        Ok(job.name)
    }

    // --- The mirror warning ---------------------------------------------------------------

    fn mirror_warning(&mut self, ctx: &egui::Context) {
        if !self.mirror_prompt {
            return;
        }
        super::dialog(ctx, "mirror-warning").show(ctx, |ui| {
            ui.set_max_width(480.0);
            heading(ui, &tr("mirror-warning-title"));
            ui.add_space(space::S);
            ui.label(flow(&tr("mirror-warning-body")));
            ui.add_space(space::L);
            ui.horizontal(|ui| {
                if secondary_button(ui, &tr("mirror-warning-keep-versioned")).clicked() {
                    self.mirror_prompt = false;
                }
                if danger_button_filled(ui, &tr("mirror-warning-confirm")).clicked() {
                    self.form.mode = Mode::Mirror;
                    self.form.mirror_deletes_confirmed = true;
                    self.mirror_prompt = false;
                }
            });
        });
    }

    // --- The fields -----------------------------------------------------------------------

    /// Core's messages about `at`, under it.
    fn issues_at(&self, ui: &mut Ui, at: Field) {
        for issue in self.issues.iter().filter(|i| i.at == at) {
            ui.horizontal_wrapped(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 16.0), egui::Sense::hover());
                Icon::Cross.paint(ui.painter(), rect.shrink(1.0), theme::of(ui).failed);
                error_text(ui, &sentence(&issue.text));
            });
        }
    }

    fn fields(&mut self, ui: &mut Ui) {
        section_card(ui, &tr("editor-section-general"), None, |ui| {
            self.general(ui)
        });
        section_card(
            ui,
            &tr("editor-section-sources"),
            Some(&tr("editor-section-sources-desc")),
            |ui| self.sources(ui),
        );
        section_card(
            ui,
            &tr("editor-section-targets"),
            Some(&tr("editor-section-targets-desc")),
            |ui| self.targets(ui),
        );
        section_card(
            ui,
            &tr("editor-section-steps"),
            Some(&tr("editor-section-steps-desc")),
            |ui| self.steps(ui),
        );
        section_card(
            ui,
            &tr("editor-section-schedule"),
            Some(&tr("editor-section-schedule-desc")),
            |ui| self.schedule_and_retention(ui),
        );
    }

    fn general(&mut self, ui: &mut Ui) {
        setting_row(ui, &tr("jobs-name"), Some(&tr("editor-name-hint")), |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.form.name).desired_width(240.0))
        });
        self.issues_at(ui, Field::Name);
        setting_row(ui, &tr("editor-on"), Some(&tr("editor-on-desc")), |ui| {
            toggle(ui, &mut self.form.enabled)
        });
        setting_row(ui, &tr("jobs-notify-success"), None, |ui| {
            toggle(ui, &mut self.form.notify_on_success)
        });
        ui.add_space(space::S);
        ui.label(tr("jobs-mode"));
        self.mode(ui);
        self.issues_at(ui, Field::Mode);
        if self.form.mode == Mode::Mirror {
            ui.add_space(space::S);
            info_bar(
                ui,
                InfoKind::Warning,
                Some(&tr("editor-mirror-bar-title")),
                &tr("editor-mirror-bar"),
            );
        }
    }

    fn mode(&mut self, ui: &mut Ui) {
        let versioned = ui.radio(
            self.form.mode == Mode::Versioned,
            format!(
                "{}: {}",
                tr("mode-versioned"),
                tr("editor-mode-versioned-help")
            ),
        );
        if versioned.clicked() {
            self.form.mode = Mode::Versioned;
            self.form.mirror_deletes_confirmed = false;
        }
        let mirror = ui.radio(
            self.form.mode == Mode::Mirror,
            format!("{}: {}", tr("mode-mirror"), tr("editor-mode-mirror-help")),
        );
        if mirror.clicked() && self.form.mode != Mode::Mirror {
            // Only the warning's "Use a mirror anyway" makes it a mirror.
            self.mirror_prompt = true;
        }
    }

    fn sources(&mut self, ui: &mut Ui) {
        let mut remove = None;
        self.exclude_drafts
            .resize(self.form.sources.len(), String::new());
        if self.form.sources.is_empty() {
            ui.label(RichText::new(tr("editor-sources-none")).color(theme::of(ui).text_secondary));
        }
        for i in 0..self.form.sources.len() {
            item_frame(ui, |ui| {
                let source = &mut self.form.sources[i];
                ui.horizontal(|ui| {
                    let l = ui.label(tr("editor-source-path"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if remove_button(ui, &source.path).clicked() {
                            remove = Some(i);
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut source.path)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(l.id);
                    });
                });
                ui.add_space(space::XS);
                excludes(ui, &mut source.excludes, &mut self.exclude_drafts[i]);
            });
            self.issues_at(ui, Field::Source(i));
        }
        if let Some(i) = remove {
            self.form.sources.remove(i);
            self.exclude_drafts.remove(i);
        }
        self.issues_at(ui, Field::Sources);
        ui.horizontal(|ui| {
            if secondary_button_with_icon(ui, Icon::Folder, &tr("editor-add-folder")).clicked()
                && let Some(path) = self.backend.pick_folder()
            {
                self.form.add_source(path);
            }
            if secondary_button_with_icon(ui, Icon::Add, &tr("editor-add-file")).clicked()
                && let Some(path) = self.backend.pick_file()
            {
                self.form.add_source(path);
            }
        });
    }

    /// Whether `target`'s password is saved here, from Credential Manager the first time.
    fn key_saved(&mut self, name: &str) -> bool {
        if let Some(saved) = self.keys.get(name) {
            return *saved;
        }
        let saved = self.backend.secrets().get(name).ok().flatten().is_some();
        self.keys.insert(name.to_owned(), saved);
        saved
    }

    fn targets(&mut self, ui: &mut Ui) {
        let mut remove = None;
        let mut set_up = None;
        let mut enter_key = None;
        let versioned = self.form.mode == Mode::Versioned;
        if self.volumes.is_none()
            && self
                .form
                .targets
                .iter()
                .any(|t| t.kind == TargetKind::Drive)
        {
            self.volumes = Some(self.backend.volumes());
        }
        // The targets the job had as a versioned job: their repositories exist already.
        let known: HashSet<String> = self
            .original
            .as_ref()
            .filter(|o| o.mode == Mode::Versioned)
            .map(|o| o.targets.iter().map(password_secret_name).collect())
            .unwrap_or_default();
        if self.form.targets.is_empty() {
            ui.label(RichText::new(tr("editor-targets-none")).color(theme::of(ui).text_secondary));
        }
        for i in 0..self.form.targets.len() {
            let what = self.form.targets[i].to_target().display_name();
            let kind = self.form.targets[i].kind.clone();
            item_frame(ui, |ui| {
                ui.horizontal(|ui| {
                    let title = match kind {
                        TargetKind::Drive => tr("editor-target-drive"),
                        TargetKind::Share => tr("editor-target-share"),
                    };
                    ui.label(RichText::new(title).font(theme::semibold(theme::BODY_SIZE)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if remove_button(ui, &what).clicked() {
                            remove = Some(i);
                        }
                    });
                });
                match kind {
                    TargetKind::Drive => self.drive(ui, i),
                    TargetKind::Share => self.share(ui, i),
                }
                let target = &mut self.form.targets[i];
                setting_row(ui, &tr("editor-target-name"), None, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut target.name).desired_width(200.0))
                });
                if versioned {
                    let target = self.form.targets[i].to_target();
                    let name = password_secret_name(&target);
                    let new = !known.contains(&name);
                    let saved = !new && self.key_saved(&name);
                    let response = setting_row(
                        ui,
                        &tr("editor-key-status"),
                        (!new && !saved)
                            .then(|| tr("editor-key-missing-hint"))
                            .as_deref(),
                        |ui| {
                            let enter = secondary_button(ui, &tr("editor-key-enter"))
                                .accessible_name(tr!(
                                    "editor-key-enter-for",
                                    target = what.as_str()
                                ))
                                .on_hover_text(tr("editor-key-enter-hint"));
                            if enter.clicked() {
                                enter_key = Some(i);
                            }
                            if self.original.is_some()
                                && secondary_button(ui, &tr("editor-setup")).clicked()
                            {
                                set_up = Some(i);
                            }
                            ui.add_space(space::XS);
                            let (tone, key) = if new {
                                (Tone::Neutral, "editor-key-new")
                            } else if saved {
                                (Tone::Ok, "editor-key-saved")
                            } else {
                                (Tone::Warning, "editor-key-missing")
                            };
                            status_chip(ui, tone, &tr(key))
                        },
                    );
                    let _ = response;
                }
            });
            self.issues_at(ui, Field::Target(i));
        }
        if let Some(i) = remove {
            self.form.targets.remove(i);
            if i < self.changing_login.len() {
                self.changing_login.remove(i);
            }
        }
        if let Some(i) = enter_key {
            self.issues.clear();
            self.notes.clear();
            let target = self.form.targets[i].to_target();
            // A share's login, if one is being typed, is what reaches the repository.
            match self.store_logins() {
                Ok(()) => self.key_entry = Some(KeyEntry::new(Arc::clone(&self.backend), target)),
                Err(e) => self.issues = vec![Issue::general(e)],
            }
        }
        if let Some(i) = set_up {
            self.issues.clear();
            self.notes.clear();
            let target = self.form.targets[i].to_target();
            match self.store_logins() {
                Ok(()) => {
                    self.setup = Some((Setup::new(Arc::clone(&self.backend), vec![target]), false))
                }
                Err(e) => self.issues = vec![Issue::general(e)],
            }
        }
        self.issues_at(ui, Field::Targets);
        ui.horizontal(|ui| {
            if secondary_button_with_icon(ui, Icon::Add, &tr("editor-add-drive")).clicked() {
                self.form.targets.push(TargetForm::drive());
            }
            if secondary_button_with_icon(ui, Icon::Add, &tr("editor-add-share")).clicked() {
                self.form.targets.push(TargetForm::share());
            }
        });
    }

    fn drive(&mut self, ui: &mut Ui, i: usize) {
        let volumes = self.volumes.clone();
        let mut refresh = false;
        let target = &mut self.form.targets[i];
        setting_row(ui, &tr("editor-drive"), None, |ui| {
            if secondary_button(ui, &tr("editor-drive-refresh")).clicked() {
                refresh = true;
            }
            let current = describe_ref(&target.volume, volumes.as_ref());
            egui::ComboBox::from_id_salt(("drive", i))
                .selected_text(current)
                .width(300.0)
                .show_ui(ui, |ui| {
                    if let Some(Ok(list)) = &volumes {
                        for v in list {
                            let chosen = target.volume.volume_id.as_deref() == Some(v.id.as_str());
                            if dropdown_choice(ui, chosen, describe(v)).clicked() {
                                target.volume.volume_id = Some(v.id.clone());
                                target.volume.label = v.label.clone();
                            }
                        }
                    }
                })
                .response
        });
        if refresh {
            self.volumes = Some(self.backend.volumes());
        }
        if let Some(Err(e)) = &self.volumes {
            error_text(ui, e);
        }
        let target = &mut self.form.targets[i];
        setting_row(ui, &tr("editor-drive-folder"), None, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut target.path)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(240.0),
            )
        });
    }

    fn steps(&mut self, ui: &mut Ui) {
        let p = theme::of(ui);
        let mirror = self.form.mode == Mode::Mirror;
        ui.label(RichText::new(tr("editor-steps-before")).font(theme::semibold(theme::BODY_SIZE)));
        let mut remove = None;
        if self.form.before.is_empty() {
            ui.label(RichText::new(tr("editor-steps-none")).color(p.text_secondary));
        }
        for (i, step) in self.form.before.iter_mut().enumerate() {
            item_frame(ui, |ui| {
                let (title, what) = match step {
                    BeforeForm::Sqlite { db } => (tr("editor-step-sqlite-title"), db.clone()),
                    BeforeForm::Command { cmd, .. } => {
                        (tr("editor-step-command-title"), cmd.clone())
                    }
                };
                if step_header(ui, i, &title, &what) {
                    remove = Some(i);
                }
                match step {
                    BeforeForm::Sqlite { db } => {
                        setting_row(ui, &tr("editor-step-sqlite"), None, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(db)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(320.0),
                            )
                        });
                    }
                    BeforeForm::Command { cmd, timeout_secs } => command(ui, cmd, timeout_secs),
                }
            });
        }
        if let Some(i) = remove {
            self.form.before.remove(i);
        }
        let add = secondary_button_with_icon(ui, Icon::Add, &tr("editor-steps-add"))
            .accessible_name(tr!("editor-steps-add-to", list = tr("editor-steps-before")));
        egui::Popup::menu(&add).show(|ui| {
            if ui.button(tr("editor-steps-add-sqlite")).clicked() {
                let db = self.backend.pick_file().unwrap_or_default();
                self.form.before.push(BeforeForm::Sqlite {
                    db: db.to_string_lossy().into_owned(),
                });
                ui.close();
            }
            if ui.button(tr("editor-steps-add-command")).clicked() {
                let (cmd, timeout_secs) = JobForm::new_command();
                self.form
                    .before
                    .push(BeforeForm::Command { cmd, timeout_secs });
                ui.close();
            }
        });

        ui.add_space(space::M);
        ui.label(RichText::new(tr("editor-steps-after")).font(theme::semibold(theme::BODY_SIZE)));
        let mut remove = None;
        if self.form.after.is_empty() {
            ui.label(RichText::new(tr("editor-steps-none")).color(p.text_secondary));
        }
        for (i, step) in self.form.after.iter_mut().enumerate() {
            item_frame(ui, |ui| {
                let (title, what) = match step {
                    AfterForm::Command { cmd, .. } => {
                        (tr("editor-step-command-title"), cmd.clone())
                    }
                    AfterForm::Verify { .. } => {
                        (tr("editor-step-verify"), tr("editor-step-verify"))
                    }
                };
                if step_header(ui, i, &title, &what) {
                    remove = Some(i);
                }
                match step {
                    AfterForm::Command { cmd, timeout_secs } => command(ui, cmd, timeout_secs),
                    AfterForm::Verify { schedule, subset } => {
                        setting_row(ui, &tr("editor-step-verify-schedule"), None, |ui| {
                            ui.add(egui::TextEdit::singleline(schedule).desired_width(200.0))
                        });
                        setting_row(ui, &tr("editor-step-verify-subset"), None, |ui| {
                            ui.add(egui::TextEdit::singleline(subset).desired_width(80.0))
                        });
                    }
                }
            });
        }
        if let Some(i) = remove {
            self.form.after.remove(i);
        }
        let add = secondary_button_with_icon(ui, Icon::Add, &tr("editor-steps-add"))
            .accessible_name(tr!("editor-steps-add-to", list = tr("editor-steps-after")));
        egui::Popup::menu(&add).show(|ui| {
            if ui.button(tr("editor-steps-add-command")).clicked() {
                let (cmd, timeout_secs) = JobForm::new_command();
                self.form
                    .after
                    .push(AfterForm::Command { cmd, timeout_secs });
                ui.close();
            }
            if !mirror && ui.button(tr("editor-steps-add-verify")).clicked() {
                self.form.after.push(JobForm::new_verify());
                ui.close();
            }
        });
        self.issues_at(ui, Field::Steps);
    }

    fn schedule_and_retention(&mut self, ui: &mut Ui) {
        self.schedule.ui(ui, &mut self.form.schedule);
        self.issues_at(ui, Field::Schedule);
        if self.form.mode == Mode::Versioned {
            let r = &mut self.form.retention;
            // Keep: the newest backups (by the counts below), or everything, forever.
            let choices = [tr("editor-keep-newest"), tr("editor-keep-forever")];
            let help = tr(if r.keep_forever {
                "editor-keep-forever-help"
            } else {
                "editor-keep-newest-help"
            });
            let mut chosen = None;
            setting_row(ui, &tr("editor-retention"), Some(&help), |ui| {
                let (group, clicked) =
                    segmented(ui, &[&choices[0], &choices[1]], usize::from(r.keep_forever));
                chosen = clicked;
                group
            });
            if let Some(choice) = chosen {
                r.keep_forever = choice == 1;
            }
            if r.keep_forever {
                self.issues_at(ui, Field::Retention);
                return;
            }
            // One row: Daily, Weekly and Monthly side by side (added right to left), each
            // number named by its own label.
            setting_row(
                ui,
                &tr("editor-retention-counts"),
                Some(&tr("editor-retention-desc")),
                |ui| {
                    let mut first = None;
                    for (key, value) in [
                        ("editor-retention-monthly", &mut r.monthly),
                        ("editor-retention-weekly", &mut r.weekly),
                        ("editor-retention-daily", &mut r.daily),
                    ] {
                        let field = ui.add(egui::DragValue::new(value).range(0..=999));
                        let label = ui.label(
                            RichText::new(tr(key))
                                .small()
                                .color(theme::of(ui).text_secondary),
                        );
                        ui.add_space(space::M);
                        first = Some(field.labelled_by(label.id));
                    }
                    first.expect("three numbers")
                },
            );
        } else {
            setting_row(
                ui,
                &tr("editor-mirror-limit-title"),
                Some(&tr("editor-delete-limit")),
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.form.mirror_delete_limit_percent)
                            .range(0..=100)
                            .suffix("%"),
                    )
                },
            );
        }
        self.issues_at(ui, Field::Retention);
    }
}

/// A source, target or step: a box of its own inside its section's card.
fn item_frame(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    let p = theme::of(ui);
    egui::Frame::new()
        .fill(p.background)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(theme::CONTROL_RADIUS))
        .inner_margin(egui::Margin::symmetric(space::M as i8, space::S as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical(body);
        });
    ui.add_space(space::S);
}

/// A step's number and kind, with its Remove: `true` when removed.
fn step_header(ui: &mut Ui, i: usize, title: &str, what: &str) -> bool {
    let mut removed = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{}.  {title}", i + 1)).font(theme::semibold(theme::BODY_SIZE)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            removed = remove_button(ui, what).clicked();
        });
    });
    removed
}

/// A source's exclusions as chips, each removable, and a field to add one. The form keeps
/// them one a line.
fn excludes(ui: &mut Ui, excludes: &mut String, draft: &mut String) {
    let p = theme::of(ui);
    let lines: Vec<String> = excludes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(tr("editor-excludes")).color(p.text_secondary));
        if lines.is_empty() {
            ui.label(RichText::new(tr("editor-excludes-none")).color(p.text_secondary));
        }
        for (i, pattern) in lines.iter().enumerate() {
            if removable_chip(
                ui,
                pattern,
                &tr!("editor-remove-for", item = pattern.as_str()),
            ) {
                remove = Some(i);
            }
        }
    });
    let mut add = false;
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(draft)
                .hint_text(tr("editor-exclude-new"))
                .desired_width(320.0),
        );
        let field = field.accessible_name(tr("editor-exclude-new"));
        add |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        add |= ui
            .add_enabled_ui(!draft.trim().is_empty(), |ui| {
                secondary_button(ui, &tr("editor-exclude-add"))
            })
            .inner
            .clicked();
    });
    if remove.is_some() || (add && !draft.trim().is_empty()) {
        let mut kept: Vec<String> = lines
            .into_iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != remove)
            .map(|(_, l)| l)
            .collect();
        if add && !draft.trim().is_empty() {
            kept.push(draft.trim().to_owned());
            draft.clear();
        }
        *excludes = kept.join("\n");
    }
}

fn remove_button(ui: &mut Ui, what: &str) -> Response {
    danger_button(ui, Some(Icon::Delete), &tr("editor-remove"))
        .accessible_name(tr!("editor-remove-for", item = what))
}

fn command(ui: &mut Ui, cmd: &mut String, timeout_secs: &mut u64) {
    setting_row(ui, &tr("editor-step-command"), None, |ui| {
        ui.add(
            egui::TextEdit::singleline(cmd)
                .font(egui::TextStyle::Monospace)
                .desired_width(320.0),
        )
    });
    setting_row(ui, &tr("editor-step-timeout"), None, |ui| {
        ui.add(egui::DragValue::new(timeout_secs).range(1..=86_400))
    });
}

/// A connected volume as the drive list shows it: label, letter, free and total size.
fn describe(v: &VolumeInfo) -> String {
    let label = v.label.clone().unwrap_or_else(|| tr("volume-no-label"));
    let letter = v
        .letter()
        .map_or_else(|| tr("volume-no-letter"), |c| format!("{c}:"));
    let mut text = format!("{label} ({letter})");
    if let (Some(free), Some(total)) = (v.free_bytes, v.total_bytes) {
        text.push_str(&format!(
            ", {}",
            tr!(
                "editor-drive-space",
                free = size_text(free),
                total = size_text(total)
            )
        ));
    }
    if v.removable {
        text.push_str(&format!(", {}", tr("volume-removable").to_lowercase()));
    }
    text
}

/// The chosen volume: as the list shows it when it's connected, else by its label.
fn describe_ref(
    volume: &replica_core::job::VolumeRef,
    volumes: Option<&Result<Vec<VolumeInfo>, String>>,
) -> String {
    if volume.volume_id.is_none() && volume.label.is_none() {
        return tr("editor-drive-pick");
    }
    let connected = volumes.and_then(|v| v.as_ref().ok()).and_then(|list| {
        list.iter()
            .find(|v| volume.volume_id.as_deref() == Some(v.id.as_str()))
            .or_else(|| {
                list.iter().find(|v| {
                    v.label.is_some()
                        && v.label.as_deref().map(str::to_lowercase)
                            == volume.label.as_deref().map(str::to_lowercase)
                })
            })
    });
    match connected {
        Some(v) => describe(v),
        None => tr!(
            "editor-drive-not-connected",
            label = volume
                .label
                .clone()
                .or_else(|| volume.volume_id.clone())
                .unwrap_or_default()
        ),
    }
}

/// `text` with a capital first letter: core's messages are written to follow "job: ", and
/// stand alone here.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
