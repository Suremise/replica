//! The tray's Win32 pieces: a hidden window that owns a notification-area icon, its balloons
//! and its popup menu, the message loop, single-instance objects and DPI awareness.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::marker::PhantomData;
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, RECT,
    WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, SetEvent,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, GetSystemMetricsForDpi,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_ERROR, NIIF_INFO,
    NIIF_RESPECT_QUIET_TIME, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFY_ICON_DATA_FLAGS, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, HICON, HMENU, ICON_BIG, ICON_SMALL,
    LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, PostMessageW,
    QS_ALLINPUT, RegisterClassW, RegisterWindowMessageW, SM_CXICON, SM_CXSMICON, SendMessageW,
    SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CONTEXTMENU, WM_NULL, WM_SETICON, WM_USER,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

use super::io_error;

/// The notify icon's callback message.
const CALLBACK: u32 = WM_APP + 1;
/// With `NOTIFYICON_VERSION_4`: a click or Enter on the icon, and a click on a balloon.
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

/// What the user did with the icon, or what Windows told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    /// A click (or Enter) on the icon.
    Activate,
    /// A right-click (or the menu key) on the icon, at these screen coordinates.
    Menu { x: i32, y: i32 },
    /// A click on a balloon.
    BalloonClicked,
    /// Explorer restarted: the icon has to be added again.
    TaskbarCreated,
}

thread_local! {
    static EVENTS: RefCell<VecDeque<UiEvent>> = const { RefCell::new(VecDeque::new()) };
}

fn push(event: UiEvent) {
    EVENTS.with(|e| e.borrow_mut().push_back(event));
}

fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    // SAFETY: a static, NUL-terminated name.
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == CALLBACK {
        match (lparam.0 as u32) & 0xFFFF {
            NIN_SELECT | NIN_KEYSELECT => push(UiEvent::Activate),
            WM_CONTEXTMENU => push(UiEvent::Menu {
                x: (wparam.0 & 0xFFFF) as u16 as i16 as i32,
                y: ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
            }),
            NIN_BALLOONUSERCLICK => push(UiEvent::BalloonClicked),
            _ => {}
        }
        return LRESULT(0);
    }
    if msg == taskbar_created() {
        push(UiEvent::TaskbarCreated);
    }
    // SAFETY: passes the message on unchanged to the default handler.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn window_class() -> io::Result<PCWSTR> {
    static REGISTERED: OnceLock<Result<(), i32>> = OnceLock::new();
    let class = w!("Replica.NotifyWindow");
    let result = REGISTERED.get_or_init(|| {
        // SAFETY: the class name is static and the window procedure lives for the program.
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.code().0)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                Err(GetLastError().0 as i32)
            } else {
                Ok(())
            }
        }
    });
    match result {
        Ok(()) => Ok(class),
        Err(code) => Err(io::Error::from_raw_os_error(*code)),
    }
}

