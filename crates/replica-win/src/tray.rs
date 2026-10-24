//! The tray scaffold: an icon in the notification area with light- and dark-taskbar variants,
//! a tooltip, balloons, a right-click menu, one instance per user session, and the message
//! loop that drives them.
//!
//! Self-contained so other apps can copy it: nothing here is Replica's. The app
//! gives it `.ico` files and menu items, and gets [`TrayEvent`]s back from [`Tray::pump`].
//!
//! It owns its notify icon (`Shell_NotifyIconW`) rather than using the `tray-icon` crate, so
//! that balloons, which `tray-icon` doesn't offer, come from the same icon in every mode,
//! portable included (Windows toasts need an AppUserModelID that a portable copy lacks).

use std::io;
use std::time::Duration;

use crate::win32::registry;
use crate::win32::ui::{self, Icon, InstanceLock, NamedEvent, NotifyIcon, UiEvent};
pub use crate::win32::ui::{BalloonKind, MenuItem};

/// Where Windows records whether the taskbar is light.
const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Whether the taskbar is light (so a dark glyph shows on it). Dark when Windows doesn't say,
/// as it is by default.
pub fn taskbar_is_light() -> bool {
    registry::get_dword(PERSONALIZE, "SystemUsesLightTheme")
        .ok()
        .flatten()
        .is_some_and(|v| v != 0)
}

/// Makes the process per-monitor DPI aware. Call it first thing.
pub fn set_dpi_aware() {
    ui::set_per_monitor_dpi_aware();
}

/// The images of a `.ico` file, to pick a size from.
#[derive(Debug, Clone, Copy)]
pub struct IconFile(pub &'static [u8]);

impl IconFile {
    /// Each image's size and data (a PNG or a DIB, as stored).
    pub fn images(&self) -> Vec<(u32, &'static [u8])> {
        let bytes = self.0;
        let read16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let read32 = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        if bytes.len() < 6 || read16(0) != 0 || read16(2) != 1 {
            return Vec::new();
        }
        (0..read16(4) as usize)
            .filter_map(|i| {
                let entry = 6 + i * 16;
                if entry + 16 > bytes.len() {
                    return None;
                }
                let size = match bytes[entry] {
                    0 => 256,
                    n => u32::from(n),
                };
                let (length, offset) = (read32(entry + 8) as usize, read32(entry + 12) as usize);
                bytes
                    .get(offset..offset.checked_add(length)?)
                    .map(|image| (size, image))
            })
            .collect()
    }

    /// The image for `size` pixels: that size, else the next bigger, else the biggest.
    pub fn image_for(&self, size: u32) -> Option<(u32, &'static [u8])> {
        let mut images = self.images();
        images.sort_by_key(|(s, _)| *s);
        images
            .iter()
            .find(|(s, _)| *s >= size)
            .or(images.last())
            .copied()
    }
}

/// One icon in two variants: for a light taskbar (a dark glyph) and for a dark one.
#[derive(Debug, Clone, Copy)]
pub struct ThemedIcon {
    pub light_taskbar: IconFile,
    pub dark_taskbar: IconFile,
}

impl ThemedIcon {
    fn pick(&self, light: bool) -> IconFile {
        if light {
            self.light_taskbar
        } else {
            self.dark_taskbar
        }
    }
}

/// What happened, for the app to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// A click on the icon: usually "open the window".
    Activate,
    /// A right-click: show the menu here ([`Tray::show_menu`]).
    Menu { x: i32, y: i32 },
    /// A click on a balloon.
    BalloonClicked,
    /// Another launch of the app asked this one to show itself.
    ShowRequested,
    /// Another process asked this one to quit ([`request_quit`]): an uninstall or an upgrade.
    QuitRequested,
}

/// The app's icon in the notification area. It lives on the thread that made it.
pub struct Tray {
    icon: NotifyIcon,
    current: ThemedIcon,
    light: bool,
    size: u32,
}

