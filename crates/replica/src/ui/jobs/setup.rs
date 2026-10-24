//! Setting up versioned targets, as `replica init` does: find the target, make a
//! password, **show it once and go on only when the user says they've saved a copy**, store it,
//! then create the repository. Checking and creating run off the UI thread.
//!
//! The password window can't be skipped: its only ways out are "I've saved a copy" and Cancel,
//! and Cancel stores nothing and stops the whole save.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui;
use replica_core::i18n::tr;
use replica_core::job::Target;
use replica_core::tr;
use replica_win::keys::{self, NewPassword};

use crate::ui::backend::{Backend, TargetCheck};
use crate::ui::theme::space;
use crate::ui::widgets::{copy_field, flow, heading, primary_button, secondary_button, steps};

/// How setting up the targets ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupEnd {
    /// Each target is set up, or was already. `notes` says what's worth knowing.
    Done {
        notes: Vec<String>,
    },
    /// Some targets aren't connected: the user can save without them, or not.
    NotAvailable {
        notes: Vec<String>,
    },
    /// The user cancelled at the password.
    Cancelled,
    Failed(String),
}

enum Step {
    Next,
    Checking(Target, Receiver<TargetCheck>),
    Password(Target, NewPassword),
    Creating(Target, Receiver<Result<(), String>>),
}

pub struct Setup {
    backend: Arc<dyn Backend>,
    queue: VecDeque<Target>,
    step: Step,
    notes: Vec<String>,
    not_available: bool,
}

impl Setup {
    pub fn new(backend: Arc<dyn Backend>, targets: Vec<Target>) -> Self {
        Self {
            backend,
            queue: targets.into(),
            step: Step::Next,
            notes: Vec::new(),
            not_available: false,
        }
    }

    /// What it's doing now, for a line under the editor.
    pub fn activity(&self) -> Option<String> {
        match &self.step {
            Step::Checking(t, _) => Some(tr!("editor-setup-checking", target = t.display_name())),
            Step::Creating(t, _) => Some(
                // Waiting its turn for the repository, if another job is using it.
                self.backend
                    .waiting()
                    .unwrap_or_else(|| tr!("editor-setup-initialising", target = t.display_name())),
            ),
            _ => None,
        }
    }