/// Makes the process per-monitor DPI aware (v2), so icons and menus are sharp on every
/// monitor. Call it before any window is made.
pub fn set_per_monitor_dpi_aware() {
    // SAFETY: a process-wide setting; it fails harmlessly if a manifest set it already.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Copies `text` into a fixed-size wide buffer, cut short to fit, NUL-terminated.
fn fill(buffer: &mut [u16], text: &str) {
    let max = buffer.len() - 1;
    let mut n = 0;
    for unit in text.encode_utf16().take(max) {
        buffer[n] = unit;
        n += 1;
    }
    buffer[n] = 0;
}

/// An icon made from one image of a `.ico` file (a PNG or a DIB, as stored there).
pub struct Icon(HICON);

impl Icon {
    pub fn from_ico_image(image: &[u8], size: i32) -> io::Result<Self> {
        // SAFETY: Windows reads `image` (a live slice) and makes its own icon from it.
        unsafe { CreateIconFromResourceEx(image, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR) }
            .map(Icon)
            .map_err(|e| io_error(&e))
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: the icon was made by CreateIconFromResourceEx and is destroyed once.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

/// The kind of a balloon, which picks its little icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalloonKind {
    Info,
    Warning,
    Error,
}

/// A popup-menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItem {
    Command {
        id: u32,
        text: String,
        checked: bool,
        enabled: bool,
    },
    Submenu {
        text: String,
        items: Vec<MenuItem>,
        enabled: bool,
    },
    Separator,
}

/// A hidden window with an icon in the notification area. It lives on the thread that made it
/// (so it isn't `Send`), and its events come out of [`pump`] on that thread.
pub struct NotifyIcon {
    hwnd: HWND,
    id: u32,
    icon: Icon,
    tip: String,
    _not_send: PhantomData<*const ()>,
}

impl NotifyIcon {
    /// Adds an icon showing `icon`, with the tooltip `tip`.
    pub fn new(icon: Icon, tip: &str) -> io::Result<Self> {
        let class = window_class()?;
        // SAFETY: a hidden tool window of our registered class; it's destroyed in Drop.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0),
                class,
                w!("Replica"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
        }
        .map_err(|e| io_error(&e))?;
        let this = NotifyIcon {
            hwnd,
            id: 1,
            icon,
            tip: tip.to_owned(),
            _not_send: PhantomData,
        };
        this.add()?;
        Ok(this)
    }

    fn data(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: self.id,
            uFlags: flags,
            uCallbackMessage: CALLBACK,
            hIcon: self.icon.0,
            ..Default::default()
        };
        fill(&mut data.szTip, &self.tip);
        data
    }

    fn notify(
        &self,
        message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE,
        data: &NOTIFYICONDATAW,
    ) -> io::Result<()> {
        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW for our window.
        if unsafe { Shell_NotifyIconW(message, data) }.as_bool() {
            Ok(())
        } else {
            Err(io::Error::other("Shell_NotifyIcon failed"))
        }
    }

    /// Adds the icon (again, after Explorer restarts: [`UiEvent::TaskbarCreated`]).
    pub fn add(&self) -> io::Result<()> {
        let mut data = self.data(NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        self.notify(NIM_ADD, &data)?;
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        self.notify(NIM_SETVERSION, &data)
    }

    pub fn set_icon(&mut self, icon: Icon) -> io::Result<()> {
        self.icon = icon;
        self.notify(NIM_MODIFY, &self.data(NIF_ICON))
    }

    pub fn set_tip(&mut self, tip: &str) -> io::Result<()> {
        if self.tip == tip {
            return Ok(());
        }
        self.tip = tip.to_owned();
        self.notify(NIM_MODIFY, &self.data(NIF_TIP | NIF_SHOWTIP))
    }

    /// Shows a balloon from the icon. Windows shows it as a notification, and keeps quiet
    /// during "do not disturb".
    pub fn balloon(&self, title: &str, body: &str, kind: BalloonKind) -> io::Result<()> {
        let mut data = self.data(NIF_INFO);
        fill(&mut data.szInfoTitle, title);
        fill(&mut data.szInfo, body);
        data.dwInfoFlags = NIIF_RESPECT_QUIET_TIME
            | match kind {
                BalloonKind::Info => NIIF_INFO,
                BalloonKind::Warning => NIIF_WARNING,
                BalloonKind::Error => NIIF_ERROR,
            };
        self.notify(NIM_MODIFY, &data)
    }

    /// The small-icon size for the icon's monitor, in pixels.
    pub fn icon_size(&self) -> i32 {
        // SAFETY: a query about our own live window.
        unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(self.hwnd).max(96)) }
    }

    /// Shows `items` as a popup menu at the screen point `(x, y)` and returns the chosen
    /// command's ID, or `None` if the menu was dismissed.
    pub fn show_menu(&self, items: &[MenuItem], x: i32, y: i32) -> io::Result<Option<u32>> {
        let menu = build_menu(items)?;
        // SAFETY: the menu is ours and destroyed below (its submenus with it). The window must
        // be foreground for the menu to close when the user clicks elsewhere, and the WM_NULL
        // afterwards is the documented companion to that.
        let chosen = unsafe {
            let _ = SetForegroundWindow(self.hwnd);
            let chosen = TrackPopupMenuEx(
                menu,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0,
                x,
                y,
                self.hwnd,
                None,
            );
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            chosen.0
        };
        Ok((chosen > 0).then_some(chosen as u32))
    }
}

fn build_menu(items: &[MenuItem]) -> io::Result<HMENU> {
    // SAFETY: a new, empty popup menu; the caller destroys it, which destroys the submenus
    // attached here too.
    let menu = unsafe { CreatePopupMenu() }.map_err(|e| io_error(&e))?;
    for item in items {
        // SAFETY: the text is an HSTRING that outlives each call; submenus are attached to
        // `menu`, which then owns them.
        let result = unsafe {
            match item {
                MenuItem::Command {
                    id,
                    text,
                    checked,
                    enabled,
                } => {
                    let mut flags = MF_STRING;
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    AppendMenuW(menu, flags, *id as usize, &HSTRING::from(text.as_str()))
                }
                MenuItem::Submenu {
                    text,
                    items,
                    enabled,
                } => {
                    let sub = build_menu(items)?;
                    let mut flags = MF_POPUP | MF_STRING;
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    AppendMenuW(menu, flags, sub.0 as usize, &HSTRING::from(text.as_str()))
                }
                MenuItem::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()),
            }
        };
        if let Err(e) = result {
            // SAFETY: destroys the half-built menu once.
            let _ = unsafe { DestroyMenu(menu) };
            return Err(io_error(&e));
        }
    }
    Ok(menu)
}

