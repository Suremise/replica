//! "Enter a saved password…": putting a versioned target's repository password back from the
//! copy the user kept (on a new PC, or after its entry was removed), as `replica key set` does.
//! The password is checked against the repository off the UI thread and saved only if it opens
//! it. A saved password that opens it already is replaced only by "Replace it anyway".

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui;
use replica_core::i18n::tr;
use replica_core::job::Target;
use replica_core::platform::Secret;
use replica_core::tr;
use replica_win::keys::{Entered, NotSaved};

use crate::ui::backend::Backend;
use crate::ui::theme::space;
use crate::ui::widgets::{
    danger_button_filled, error_text, flow, heading, primary_button, secondary_button, setting_row,
};

/// How the window ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEntryEnd {
    /// Saved (or saved already): a note saying so.
    Saved(String),
    Cancelled,
}

pub struct KeyEntry {
    backend: Arc<dyn Backend>,
    target: Target,
    password: String,
    checking: Option<Receiver<Result<Entered, NotSaved>>>,
    /// Why the last try saved nothing.
    error: Option<NotSaved>,
}

impl KeyEntry {
    pub fn new(backend: Arc<dyn Backend>, target: Target) -> Self {
        Self {
            backend,
            target,
            password: String::new(),
            checking: None,
            error: None,
        }
    }

    /// Shows the window; returns how it ended, once it has.
    pub fn update(&mut self, ctx: &egui::Context) -> Option<KeyEntryEnd> {
        if let Some(end) = self.poll() {
            return Some(end);
        }
        let name = self.target.display_name();
        let busy = self.checking.is_some();
        let mut end = None;
        let mut save = None;
        super::dialog(ctx, "key-entry").show(ctx, |ui| {
            ui.set_max_width(520.0);
            heading(ui, &tr("key-entry-title"));
            ui.add_space(space::S);
            ui.label(flow(&tr!("key-entry-body", target = name.as_str())));
            ui.add_space(space::S);
            let field = setting_row(ui, &tr("key-entry-password"), None, |ui| {
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(260.0),
                )
            });
            if field.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !self.password.is_empty()
            {
                save = Some(false);
            }
            match &self.error {
                // The window's own words: the CLI's say --force.
                Some(NotSaved::SavedWorks { target }) => {
                    error_text(ui, &tr!("key-entry-saved-works", target = target.as_str()))
                }
                Some(error) => error_text(ui, &error.to_string()),
                None => {}
            }
            ui.add_space(space::L);
            ui.horizontal(|ui| {
                let can_save = !busy && !self.password.is_empty();
                if ui
                    .add_enabled_ui(can_save, |ui| primary_button(ui, &tr("key-entry-save")))
                    .inner
                    .clicked()
                {
                    save = Some(false);
                }
                if matches!(self.error, Some(NotSaved::SavedWorks { .. }))
                    && ui
                        .add_enabled_ui(can_save, |ui| {
                            danger_button_filled(ui, &tr("key-entry-replace"))
                        })
                        .inner
                        .clicked()
                {
                    save = Some(true);
                }
                if ui
                    .add_enabled_ui(!busy, |ui| secondary_button(ui, &tr("button-cancel")))
                    .inner
                    .clicked()
                {
                    end = Some(KeyEntryEnd::Cancelled);
                }
                if busy {
                    ui.spinner();
                    ui.label(tr!("key-entry-checking", target = name.as_str()));
                }
            });
        });
        if let Some(force) = save {
            self.start(ctx, force);
        }
        if end.is_some() {
            self.password.clear();
        }
        end
    }

    /// Checks and saves the password typed, on its own thread.
    fn start(&mut self, ctx: &egui::Context, force: bool) {
        let (tx, rx) = channel();
        let backend = Arc::clone(&self.backend);
        let target = self.target.clone();
        let secret = Secret::new(self.password.as_str());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(backend.save_entered_password(&target, secret, force));
            ctx.request_repaint();
        });
        self.error = None;
        self.checking = Some(rx);
    }

    /// The check's answer, if it has come.
    fn poll(&mut self) -> Option<KeyEntryEnd> {
        let rx = self.checking.as_ref()?;
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(NotSaved::Failed(tr("editor-setup-stopped"))),
        };
        self.checking = None;
        let target = self.target.display_name();
        match answer {
            Ok(entered) => {
                self.password.clear();
                let key = match entered {
                    Entered::Saved => "key-entry-saved",
                    Entered::Replaced => "key-entry-replaced",
                    Entered::Unchanged => "key-entry-unchanged",
                };
                Some(KeyEntryEnd::Saved(tr!(key, target = target.as_str())))
            }
            Err(e) => {
                // A wrong one is typed again; one that opens the repository stays, for
                // "Replace it anyway".
                if matches!(e, NotSaved::Wrong { .. }) {
                    self.password.clear();
                }
                self.error = Some(e);
                None
            }
        }
    }
}