    /// Moves on as far as it can, showing the password window when it's waiting for the user.
    /// Returns how it ended, once it has.
    pub fn update(&mut self, ctx: &egui::Context) -> Option<SetupEnd> {
        loop {
            match std::mem::replace(&mut self.step, Step::Next) {
                Step::Next => match self.queue.pop_front() {
                    None => {
                        let notes = std::mem::take(&mut self.notes);
                        return Some(if self.not_available {
                            SetupEnd::NotAvailable { notes }
                        } else {
                            SetupEnd::Done { notes }
                        });
                    }
                    Some(target) => {
                        let rx = self.spawn(ctx, {
                            let target = target.clone();
                            move |b: &dyn Backend| b.check_target(&target)
                        });
                        self.step = Step::Checking(target, rx);
                    }
                },
                Step::Checking(target, rx) => match rx.try_recv() {
                    Ok(check) => {
                        if let Some(end) = self.checked(ctx, target, check) {
                            return Some(end);
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        self.step = Step::Checking(target, rx);
                        return None;
                    }
                    Err(TryRecvError::Disconnected) => {
                        return Some(SetupEnd::Failed(tr!(
                            "editor-setup-failed",
                            target = target.display_name(),
                            error = tr("editor-setup-stopped")
                        )));
                    }
                },
                Step::Password(target, new) => {
                    return match self.password_window(ctx, &target, &new) {
                        PasswordAnswer::Waiting => {
                            self.step = Step::Password(target, new);
                            None
                        }
                        PasswordAnswer::Cancel => Some(SetupEnd::Cancelled),
                        PasswordAnswer::Saved => match new.confirm_saved(self.backend.secrets()) {
                            Ok(secret) => {
                                self.create(ctx, target, secret);
                                continue;
                            }
                            Err(e) => Some(self.failed(&target, &e.to_string())),
                        },
                    };
                }
                Step::Creating(target, rx) => match rx.try_recv() {
                    Ok(Ok(())) => self
                        .notes
                        .push(tr!("editor-setup-done", target = target.display_name())),
                    Ok(Err(e)) => return Some(self.failed(&target, &e)),
                    Err(TryRecvError::Empty) => {
                        self.step = Step::Creating(target, rx);
                        return None;
                    }
                    Err(TryRecvError::Disconnected) => {
                        return Some(self.failed(&target, &tr("editor-setup-stopped")));
                    }
                },
            }
        }
    }

    fn failed(&self, target: &Target, error: &str) -> SetupEnd {
        SetupEnd::Failed(tr!(
            "editor-setup-failed",
            target = target.display_name(),
            error = error
        ))
    }

    /// After a target is checked: go on to the next, ask for the password, or create it.
    fn checked(
        &mut self,
        ctx: &egui::Context,
        target: Target,
        check: TargetCheck,
    ) -> Option<SetupEnd> {
        let name = target.display_name();
        match check {
            TargetCheck::NotAvailable(why) => {
                self.not_available = true;
                self.notes.push(tr!(
                    "editor-setup-not-available",
                    target = name.as_str(),
                    reason = why
                ));
            }
            TargetCheck::Failed(e) => return Some(self.failed(&target, &e)),
            TargetCheck::Ready {
                initialised: true,
                has_password,
                ..
            } => self.notes.push(if has_password {
                tr!("editor-setup-exists", target = name.as_str())
            } else {
                tr!("editor-setup-no-password", target = name.as_str())
            }),
            // A password saved earlier (a set-up that stopped before restic) is used as it is,
            // as `replica init` does.
            TargetCheck::Ready {
                has_password: true, ..
            } => match keys::for_restic(&target, self.backend.secrets()) {
                Ok(Some(secret)) => self.create(ctx, target, secret),
                Ok(None) => {
                    return Some(self.failed(&target, &tr("editor-setup-password-missing")));
                }
                Err(e) => return Some(self.failed(&target, &e.to_string())),
            },
            TargetCheck::Ready { .. } => match keys::generate(&target, self.backend.secrets()) {
                Ok(new) => self.step = Step::Password(target, new),
                Err(e) => return Some(self.failed(&target, &e.to_string())),
            },
        }
        None
    }

    fn create(
        &mut self,
        ctx: &egui::Context,
        target: Target,
        secret: replica_core::platform::Secret,
    ) {
        let rx = self.spawn(ctx, {
            let target = target.clone();
            move |b: &dyn Backend| b.init_repository(&target, secret)
        });
        self.step = Step::Creating(target, rx);
    }

    /// Runs `work` on its own thread; the window repaints when it's done.
    fn spawn<T: Send + 'static>(
        &self,
        ctx: &egui::Context,
        work: impl FnOnce(&dyn Backend) -> T + Send + 'static,
    ) -> Receiver<T> {
        let (tx, rx) = channel();
        let backend = Arc::clone(&self.backend);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work(backend.as_ref()));
            ctx.request_repaint();
        });
        rx
    }

    fn password_window(
        &self,
        ctx: &egui::Context,
        target: &Target,
        new: &NewPassword,
    ) -> PasswordAnswer {
        let mut answer = PasswordAnswer::Waiting;
        super::dialog(ctx, "password-save").show(ctx, |ui| {
            ui.set_max_width(560.0);
            heading(ui, &tr("password-save-title"));
            ui.add_space(space::S);
            // Where this is: the password first, then the repository it opens.
            steps(
                ui,
                &[
                    tr("password-save-step-save"),
                    tr("password-save-step-create"),
                ],
                0,
            );
            ui.add_space(space::M);
            ui.label(flow(&tr!(
                "password-save-body",
                target = target.display_name()
            )));
            ui.add_space(space::M);
            copy_field(ui, new.reveal(), &tr("password-save-copy"));
            ui.add_space(space::L);
            ui.horizontal(|ui| {
                if primary_button(ui, &tr("password-save-confirm")).clicked() {
                    answer = PasswordAnswer::Saved;
                }
                if secondary_button(ui, &tr("button-cancel")).clicked() {
                    answer = PasswordAnswer::Cancel;
                }
            });
        });
        answer
    }
}

enum PasswordAnswer {
    Waiting,
    Saved,
    Cancel,
}
