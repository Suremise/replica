//! Replica's window: every job listed and editable, with History, Restore and Settings tabs;
//! closing or minimising it goes to the tray.
//!
//! It's egui (eframe, with the glow renderer and AccessKit, so Narrator reads it), on a thread
//! of its own that the tray starts the first time it's opened ([`Window::start`]). The tray
//! keeps its own message loop on the main thread. A process can make only one winit event
//! loop, so the window is never closed, only hidden, and shown again by [`Window::show`].
//!
//! The window doesn't run jobs or touch schedules itself: it edits `settings.toml` and asks
//! the tray ([`backend::UiRequest`]), which reports back through a shared
//! [`backend::Status`].

pub mod about;
pub mod backend;
pub mod fonts;
pub mod geometry;
pub mod history;
pub mod icons;
pub mod jobs;
pub mod restore;
pub mod settings;
pub mod shell;
pub mod theme;
pub mod widgets;

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use replica_core::i18n::tr;
use replica_core::tr;
use replica_win::tray::IconFile;
use replica_win::window::{is_on_screen, set_icons, work_area};

use self::backend::Backend;
use self::geometry::{DEFAULT_SIZE, MIN_SIZE, WindowState, first_rect};
use self::shell::Shell;

/// The app's icon, for the window's title bar and taskbar button.
const APP_ICON: IconFile = IconFile(include_bytes!("../../../../assets/icon/replica.ico"));

/// How long the window's thread has to take the tray's Open before the tray brings the
/// window forward itself ([`Window::follow_up`]).
pub const SHOW_GRACE: Duration = Duration::from_secs(2);

/// What the tray and the window's thread share.
#[derive(Default)]
pub struct Shared {
    /// The window's egui context, once it exists: to wake it from the tray.
    ctx: Mutex<Option<egui::Context>>,
    /// The window's `HWND`, once it exists (0 before).
    hwnd: AtomicIsize,
    /// The tray asked for the window to show.
    show: AtomicBool,
    /// Why the window couldn't start, or stopped, if it did.
    failed: Mutex<Option<String>>,
}

/// How long after bringing the window forward the tray focuses it again: a dialog it owns
/// shows only once the window has restored.
const FOCUS_AGAIN: Duration = Duration::from_millis(300);
/// How long a window brought forward from the tray has to show and answer before the tray
/// says it isn't responding.
pub const RECOVER_WITHIN: Duration = Duration::from_secs(3);
/// How long the window has to start before the tray says it hasn't.
pub const START_WITHIN: Duration = Duration::from_secs(30);

/// The tray's handle on the window.
pub struct Window {
    shared: Arc<Shared>,
    /// What [`Window::follow_up`] has still to check or do.
    pending: Mutex<Pending>,
}

#[derive(Debug, Clone, Copy)]
enum Pending {
    None,
    /// Asked to show at this time, not yet seen taken.
    Asked(Instant),
    /// Brought forward from the tray at this time, not yet seen showing and answering.
    Recovering {
        since: Instant,
        focused: bool,
    },
}

/// What came of the tray's Open, once the window has had [`SHOW_GRACE`] to take it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUp {
    /// Nothing to do, or nothing yet: not asked, taken, or still within a bound.
    None,
    /// Its thread didn't take it: the tray brought the window forward itself, with the dialog
    /// it owns, and checks that it shows and answers within [`RECOVER_WITHIN`].
    BroughtForward,
    /// Brought forward from the tray, it showed and answered (its thread was in a dialog).
    Recovered,
    /// It didn't show or answer in time: its thread is stuck. The tray says so.
    NotResponding,
    /// It hasn't started within [`START_WITHIN`]. The tray says so.
    NotStarted,
    /// There's no window any more: [`Window::failed`] now says so.
    Gone,
}

/// What the tray does to the window's `HWND` itself, from its own thread: Win32 in the app
/// ([`Win32`]), a window with a stuck thread in the tests.
pub trait Native {
    fn exists(&self, hwnd: isize) -> bool;
    fn bring_forward(&self, hwnd: isize);
    fn focus_front(&self, hwnd: isize);
    /// Whether it shows (not hidden or minimised) and its thread answers.
    fn responds(&self, hwnd: isize) -> bool;
}

/// The real windows ([`replica_win::window`]).
pub struct Win32;

/// How long the tray waits for the window's thread to answer, each time it asks.
const ANSWER_WITHIN: Duration = Duration::from_millis(100);

impl Native for Win32 {
    fn exists(&self, hwnd: isize) -> bool {
        replica_win::window::exists(hwnd)
    }
    fn bring_forward(&self, hwnd: isize) {
        replica_win::window::bring_forward(hwnd)
    }
    fn focus_front(&self, hwnd: isize) {
        replica_win::window::focus_front(hwnd)
    }
    fn responds(&self, hwnd: isize) -> bool {
        replica_win::window::responds(hwnd, ANSWER_WITHIN)
    }
}