fn make_icon(icon: ThemedIcon, light: bool, size: u32) -> io::Result<(Icon, u32)> {
    let (actual, image) = icon
        .pick(light)
        .image_for(size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "an empty .ico"))?;
    Ok((Icon::from_ico_image(image, actual.min(size) as i32)?, size))
}

impl Tray {
    /// Shows `icon` with the tooltip `tip`.
    pub fn new(icon: ThemedIcon, tip: &str) -> io::Result<Self> {
        let light = taskbar_is_light();
        // A first guess at the size; corrected once the window knows its monitor.
        let (first, _) = make_icon(icon, light, 16)?;
        let notify = NotifyIcon::new(first, tip)?;
        let mut tray = Tray {
            size: 0,
            icon: notify,
            current: icon,
            light,
        };
        tray.refresh()?;
        Ok(tray)
    }

    /// Shows another icon (a new state).
    pub fn set_icon(&mut self, icon: ThemedIcon) -> io::Result<()> {
        self.current = icon;
        self.size = 0;
        self.refresh()
    }

    /// Follows a change of taskbar theme or of DPI. [`Tray::pump`] calls it.
    pub fn refresh(&mut self) -> io::Result<()> {
        let light = taskbar_is_light();
        let size = self.icon.icon_size().max(16) as u32;
        if light != self.light || size != self.size {
            let (icon, size) = make_icon(self.current, light, size)?;
            self.icon.set_icon(icon)?;
            self.light = light;
            self.size = size;
        }
        Ok(())
    }

    pub fn set_tooltip(&mut self, tip: &str) -> io::Result<()> {
        self.icon.set_tip(tip)
    }

    pub fn balloon(&self, title: &str, body: &str, kind: BalloonKind) -> io::Result<()> {
        self.icon.balloon(title, body, kind)
    }

    /// Shows a menu at `(x, y)` and returns the chosen command's ID.
    pub fn show_menu(&self, items: &[MenuItem], x: i32, y: i32) -> io::Result<Option<u32>> {
        self.icon.show_menu(items, x, y)
    }

    /// Runs the message loop for up to `timeout` and returns what happened. `instance`'s show
    /// requests come out as [`TrayEvent::ShowRequested`]. The icon comes back by itself after
    /// Explorer restarts, and follows the taskbar's theme.
    pub fn pump(&mut self, timeout: Duration, instance: Option<&SingleInstance>) -> Vec<TrayEvent> {
        let wakes: Vec<&NamedEvent> = instance.map(|i| vec![&i.wake, &i.quit]).unwrap_or_default();
        let (events, woke) = ui::pump(timeout, &wakes);
        let mut out = Vec::new();
        for event in events {
            match event {
                UiEvent::Activate => out.push(TrayEvent::Activate),
                UiEvent::Menu { x, y } => out.push(TrayEvent::Menu { x, y }),
                UiEvent::BalloonClicked => out.push(TrayEvent::BalloonClicked),
                UiEvent::TaskbarCreated => {
                    if let Err(error) = self.icon.add() {
                        tracing::warn!(%error, "couldn't add the tray icon again");
                    }
                }
            }
        }
        match woke {
            Some(0) => out.push(TrayEvent::ShowRequested),
            Some(_) => out.push(TrayEvent::QuitRequested),
            None => {}
        }
        if let Err(error) = self.refresh() {
            tracing::warn!(%error, "couldn't update the tray icon");
        }
        out
    }
}

/// One instance of the app per user session.
pub struct SingleInstance {
    _lock: InstanceLock,
    wake: NamedEvent,
    quit: NamedEvent,
}

/// What [`SingleInstance::acquire`] found.
pub enum Instance {
    /// This is the only one.
    First(SingleInstance),
    /// Another was running; it has been asked to show itself.
    Second,
}