impl Drop for NotifyIcon {
    fn drop(&mut self) {
        let data = self.data(NOTIFY_ICON_DATA_FLAGS(0));
        let _ = self.notify(NIM_DELETE, &data);
        // SAFETY: our window, destroyed once on its own thread.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// Runs this thread's messages for up to `timeout`, or until `wake` is set, and returns the
/// icon events that came in and whether `wake` was set.
/// Returns the window events, and which of `wakes` was set, if one was.
pub fn pump(timeout: Duration, wakes: &[&NamedEvent]) -> (Vec<UiEvent>, Option<usize>) {
    let handles: Vec<HANDLE> = wakes.iter().map(|w| w.0).collect();
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: the handles are live for the call; a message or the event ends the wait.
    let waited = unsafe {
        MsgWaitForMultipleObjectsEx(
            (!handles.is_empty()).then_some(handles.as_slice()),
            millis,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        )
    };
    // WAIT_OBJECT_0 + i: the i-th handle was set (WAIT_OBJECT_0 is 0).
    let woke = Some(waited.0.wrapping_sub(WAIT_OBJECT_0.0) as usize).filter(|i| *i < handles.len());
    let mut msg = MSG::default();
    // SAFETY: `msg` is a live MSG; each message is translated and dispatched on this thread.
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let events = EVENTS.with(|e| e.borrow_mut().drain(..).collect());
    (events, woke)
}

/// A named, auto-reset event: one process waits on it ([`pump`]), others set it.
pub struct NamedEvent(HANDLE);

// SAFETY: an event handle may be used from any thread.
unsafe impl Send for NamedEvent {}
unsafe impl Sync for NamedEvent {}

impl NamedEvent {
    /// Creates (or opens) the event `name`.
    pub fn create(name: &str) -> io::Result<Self> {
        // SAFETY: the name outlives the call; default security.
        unsafe { CreateEventW(None, false, false, &HSTRING::from(name)) }
            .map(NamedEvent)
            .map_err(|e| io_error(&e))
    }

    /// Sets the existing event `name`.
    pub fn signal(name: &str) -> io::Result<()> {
        // SAFETY: the handle is closed straight after use.
        unsafe {
            let handle = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name))
                .map_err(|e| io_error(&e))?;
            let result = SetEvent(handle).map_err(|e| io_error(&e));
            let _ = CloseHandle(handle);
            result
        }
    }
}

impl Drop for NamedEvent {
    fn drop(&mut self) {
        // SAFETY: closes our handle once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Proof that this is the only instance: a named mutex held while it lives.
pub struct InstanceLock(HANDLE);

impl InstanceLock {
    /// Takes the mutex `name`, or returns `None` if another process has it.
    pub fn acquire(name: &str) -> io::Result<Option<Self>> {
        // SAFETY: the name outlives the call; GetLastError is read straight after it.
        unsafe {
            let handle =
                CreateMutexW(None, true, &HSTRING::from(name)).map_err(|e| io_error(&e))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Ok(None);
            }
            Ok(Some(InstanceLock(handle)))
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // SAFETY: closing the handle releases the mutex (owned since creation).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

// --- An app's own window ------------------------------------------------------------------------

/// The size in pixels of a window's big (`small == false`) or small icon on its monitor.
pub fn window_icon_size(hwnd: isize, small: bool) -> i32 {
    let metric = if small { SM_CXSMICON } else { SM_CXICON };
    // SAFETY: a query about a window handle; a stale one gives the default DPI.
    unsafe { GetSystemMetricsForDpi(metric, GetDpiForWindow(HWND(hwnd as _)).max(96)) }
}

/// Sets a window's big and small icons. The caller keeps both alive while the window is.
pub fn set_window_icons(hwnd: isize, big: &Icon, small: &Icon) {
    let hwnd = HWND(hwnd as _);
    // SAFETY: WM_SETICON takes the icon handles, which outlive the window (the caller's job).
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(big.0.0 as isize)),
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0.0 as isize)),
        );
    }
}

/// A window, by its `HWND`, as the owner of a dialog (such as a file dialog).
pub struct DialogOwner(std::num::NonZeroIsize);