impl Window {
    /// Starts the window's thread, and shows it.
    pub fn start(backend: Arc<dyn Backend>) -> Window {
        let shared = Arc::new(Shared {
            show: AtomicBool::new(true),
            ..Shared::default()
        });
        let theirs = Arc::clone(&shared);
        tracing::info!("starting the window");
        let spawned = std::thread::Builder::new()
            .name("window".into())
            .spawn(move || {
                // A panic here is logged by the panic hook; the tray must hear of it too, or
                // its Open would ask a window that's gone.
                let ran = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    run(backend, Arc::clone(&theirs))
                }));
                let error = match ran {
                    Ok(Ok(())) => tr("window-gone"),
                    Ok(Err(error)) => error,
                    Err(panic) => tr!(
                        "window-stopped",
                        error = replica_win::logging::panic_message(panic.as_ref())
                    ),
                };
                tracing::error!(%error, "the window stopped");
                theirs.hwnd.store(0, Ordering::SeqCst);
                *theirs.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
            });
        if let Err(error) = spawned {
            *shared.failed.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(tr!("window-start-failed", error = error.to_string()));
        }
        Window {
            shared,
            pending: Mutex::new(Pending::Asked(Instant::now())),
        }
    }

    /// Brings the window back from the tray, to the front. If its thread doesn't take that
    /// within [`SHOW_GRACE`], [`Window::follow_up`] does it from the tray.
    pub fn show(&self) {
        self.shared.show.store(true, Ordering::SeqCst);
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Pending::Asked(Instant::now());
        self.repaint();
    }

    /// Checks on the last [`Window::show`]: the tray calls this as it goes, and tells the user
    /// what [`FollowUp::NotResponding`] and [`FollowUp::NotStarted`] mean. A window whose
    /// thread hasn't taken Open within [`SHOW_GRACE`] is brought forward from here, and must
    /// then show and answer within [`RECOVER_WITHIN`]; one that's gone is marked failed. So
    /// Open never silently does nothing.
    pub fn follow_up(&self, now: Instant) -> FollowUp {
        self.follow_up_with(now, &Win32)
    }

    fn follow_up_with(&self, now: Instant, native: &dyn Native) -> FollowUp {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let hwnd = self.shared.hwnd.load(Ordering::SeqCst);
        let taken = !self.shared.show.load(Ordering::SeqCst);
        match *pending {
            Pending::None => FollowUp::None,
            Pending::Recovering { since, focused } => {
                if taken || native.responds(hwnd) {
                    // Its thread took Open, or the window shows and answers: in a dialog it
                    // owns, which keeps the window's messages moving, and which shows now
                    // the window has, so it can take the focus.
                    if !taken && !focused {
                        native.focus_front(hwnd);
                    }
                    *pending = Pending::None;
                    tracing::info!("the window came forward from the tray");
                    return FollowUp::Recovered;
                }
                if now.duration_since(since) >= RECOVER_WITHIN {
                    *pending = Pending::None;
                    tracing::error!("Open: the window didn't come forward: its thread is stuck");
                    return FollowUp::NotResponding;
                }
                if !focused && now.duration_since(since) >= FOCUS_AGAIN {
                    native.focus_front(hwnd);
                    *pending = Pending::Recovering {
                        since,
                        focused: true,
                    };
                }
                FollowUp::None
            }
            Pending::Asked(at) => {
                if taken {
                    *pending = Pending::None;
                    return FollowUp::None;
                }
                let waited = now.duration_since(at);
                if waited < SHOW_GRACE || self.failed().is_some() {
                    return FollowUp::None;
                }
                if hwnd != 0 && native.exists(hwnd) {
                    tracing::warn!(
                        "the window didn't take Open in time (busy in a dialog?): bringing it forward"
                    );
                    native.bring_forward(hwnd);
                    *pending = Pending::Recovering {
                        since: now,
                        focused: false,
                    };
                    return FollowUp::BroughtForward;
                }
                let starting = hwnd == 0
                    && self
                        .shared
                        .ctx
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_none();
                if starting {
                    // Its first frame takes Open, unless it never comes.
                    if waited < START_WITHIN {
                        return FollowUp::None;
                    }
                    *pending = Pending::None;
                    tracing::error!("Open: the window hasn't started");
                    return FollowUp::NotStarted;
                }
                *pending = Pending::None;
                tracing::error!("Open: the window is gone");
                *self.shared.failed.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(tr("window-gone"));
                FollowUp::Gone
            }
        }
    }

    /// Asks the window to draw itself again (the tray's status changed).
    pub fn repaint(&self) {
        if let Some(ctx) = self
            .shared
            .ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            ctx.request_repaint();
        }
    }

    /// Why the window couldn't start or stopped, if it did.
    pub fn failed(&self) -> Option<String> {
        self.shared
            .failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Runs the window until the process ends.
fn run(backend: Arc<dyn Backend>, shared: Arc<Shared>) -> Result<(), String> {
    let state = WindowState::load(&backend.window_state_file());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(tr("app-name"))
        .with_min_inner_size(MIN_SIZE)
        .with_inner_size(DEFAULT_SIZE)
        .with_visible(true);
    // Where it was; else, the first time, the default size (less on a small screen) in the
    // middle of the work area.
    let placed = state
        .rect
        .filter(|r| r.usable(is_on_screen))
        .or_else(|| work_area().map(|(work, ppp)| first_rect(work, ppp)));
    if let Some(r) = placed {
        viewport = viewport
            .with_position([r.x, r.y])
            .with_inner_size([r.width, r.height]);
    }
    if state.maximized {
        viewport = viewport.with_maximized(true);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: placed.is_none(),
        persist_window: false,
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            // The tray's loop has the main thread.
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    // Why the app couldn't be made, in the user's words (eframe wraps the creator's error in
    // its own English).
    let no_font: Arc<Mutex<Option<String>>> = Arc::default();
    let no_font_seen = Arc::clone(&no_font);
    eframe::run_native(
        "Replica",
        options,
        Box::new(move |cc| {
            if let Err(e) = fonts::install(&cc.egui_ctx) {
                let text = tr!(
                    "window-no-font",
                    dir = e.dir.display().to_string(),
                    files = e.missing.join(", ")
                );
                *no_font_seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
                return Err(text.into());
            }
            style(&cc.egui_ctx);
            *shared.ctx.lock().unwrap_or_else(|e| e.into_inner()) = Some(cc.egui_ctx.clone());
            let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
                _ => None,
            };
            let icons = hwnd.and_then(|hwnd| {
                set_icons(hwnd, APP_ICON)
                    .inspect_err(|e| tracing::warn!(error = %e, "couldn't set the window's icon"))
                    .ok()
            });
            if let Some(hwnd) = hwnd {
                // The file dialogs belong to it, and the tray can reach it while its thread is
                // busy in one.
                backend.set_owner(hwnd);
                shared.hwnd.store(hwnd, Ordering::SeqCst);
            }
            tracing::info!("the window started");
            Ok(Box::new(Shell::new(backend, shared, state, icons)))
        }),
    )
    .map_err(|e| {
        no_font
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| tr!("window-start-failed", error = e.to_string()))
    })
}

