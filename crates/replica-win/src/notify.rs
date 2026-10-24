//! [`Notifier`] for Windows: notifications as balloons from the tray icon.
//!
//! Windows shows a balloon as a notification (and keeps it in the notification centre), in
//! every mode: unlike toasts, it needs no AppUserModelID, which a portable copy lacks. The tray
//! icon lives on the tray's thread, so [`TrayNotifier`] queues notifications from any thread
//! and the tray shows them ([`TrayNotifier::show_pending`]). A run without the tray
//! (`replica-tray --run`) shows its own with [`show_alone`].

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use replica_core::platform::{Level, Notification, Notifier, PlatformError};

use crate::tray::{BalloonKind, ThemedIcon, Tray};

fn kind(level: Level) -> BalloonKind {
    match level {
        Level::Info | Level::Success => BalloonKind::Info,
        Level::Warning => BalloonKind::Warning,
        Level::Error => BalloonKind::Error,
    }
}

/// Queues notifications for the tray to show.
pub struct TrayNotifier {
    send: Mutex<Sender<Notification>>,
    receive: Mutex<Receiver<Notification>>,
}

impl Default for TrayNotifier {
    fn default() -> Self {
        let (send, receive) = channel();
        Self {
            send: Mutex::new(send),
            receive: Mutex::new(receive),
        }
    }
}

impl TrayNotifier {
    /// Shows the queued notifications from `tray`, on the tray's thread.
    pub fn show_pending(&self, tray: &Tray) {
        let receive = self.receive.lock().unwrap_or_else(|e| e.into_inner());
        for n in receive.try_iter() {
            if let Err(error) = tray.balloon(&n.title, &n.body, kind(n.level)) {
                tracing::warn!(%error, title = %n.title, "couldn't show a notification");
            }
        }
    }
}

impl Notifier for TrayNotifier {
    fn notify(&self, notification: &Notification) -> Result<(), PlatformError> {
        self.send
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(notification.clone())
            .map_err(|e| PlatformError(e.to_string()))
    }
}

/// Shows `notification` with an icon of its own, for a process without the tray: the icon
/// stays for `linger` (so the balloon can be read and clicked), then goes.
pub fn show_alone(
    icon: ThemedIcon,
    tip: &str,
    notification: &Notification,
    linger: Duration,
) -> std::io::Result<()> {
    let mut tray = Tray::new(icon, tip)?;
    tray.balloon(
        &notification.title,
        &notification.body,
        kind(notification.level),
    )?;
    let until = Instant::now() + linger;
    while Instant::now() < until {
        tray.pump(Duration::from_millis(250), None);
    }
    Ok(())
}