impl DialogOwner {
    /// The window `hwnd`, which must live while the owner is used; `None` for 0.
    pub fn new(hwnd: isize) -> Option<Self> {
        std::num::NonZeroIsize::new(hwnd).map(Self)
    }
}

impl raw_window_handle::HasWindowHandle for DialogOwner {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let raw = raw_window_handle::RawWindowHandle::Win32(
            raw_window_handle::Win32WindowHandle::new(self.0),
        );
        // SAFETY: the window outlives the owner (`new`'s contract); a stale handle only makes
        // the dialog unowned.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

impl raw_window_handle::HasDisplayHandle for DialogOwner {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::windows())
    }
}

/// Whether `hwnd` is a window that still exists.
pub fn window_exists(hwnd: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;
    // SAFETY: a query about a window handle; a stale or zero one is false.
    hwnd != 0 && unsafe { IsWindow(Some(HWND(hwnd as _))) }.as_bool()
}

/// Shows and restores another thread's window without waiting on that thread, then focuses
/// it or the modal dialog it owns ([`focus_front`]).
pub fn bring_forward(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SW_SHOW, ShowWindowAsync};
    let hwnd = HWND(hwnd as _);
    // SAFETY: plain calls on a window handle; the async ones only post to its thread, so a
    // busy thread can't block the caller.
    unsafe {
        let _ = ShowWindowAsync(hwnd, SW_SHOW);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
    focus_front(hwnd.0 as isize);
}

/// Whether another thread's window shows (visible, not minimised) and its thread answers a
/// message within `timeout`: it's pumping messages, itself or in a modal dialog's loop. One
/// whose thread is stuck can't answer, and nothing posted to it takes effect.
pub fn window_responds(hwnd: isize, timeout: Duration) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindowVisible, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW,
    };
    let hwnd = HWND(hwnd as _);
    // SAFETY: queries and a WM_NULL (which does nothing) to a window handle, bounded by the
    // timeout; a stale handle fails.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        let answered = SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            timeout.as_millis().min(u32::MAX as u128) as u32,
            None,
        );
        answered.0 != 0
    }
}

/// The top-level windows of process `pid`: (`HWND`, title, visible).
pub fn process_windows(pid: u32) -> Vec<(isize, String, bool)> {
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::BOOL;
    struct Found {
        pid: u32,
        windows: Vec<(isize, String, bool)>,
    }
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        // SAFETY: `found` is the `Found` below, alive for the whole enumeration.
        let found = unsafe { &mut *(found.0 as *mut Found) };
        let mut pid = 0;
        // SAFETY: queries about a window handle Windows just gave; `pid` and `text` are live.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == found.pid {
                let mut text = [0u16; 256];
                let len = GetWindowTextW(hwnd, &mut text).max(0) as usize;
                found.windows.push((
                    hwnd.0 as isize,
                    String::from_utf16_lossy(&text[..len]),
                    IsWindowVisible(hwnd).as_bool(),
                ));
            }
        }
        true.into()
    }
    let mut found = Found {
        pid,
        windows: Vec::new(),
    };
    // SAFETY: the callback only touches `found`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut found as *mut Found as isize)) };
    found.windows
}

/// Asks a window to close, as its close button does (`WM_CLOSE`, posted to it alone).
pub fn post_close(hwnd: isize) -> io::Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;
    // SAFETY: posts a message to one window handle.
    unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0))
}

/// Gives the focus to a window, or to the modal dialog it owns if one is open (enabled while
/// the window isn't). A dialog only shows once its owner has restored, so after
/// [`bring_forward`] this is worth calling again a moment later.
pub fn focus_front(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{GW_ENABLEDPOPUP, GetWindow};
    let hwnd = HWND(hwnd as _);
    // SAFETY: plain calls on a window handle; a stale one fails harmlessly.
    unsafe {
        let front = GetWindow(hwnd, GW_ENABLEDPOPUP).unwrap_or(hwnd);
        let _ = SetForegroundWindow(front);
    }
}