impl SingleInstance {
    /// Becomes the one instance of `app` (a name such as `Replica.Tray`) in this session, or,
    /// if there already is one, asks it to show itself.
    pub fn acquire(app: &str) -> io::Result<Instance> {
        let event = format!(r"Local\{app}.Show");
        match InstanceLock::acquire(&format!(r"Local\{app}.Instance"))? {
            Some(lock) => Ok(Instance::First(SingleInstance {
                _lock: lock,
                wake: NamedEvent::create(&event)?,
                quit: NamedEvent::create(&format!(r"Local\{app}.Quit"))?,
            })),
            None => {
                // The first instance may still be starting: its event comes a moment after its
                // lock.
                for _ in 0..50 {
                    if NamedEvent::signal(&event).is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(Instance::Second)
            }
        }
    }
}

/// Asks the running instance of `app`, if there is one, to quit, and waits up to `wait` for
/// it to end. Returns whether none is running now (none was, or it ended).
pub fn request_quit(app: &str, wait: Duration) -> io::Result<bool> {
    let lock = format!(r"Local\{app}.Instance");
    if let Some(free) = InstanceLock::acquire(&lock)? {
        drop(free);
        return Ok(true);
    }
    // Not there yet means it's still starting: it makes its events a moment after its lock.
    let until = std::time::Instant::now() + wait;
    let mut asked = false;
    while std::time::Instant::now() < until {
        if !asked {
            asked = NamedEvent::signal(&format!(r"Local\{app}.Quit")).is_ok();
        }
        if let Some(free) = InstanceLock::acquire(&lock)? {
            drop(free);
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAY_ICO: &[u8] = include_bytes!("../../../assets/tray/idle-on-dark.ico");
    const APP_ICO: &[u8] = include_bytes!("../../../assets/icon/replica.ico");

    #[test]
    fn reads_the_sizes_in_an_ico_file() {
        let sizes: Vec<u32> = IconFile(TRAY_ICO).images().iter().map(|i| i.0).collect();
        assert_eq!(sizes, [16, 20, 24, 32]);
        let app: Vec<u32> = IconFile(APP_ICO).images().iter().map(|i| i.0).collect();
        assert_eq!(app, [16, 20, 24, 32, 40, 48, 64, 256]);
    }

    #[test]
    fn picks_that_size_then_bigger_then_the_biggest() {
        let file = IconFile(TRAY_ICO);
        assert_eq!(file.image_for(20).unwrap().0, 20);
        assert_eq!(file.image_for(18).unwrap().0, 20);
        assert_eq!(file.image_for(40).unwrap().0, 32);
        assert!(IconFile(b"not an icon").image_for(16).is_none());
    }

    #[test]
    fn every_image_makes_an_icon() {
        for (size, image) in IconFile(TRAY_ICO).images() {
            Icon::from_ico_image(image, size as i32).unwrap();
        }
    }

    #[test]
    fn a_second_instance_wakes_the_first() {
        let app = format!("ReplicaTest-{}-tray", std::process::id());
        let Instance::First(first) = SingleInstance::acquire(&app).unwrap() else {
            panic!("the first one")
        };
        let second = std::thread::spawn({
            let app = app.clone();
            move || matches!(SingleInstance::acquire(&app).unwrap(), Instance::Second)
        });
        assert!(second.join().unwrap());
        let (_, woke) = ui::pump(Duration::from_secs(5), &[&first.wake, &first.quit]);
        assert_eq!(woke, Some(0));
        drop(first);
        assert!(matches!(
            SingleInstance::acquire(&app).unwrap(),
            Instance::First(_)
        ));
    }

    #[test]
    fn a_quit_request_reaches_the_instance_and_waits_for_it_to_end() {
        let app = format!("ReplicaTest-{}-quit", std::process::id());
        // None running: nothing to wait for.
        assert!(request_quit(&app, Duration::from_secs(1)).unwrap());

        let Instance::First(first) = SingleInstance::acquire(&app).unwrap() else {
            panic!("the first one")
        };
        let asker = std::thread::spawn({
            let app = app.clone();
            move || request_quit(&app, Duration::from_secs(20)).unwrap()
        });
        let (_, woke) = ui::pump(Duration::from_secs(10), &[&first.wake, &first.quit]);
        assert_eq!(woke, Some(1), "the quit event");
        drop(first);
        assert!(asker.join().unwrap(), "it saw the instance end");
    }
}