/// The app icon at the sizes `build.rs` decodes from `replica.ico`, as RGBA.
const APP_ICON_PIXELS: [(usize, &[u8]); 5] = [
    (
        20,
        include_bytes!(concat!(env!("OUT_DIR"), "/app-icon-20.rgba")),
    ),
    (
        32,
        include_bytes!(concat!(env!("OUT_DIR"), "/app-icon-32.rgba")),
    ),
    (
        40,
        include_bytes!(concat!(env!("OUT_DIR"), "/app-icon-40.rgba")),
    ),
    (
        48,
        include_bytes!(concat!(env!("OUT_DIR"), "/app-icon-48.rgba")),
    ),
    (
        64,
        include_bytes!(concat!(env!("OUT_DIR"), "/app-icon-64.rgba")),
    ),
];

/// The app icon's texture for drawing it `points` wide: the smallest of its sizes that's sharp
/// at the display's scale, made the first time it's asked for.
pub fn app_icon(ctx: &egui::Context, points: f32) -> egui::TextureId {
    let wanted = (points * ctx.pixels_per_point()).ceil() as usize;
    let (size, pixels) = APP_ICON_PIXELS
        .iter()
        .copied()
        .find(|(size, _)| *size >= wanted)
        .unwrap_or(APP_ICON_PIXELS[APP_ICON_PIXELS.len() - 1]);
    let key = egui::Id::unique(("app-icon", size));
    if let Some(texture) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return texture.id();
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([size, size], pixels);
    let texture = ctx.load_texture(
        format!("app-icon-{size}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    let id = texture.id();
    ctx.data_mut(|d| d.insert_temp(key, texture));
    id
}

/// The window's look in both of Windows' modes ([`theme`]), once the fonts are installed.
pub fn style(ctx: &egui::Context) {
    theme::install(ctx);
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A window as the tray's Win32 would find it: whether it exists, and whether it shows
    /// and answers once brought forward (its thread in a dialog) or never (stuck).
    #[derive(Default)]
    struct Fake {
        exists: bool,
        answers_once_brought: bool,
        brought: Cell<u32>,
        focused: Cell<u32>,
    }

    impl Native for Fake {
        fn exists(&self, _: isize) -> bool {
            self.exists
        }
        fn bring_forward(&self, _: isize) {
            self.brought.set(self.brought.get() + 1);
        }
        fn focus_front(&self, _: isize) {
            self.focused.set(self.focused.get() + 1);
        }
        fn responds(&self, _: isize) -> bool {
            self.answers_once_brought && self.brought.get() > 0
        }
    }

    /// The tray's handle on a window that isn't running, asked to show at `asked` and not
    /// taking it: `hwnd` is 0 if it never got as far as a window, `ctx` whether it got a
    /// context.
    fn window(hwnd: isize, ctx: bool, asked: Instant) -> Window {
        Window {
            shared: Arc::new(Shared {
                show: AtomicBool::new(true),
                hwnd: AtomicIsize::new(hwnd),
                ctx: Mutex::new(ctx.then(egui::Context::default)),
                ..Shared::default()
            }),
            pending: Mutex::new(Pending::Asked(asked)),
        }
    }

    #[test]
    fn open_taken_by_the_window_needs_nothing_more() {
        let at = Instant::now();
        let w = window(7, true, at);
        w.shared.show.store(false, Ordering::SeqCst);
        let fake = Fake::default();
        assert_eq!(w.follow_up_with(at + SHOW_GRACE * 2, &fake), FollowUp::None);
        assert_eq!(fake.brought.get(), 0);
        assert!(w.failed().is_none());
    }

    #[test]
    fn a_window_busy_in_its_dialog_is_brought_forward_and_comes() {
        let at = Instant::now();
        let w = window(7, true, at);
        let fake = Fake {
            exists: true,
            answers_once_brought: true,
            ..Fake::default()
        };
        assert_eq!(
            w.follow_up_with(at, &fake),
            FollowUp::None,
            "within its grace"
        );
        assert_eq!(fake.brought.get(), 0);
        let brought = at + SHOW_GRACE;
        assert_eq!(w.follow_up_with(brought, &fake), FollowUp::BroughtForward);
        assert_eq!(w.follow_up_with(brought, &fake), FollowUp::Recovered);
        assert_eq!(fake.focused.get(), 1, "its dialog focused, now it shows");
        assert_eq!(
            w.follow_up_with(brought + RECOVER_WITHIN * 2, &fake),
            FollowUp::None,
            "nothing more to say"
        );
    }

    #[test]
    fn a_window_whose_thread_is_stuck_is_said_not_to_respond_every_time() {
        let at = Instant::now();
        let w = window(7, true, at);
        let fake = Fake {
            exists: true,
            ..Fake::default()
        };
        let brought = at + SHOW_GRACE;
        assert_eq!(w.follow_up_with(brought, &fake), FollowUp::BroughtForward);
        assert_eq!(
            w.follow_up_with(brought + FOCUS_AGAIN, &fake),
            FollowUp::None
        );
        assert_eq!(fake.focused.get(), 1, "its dialog focused once it's shown");
        assert_eq!(
            w.follow_up_with(brought + RECOVER_WITHIN, &fake),
            FollowUp::NotResponding
        );
        assert_eq!(
            w.follow_up_with(brought + RECOVER_WITHIN * 2, &fake),
            FollowUp::None,
            "once"
        );
        assert!(w.failed().is_none(), "it may still come back");

        // The next Open tries again, and says so again.
        w.show();
        let again = Instant::now() + SHOW_GRACE;
        assert_eq!(w.follow_up_with(again, &fake), FollowUp::BroughtForward);
        assert_eq!(
            w.follow_up_with(again + RECOVER_WITHIN, &fake),
            FollowUp::NotResponding
        );
    }

    #[test]
    fn a_window_that_never_starts_is_said_not_to_have() {
        let at = Instant::now();
        let w = window(0, false, at);
        let fake = Fake::default();
        assert_eq!(
            w.follow_up_with(at + SHOW_GRACE, &fake),
            FollowUp::None,
            "starting"
        );
        assert_eq!(
            w.follow_up_with(at + START_WITHIN - Duration::from_millis(1), &fake),
            FollowUp::None
        );
        assert_eq!(
            w.follow_up_with(at + START_WITHIN, &fake),
            FollowUp::NotStarted
        );
        assert_eq!(
            w.follow_up_with(at + START_WITHIN * 2, &fake),
            FollowUp::None,
            "once"
        );
    }

    #[test]
    fn open_on_a_window_that_s_gone_says_so_rather_than_doing_nothing() {
        let at = Instant::now();
        let w = window(7, true, at);
        let fake = Fake::default();
        assert_eq!(w.follow_up_with(at + SHOW_GRACE, &fake), FollowUp::Gone);
        assert_eq!(w.failed(), Some(tr("window-gone")));
        assert_eq!(
            w.follow_up_with(at + SHOW_GRACE * 2, &fake),
            FollowUp::None,
            "once"
        );
    }
}