/// Whether any part of a rectangle (screen pixels: left, top, right, bottom) is on a monitor.
pub fn rect_on_screen(rect: [i32; 4]) -> bool {
    let [left, top, right, bottom] = rect;
    let rect = RECT {
        left,
        top,
        right,
        bottom,
    };
    // SAFETY: `rect` is a live RECT; the call only reads it.
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// The work area (left, top, right, bottom in pixels: the monitor less the taskbar) of the
/// monitor the pointer is on, or else the primary one, and its scale (1.0 at 96 DPI).
pub fn work_area() -> Option<([i32; 4], f32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    // SAFETY: `point` is a live POINT the call writes; on failure it stays at the origin,
    // which is on the primary monitor.
    let _ = unsafe { GetCursorPos(&mut point) };
    // SAFETY: plain values in; never null with MONITOR_DEFAULTTOPRIMARY.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a live MONITORINFO with its size set, as the call requires.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let (mut dpi_x, mut dpi_y) = (96, 96);
    // SAFETY: both out-pointers are live u32s.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    let r = info.rcWork;
    Some((
        [r.left, r.top, r.right, r.bottom],
        dpi_x.max(1) as f32 / 96.0,
    ))
}

/// Opens a file or folder with its default program, as a double-click in Explorer does.
pub fn shell_open(path: &str) -> io::Result<()> {
    // SAFETY: the strings outlive the call; nothing else is passed.
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    // Above 32 is success; below, an error code.
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Sender, channel};
    use std::thread::JoinHandle;

    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, PostThreadMessageW, SW_SHOWNOACTIVATE, ShowWindow, WINDOW_STYLE, WM_QUIT,
        WS_EX_NOACTIVATE, WS_POPUP,
    };

    use super::*;

    /// What a test window's thread does once its window is made.
    #[derive(Clone, Copy)]
    enum Then {
        /// Answers its messages until told to quit.
        Pump,
        /// Waits on a channel, answering nothing: a stuck thread.
        Block,
    }

    /// A small window of this test's own, off every screen and never activated, on a thread
    /// of its own. Dropping it, at the end or in a failed assertion's unwinding, lets that
    /// thread go and waits for it, which destroys the window.
    struct TestWindow {
        hwnd: isize,
        thread_id: u32,
        /// Dropped to let a blocked thread go.
        release: Option<Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestWindow {
        /// Shown or not, its thread doing `then`.
        fn new(shown: bool, then: Then) -> TestWindow {
            let (made, hwnd) = channel();
            let (release, wait) = channel::<()>();
            let thread = std::thread::spawn(move || {
                // SAFETY: a STATIC window owned by this thread, destroyed on it; plain calls.
                unsafe {
                    let hwnd = CreateWindowExW(
                        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                        w!("STATIC"),
                        w!("replica-test"),
                        WINDOW_STYLE(WS_POPUP.0),
                        -32000,
                        -32000,
                        10,
                        10,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                    if shown {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                    made.send((hwnd.0 as isize, GetCurrentThreadId())).unwrap();
                    match then {
                        Then::Pump => {
                            let mut msg = MSG::default();
                            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                        // Until released, or the sender is dropped.
                        Then::Block => {
                            let _ = wait.recv();
                        }
                    }
                    let _ = DestroyWindow(hwnd);
                }
            });
            let (hwnd, thread_id) = hwnd.recv().unwrap();
            TestWindow {
                hwnd,
                thread_id,
                release: Some(release),
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            self.release.take();
            // SAFETY: posts WM_QUIT to this window's own thread; a thread that has ended
            // already makes it fail harmlessly.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn a_window_responds_only_shown_and_with_its_thread_answering() {
        let timeout = Duration::from_millis(200);

        let pumping = TestWindow::new(true, Then::Pump);
        let hwnd = pumping.hwnd;
        assert!(window_responds(hwnd, timeout), "shown, its thread pumping");
        drop(pumping);
        assert!(!window_responds(hwnd, timeout), "gone");

        let hidden = TestWindow::new(false, Then::Pump);
        assert!(!window_responds(hidden.hwnd, timeout), "hidden");

        // Stuck: shown, but nothing it's sent is answered, so it times out.
        let stuck = TestWindow::new(true, Then::Block);
        assert!(!window_responds(stuck.hwnd, timeout), "its thread stuck");
    }

    #[test]
    fn a_hidden_window_with_a_stuck_thread_stays_hidden_when_shown_from_here() {
        use windows::Win32::UI::WindowsAndMessaging::{
            IsWindowVisible, SW_SHOWNA, ShowWindowAsync,
        };
        let stuck = TestWindow::new(false, Then::Block);
        let hwnd = HWND(stuck.hwnd as _);
        // As bring_forward shows it, but without activating anything.
        // SAFETY: a window of this test's own.
        let _ = unsafe { ShowWindowAsync(hwnd, SW_SHOWNA) };
        assert!(!window_responds(stuck.hwnd, Duration::from_millis(200)));
        // SAFETY: a query about a window of this test's own.
        assert!(
            !unsafe { IsWindowVisible(hwnd) }.as_bool(),
            "the show waits for the stuck thread"
        );
    }
}
