//! [`TargetResolver`] on Windows: drives found by volume GUID or label, whatever letter they
//! have now, and SMB shares by UNC path, connected with a login from Credential Manager.
//!
//! - A drive that isn't connected, or a share that can't be reached, is "not available", so
//!   the job is skipped. A share that refuses its login, or is already connected with another
//!   one, is an error, so the job fails with a reason the user can act on.
//! - A share is checked within a time limit: a quick probe of its SMB port first, then the
//!   connection itself. A NAS that's off doesn't hold a run up for long.
//! - Connections Replica makes (without a drive letter, not remembered after logoff) are
//!   disconnected when the [`WindowsTargets`] is dropped, at the end of the run, or by
//!   [`WindowsTargets::disconnect`]. One that succeeds only after its time-out, once the
//!   resolver has been dropped, is disconnected by the thread that made it. A share the user
//!   had connected already is left alone.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use replica_core::job::VolumeRef;
use replica_core::platform::{Credential, PlatformError, ShareStatus, TargetResolver};
use replica_core::tr;

use crate::win32;

/// A connected volume, for choosing a drive target (the window and `replica init`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Its volume GUID path, `\\?\Volume{…}\`: what a target's `volume_id` holds.
    pub id: String,
    /// Its label, if it has one.
    pub label: Option<String>,
    /// Where it's mounted: drive letters (`E:\`) and folders. Empty if it isn't mounted.
    pub mount_points: Vec<PathBuf>,
    pub file_system: Option<String>,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// Windows reports it as removable (a USB stick or card). Many external hard drives
    /// report as fixed.
    pub removable: bool,
}

impl VolumeInfo {
    /// Its drive letter, if it has one.
    pub fn letter(&self) -> Option<char> {
        self.mount_points.iter().find_map(|p| {
            let s = p.to_str()?;
            let mut chars = s.chars();
            match (chars.next(), chars.next(), chars.next(), chars.next()) {
                (Some(c), Some(':'), Some('\\'), None) if c.is_ascii_alphabetic() => {
                    Some(c.to_ascii_uppercase())
                }
                _ => None,
            }
        })
    }

    /// Where to reach it: its drive letter, else its first mount point, else its GUID path.
    pub fn root(&self) -> PathBuf {
        match self.letter() {
            Some(c) => PathBuf::from(format!("{c}:\\")),
            None => self
                .mount_points
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from(&self.id)),
        }
    }
}

/// Every volume Windows knows of, with its label, mount points and size where it can tell.
pub fn list_volumes() -> Result<Vec<VolumeInfo>, PlatformError> {
    let names = win32::volume_names()
        .map_err(|e| PlatformError(tr!("error-volumes-failed", detail = e.to_string())))?;
    Ok(names
        .into_iter()
        .map(|id| {
            let mount_points = win32::mount_points(&id).unwrap_or_default();
            // A card reader with no card has a volume but no medium: no label or size.
            let info = win32::volume_information(&id).ok();
            let space = win32::disk_space(&id).ok();
            VolumeInfo {
                label: info.as_ref().map(|i| i.0.clone()).filter(|l| !l.is_empty()),
                file_system: info.map(|i| i.1).filter(|f| !f.is_empty()),
                total_bytes: space.map(|s| s.0),
                free_bytes: space.map(|s| s.1),
                removable: win32::drive_type(&id) == win32::DRIVE_REMOVABLE,
                mount_points,
                id,
            }
        })
        .collect())
}

/// The volume GUID of whatever is mounted at `mount_point` (`C:\`).
pub fn volume_id_of(mount_point: &str) -> io::Result<String> {
    win32::volume_name_for_mount_point(mount_point)
}

/// A volume ID in a form to compare: the GUID alone, lowercase. Takes `\\?\Volume{…}\`,
/// `Volume{…}`, `{…}` or the bare GUID.
pub fn normalize_volume_id(id: &str) -> Option<String> {
    let s = id.trim();
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    let s = s.strip_suffix('\\').unwrap_or(s);
    let s = s
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("volume"))
        .map_or(s, |_| &s[6..]);
    let s = s
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(s);
    let valid = s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    valid.then(|| s.to_ascii_lowercase())
}

/// Picks the volume a target means: by ID first, then by label (ignoring case). Two connected
/// volumes with the label asked for is an error, rather than a guess at which drive to write to.
pub fn match_volume(
    volumes: &[VolumeInfo],
    wanted: &VolumeRef,
) -> Result<Option<PathBuf>, PlatformError> {
    if let Some(id) = wanted.volume_id.as_deref().and_then(normalize_volume_id)
        && let Some(found) = volumes
            .iter()
            .find(|v| normalize_volume_id(&v.id).as_deref() == Some(id.as_str()))
    {
        return Ok(Some(found.root()));
    }
    let Some(label) = wanted.label.as_deref() else {
        return Ok(None);
    };
    let mut matches = volumes.iter().filter(|v| {
        v.label
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(label))
    });
    match (matches.next(), matches.next()) {
        (Some(only), None) => Ok(Some(only.root())),
        (None, _) => Ok(None),
        (Some(_), Some(_)) => Err(PlatformError(tr!(
            "error-volume-label-ambiguous",
            label = label
        ))),
    }
}

/// The server and share root of a UNC path: `\\nas\backup\hive` gives `nas` and
/// `\\nas\backup`. `None` for anything else, `\\?\` and `\\.\` paths included.
pub fn share_root(unc: &str) -> Option<(String, String)> {
    let rest = unc.strip_prefix(r"\\")?;
    let mut parts = rest.split('\\');
    let server = parts
        .next()
        .filter(|s| !s.is_empty() && *s != "?" && *s != ".")?;
    let share = parts.next().filter(|s| !s.is_empty())?;
    Some((server.to_owned(), format!(r"\\{server}\{share}")))
}

/// What a connection attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Connected,
    Unreachable(String),
    LoginRefused,
    OtherLogin,
    AccessDenied,
    Failed(String),
}

/// Sorts a Win32 error from connecting or reaching a share.
fn classify(code: u32) -> Outcome {
    let text = || match code {
        0 => tr!("error-share-no-reason"),
        _ => io::Error::from_raw_os_error(code as i32).to_string(),
    };
    match code {
        0 => Outcome::Connected,
        // Wrong password or user, locked out, disabled, expired.
        86 | 1326 | 1327 | 1328 | 1329 | 1330 | 1331 | 1907 | 1909 => Outcome::LoginRefused,
        1219 => Outcome::OtherLogin,
        5 => Outcome::AccessDenied,
        // The path, the server or the network isn't there.
        53 | 64 | 67 | 121 | 1203 | 1222 | 1231 | 1232 | 1311 | 2250 => {
            Outcome::Unreachable(text())
        }
        _ => Outcome::Failed(text()),
    }
}

/// The OS calls reaching a share makes: Windows' own, or a fake in tests.
pub(crate) trait Connector: Send + Sync + 'static {
    /// Whether some address of `server` answers on the SMB port; `None` if the name doesn't
    /// resolve here.
    fn smb_port_answers(&self, server: &str, timeout: Duration) -> Option<bool>;
    /// Whether `root` can be reached as the user is now; the Win32 code if not.
    fn reach_as_is(&self, root: &str) -> Result<(), u32>;
    /// Connects `root` with a login; the Win32 code (0 on success).
    fn connect(&self, root: &str, username: Option<&str>, password: &str) -> u32;
    /// Disconnects a connection `connect` made; the Win32 code.
    fn disconnect(&self, root: &str) -> u32;
}

/// The real thing.
struct Win32Connector;

impl Connector for Win32Connector {
    fn smb_port_answers(&self, server: &str, timeout: Duration) -> Option<bool> {
        let addresses: Vec<_> = (server, 445).to_socket_addrs().ok()?.collect();
        if addresses.is_empty() {
            return None;
        }
        Some(
            addresses
                .iter()
                .any(|a| TcpStream::connect_timeout(a, timeout).is_ok()),
        )
    }

    fn reach_as_is(&self, root: &str) -> Result<(), u32> {
        std::fs::metadata(root)
            .map(|_| ())
            .map_err(|e| e.raw_os_error().unwrap_or(0) as u32)
    }

    fn connect(&self, root: &str, username: Option<&str>, password: &str) -> u32 {
        win32::net_add_connection(root, username, Some(password))
    }

    fn disconnect(&self, root: &str) -> u32 {
        win32::net_cancel_connection(root)
    }
}

/// The shares a resolver connected, and whether it has closed. Workers that finish after a
/// time-out check `closed` under the same lock as they record a connection, so a connection
/// made after the resolver is gone is disconnected by the worker itself.
#[derive(Default)]
struct Connections {
    closed: bool,
    made: Vec<String>,
}

/// Finds targets on this PC and connects shares.
pub struct WindowsTargets {
    /// How long each address of a share's server gets to answer on the SMB port.
    pub probe_timeout: Duration,
    /// How long reaching or connecting a share may take in all.
    pub connect_timeout: Duration,
    connector: Arc<dyn Connector>,
    connections: Arc<Mutex<Connections>>,
}

impl std::fmt::Debug for WindowsTargets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsTargets")
            .field("probe_timeout", &self.probe_timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("connected_by_us", &self.connected_by_us())
            .finish()
    }
}

impl Default for WindowsTargets {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsTargets {
    /// A 3-second probe and 20 seconds in all.
    pub fn new() -> Self {
        Self::with_connector(Arc::new(Win32Connector))
    }

    pub(crate) fn with_connector(connector: Arc<dyn Connector>) -> Self {
        Self {
            probe_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(20),
            connector,
            connections: Arc::default(),
        }
    }

    fn connections(&self) -> std::sync::MutexGuard<'_, Connections> {
        self.connections.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The shares this resolver has connected and not yet disconnected.
    pub fn connected_by_us(&self) -> Vec<String> {
        self.connections().made.clone()
    }

    /// Disconnects the shares this resolver connected so far. One with files still open stays
    /// connected (Windows refuses), and is logged. The resolver can still be used.
    pub fn disconnect(&self) {
        let made = std::mem::take(&mut self.connections().made);
        disconnect_all(self.connector.as_ref(), made);
    }
}

fn disconnect_all(connector: &dyn Connector, roots: Vec<String>) {
    for root in roots {
        let code = connector.disconnect(&root);
        if code != 0 {
            tracing::warn!(share = %root, code, "couldn't disconnect a share Replica connected");
        }
    }
}

impl Drop for WindowsTargets {
    /// Disconnects what this resolver connected, and marks it closed, so a connection that a
    /// timed-out worker makes later is disconnected by that worker.
    fn drop(&mut self) {
        let made = {
            let mut connections = self.connections();
            connections.closed = true;
            std::mem::take(&mut connections.made)
        };
        disconnect_all(self.connector.as_ref(), made);
    }
}

impl TargetResolver for WindowsTargets {
    fn locate_volume(&self, volume: &VolumeRef) -> Result<Option<PathBuf>, PlatformError> {
        match_volume(&list_volumes()?, volume)
    }

    fn connect_share(
        &self,
        unc: &str,
        login: Option<&Credential>,
    ) -> Result<ShareStatus, PlatformError> {
        let Some((server, root)) = share_root(unc) else {
            return Err(PlatformError(tr!("error-job-unc-invalid", path = unc)));
        };
        let (send, receive) = mpsc::channel();
        let user = login
            .and_then(|l| l.username.clone())
            .unwrap_or_else(|| "?".into());
        let worker = Worker {
            server,
            root: root.clone(),
            login: login.cloned(),
            probe_timeout: self.probe_timeout,
            connector: Arc::clone(&self.connector),
            connections: Arc::clone(&self.connections),
        };
        // On a time-out the worker is left to finish on its own (see `Connections`).
        std::thread::spawn(move || {
            let _ = send.send(worker.run());
        });
        let outcome = match receive.recv_timeout(self.connect_timeout) {
            Ok(outcome) => outcome,
            Err(_) => {
                return Ok(ShareStatus::NotAvailable(tr!(
                    "error-share-timeout",
                    share = root.as_str(),
                    seconds = self.connect_timeout.as_secs()
                )));
            }
        };
        match outcome {
            Outcome::Connected => Ok(ShareStatus::Connected),
            Outcome::Unreachable(detail) => Ok(ShareStatus::NotAvailable(tr!(
                "error-share-unreachable",
                share = root.as_str(),
                detail = detail
            ))),
            Outcome::LoginRefused => Err(PlatformError(tr!(
                "error-share-login-refused",
                share = root.as_str(),
                user = user
            ))),
            Outcome::OtherLogin => Err(PlatformError(tr!(
                "error-share-other-login",
                share = root.as_str()
            ))),
            Outcome::AccessDenied => Err(PlatformError(tr!(
                "error-share-access-denied",
                share = root.as_str()
            ))),
            Outcome::Failed(detail) => Err(PlatformError(tr!(
                "error-share-failed",
                share = root.as_str(),
                detail = detail
            ))),
        }
    }
}

/// One share connection, run on its own thread.
struct Worker {
    server: String,
    root: String,
    login: Option<Credential>,
    probe_timeout: Duration,
    connector: Arc<dyn Connector>,
    connections: Arc<Mutex<Connections>>,
}

impl Worker {
    /// Probes the server, then reaches the share as the user is, or connects it with the login.
    fn run(self) -> Outcome {
        let c = self.connector.as_ref();
        if c.smb_port_answers(&self.server, self.probe_timeout) == Some(false) {
            return Outcome::Unreachable(tr!(
                "error-share-smb-no-answer",
                server = self.server.as_str()
            ));
        }
        // Already reachable (the user's own connection, or no login needed): use it as it is.
        let code = match c.reach_as_is(&self.root) {
            Ok(()) => return Outcome::Connected,
            Err(code) => code,
        };
        let Some(login) = &self.login else {
            return match classify(code) {
                // The user's own login was refused: the share needs one of its own.
                Outcome::LoginRefused => Outcome::AccessDenied,
                Outcome::Connected => Outcome::Failed(tr!("error-share-no-reason")),
                other => other,
            };
        };
        let outcome =
            classify(c.connect(&self.root, login.username.as_deref(), login.secret.expose()));
        if outcome == Outcome::Connected {
            let mut connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
            if connections.closed {
                // The resolver is gone (this worker timed out): nobody else will disconnect it.
                drop(connections);
                disconnect_all(c, vec![self.root.clone()]);
            } else {
                connections.made.push(self.root.clone());
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID_A: &str = r"\\?\Volume{0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0}\";
    const GUID_B: &str = r"\\?\Volume{11111111-2222-3333-4444-555555555555}\";

    fn volume(id: &str, label: Option<&str>, mounts: &[&str]) -> VolumeInfo {
        VolumeInfo {
            id: id.into(),
            label: label.map(Into::into),
            mount_points: mounts.iter().map(PathBuf::from).collect(),
            file_system: Some("NTFS".into()),
            total_bytes: None,
            free_bytes: None,
            removable: false,
        }
    }

    fn by_id(id: &str) -> VolumeRef {
        VolumeRef {
            volume_id: Some(id.into()),
            label: None,
        }
    }

    fn by_label(label: &str) -> VolumeRef {
        VolumeRef {
            volume_id: None,
            label: Some(label.into()),
        }
    }

    #[test]
    fn volume_ids_compare_in_any_form() {
        let want = Some("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0".to_string());
        for form in [
            GUID_A,
            r"\\?\Volume{0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0}",
            "Volume{0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0}",
            "{0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0}",
            " 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0 ",
        ] {
            assert_eq!(normalize_volume_id(form), want, "{form}");
        }
        for bad in ["", "E:", "{not-a-guid}", r"\\?\Volume{0f1e2d3c}\"] {
            assert_eq!(normalize_volume_id(bad), None, "{bad}");
        }
    }

    /// The same drive, plugged in as E: and later as F:, is found both times by its ID.
    #[test]
    fn a_drive_on_a_new_letter_still_matches() {
        let before = [volume(GUID_A, Some("BACKUP"), &[r"E:\"])];
        let after = [
            volume(GUID_B, Some("OTHER"), &[r"E:\"]),
            volume(GUID_A, Some("BACKUP"), &[r"F:\"]),
        ];
        let wanted = by_id(GUID_A);
        assert_eq!(
            match_volume(&before, &wanted).unwrap(),
            Some(PathBuf::from(r"E:\"))
        );
        assert_eq!(
            match_volume(&after, &wanted).unwrap(),
            Some(PathBuf::from(r"F:\"))
        );
        assert_eq!(
            match_volume(&after, &by_label("backup")).unwrap(),
            Some(PathBuf::from(r"F:\"))
        );
    }

    #[test]
    fn an_unknown_id_falls_back_to_the_label() {
        let volumes = [volume(GUID_A, Some("BACKUP"), &[r"G:\"])];
        let wanted = VolumeRef {
            volume_id: Some(GUID_B.into()),
            label: Some("BACKUP".into()),
        };
        assert_eq!(
            match_volume(&volumes, &wanted).unwrap(),
            Some(PathBuf::from(r"G:\"))
        );
        assert_eq!(match_volume(&volumes, &by_id(GUID_B)).unwrap(), None);
        assert_eq!(match_volume(&volumes, &by_label("NOPE")).unwrap(), None);
        assert_eq!(match_volume(&[], &by_id(GUID_A)).unwrap(), None);
    }

    #[test]
    fn two_drives_with_one_label_is_an_error_not_a_guess() {
        let volumes = [
            volume(GUID_A, Some("BACKUP"), &[r"E:\"]),
            volume(GUID_B, Some("Backup"), &[r"F:\"]),
        ];
        assert!(match_volume(&volumes, &by_label("BACKUP")).is_err());
        // The ID still picks one out.
        assert_eq!(
            match_volume(&volumes, &by_id(GUID_B)).unwrap(),
            Some(PathBuf::from(r"F:\"))
        );
    }

    #[test]
    fn a_volume_is_reached_by_letter_then_folder_then_guid() {
        let v = volume(GUID_A, None, &[r"C:\mnt\backup\", r"h:\"]);
        assert_eq!(v.letter(), Some('H'));
        assert_eq!(v.root(), PathBuf::from(r"H:\"));
        let folder = volume(GUID_A, None, &[r"C:\mnt\backup\"]);
        assert_eq!(folder.letter(), None);
        assert_eq!(folder.root(), PathBuf::from(r"C:\mnt\backup\"));
        assert_eq!(volume(GUID_A, None, &[]).root(), PathBuf::from(GUID_A));
    }

    #[test]
    fn share_roots() {
        assert_eq!(
            share_root(r"\\nas\backup\hive\daily"),
            Some(("nas".into(), r"\\nas\backup".into()))
        );
        assert_eq!(
            share_root(r"\\192.168.1.5\b"),
            Some(("192.168.1.5".into(), r"\\192.168.1.5\b".into()))
        );
        for bad in [
            r"\\nas",
            r"\\nas\",
            r"\\?\C:\x",
            r"\\.\pipe\x",
            r"C:\x",
            r"\nas\x",
            "",
        ] {
            assert_eq!(share_root(bad), None, "{bad}");
        }
    }

    /// A connector whose answers the test sets; `connect` can be held until the test lets it go.
    struct FakeConnector {
        probe: Option<bool>,
        as_is: Result<(), u32>,
        connect_code: u32,
        gate: Mutex<Option<mpsc::Receiver<()>>>,
        connects: Mutex<Vec<String>>,
        disconnects: Mutex<Vec<String>>,
    }

    impl FakeConnector {
        fn new(probe: Option<bool>, as_is: Result<(), u32>, connect_code: u32) -> Self {
            Self {
                probe,
                as_is,
                connect_code,
                gate: Mutex::new(None),
                connects: Mutex::default(),
                disconnects: Mutex::default(),
            }
        }

        /// Holds `connect` until the returned sender sends (or is dropped).
        fn held(probe: Option<bool>, connect_code: u32) -> (Self, mpsc::Sender<()>) {
            let (release, gate) = mpsc::channel();
            let fake = Self::new(probe, Err(67), connect_code);
            *fake.gate.lock().unwrap() = Some(gate);
            (fake, release)
        }

        fn disconnects(&self) -> Vec<String> {
            self.disconnects.lock().unwrap().clone()
        }
    }

    impl Connector for FakeConnector {
        fn smb_port_answers(&self, _: &str, _: Duration) -> Option<bool> {
            self.probe
        }

        fn reach_as_is(&self, _: &str) -> Result<(), u32> {
            self.as_is
        }

        fn connect(&self, root: &str, _: Option<&str>, _: &str) -> u32 {
            if let Some(gate) = self.gate.lock().unwrap().take() {
                let _ = gate.recv();
            }
            self.connects.lock().unwrap().push(root.to_owned());
            self.connect_code
        }

        fn disconnect(&self, root: &str) -> u32 {
            self.disconnects.lock().unwrap().push(root.to_owned());
            0
        }
    }

    const UNC: &str = r"\\nas\backup\hive";
    const ROOT: &str = r"\\nas\backup";

    fn login() -> Credential {
        Credential {
            username: Some("replica-test".into()),
            secret: replica_core::platform::Secret::new("pw"),
        }
    }

    fn resolver(fake: &Arc<FakeConnector>, timeout: Duration) -> WindowsTargets {
        let mut r = WindowsTargets::with_connector(Arc::clone(fake) as Arc<dyn Connector>);
        r.connect_timeout = timeout;
        r
    }

    /// Polls `condition` until it holds, or fails after a generous bound.
    fn wait_for(what: &str, condition: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The worker times out, the resolver is dropped, and only then does the connection
    /// succeed: the worker disconnects it itself.
    #[test]
    fn a_connection_made_after_the_resolver_is_gone_is_disconnected() {
        let (fake, release) = FakeConnector::held(None, 0);
        let fake = Arc::new(fake);
        let r = resolver(&fake, Duration::from_millis(50));
        let status = r.connect_share(UNC, Some(&login())).unwrap();
        assert!(matches!(status, ShareStatus::NotAvailable(_)), "{status:?}");
        drop(r);
        assert!(fake.disconnects().is_empty(), "nothing was connected yet");
        release.send(()).unwrap();
        wait_for("the late disconnect", || fake.disconnects() == [ROOT]);
        assert_eq!(*fake.connects.lock().unwrap(), [ROOT]);
    }

    /// A connection that succeeds after its time-out while the resolver lives is recorded,
    /// and disconnected when the resolver goes.
    #[test]
    fn a_late_connection_is_disconnected_with_the_resolver() {
        let (fake, release) = FakeConnector::held(None, 0);
        let fake = Arc::new(fake);
        let r = resolver(&fake, Duration::from_millis(50));
        assert!(matches!(
            r.connect_share(UNC, Some(&login())).unwrap(),
            ShareStatus::NotAvailable(_)
        ));
        release.send(()).unwrap();
        wait_for("the late connection", || r.connected_by_us() == [ROOT]);
        assert!(fake.disconnects().is_empty());
        drop(r);
        assert_eq!(fake.disconnects(), [ROOT]);
    }

    #[test]
    fn a_connection_in_time_is_disconnected_once() {
        let fake = Arc::new(FakeConnector::new(Some(true), Err(67), 0));
        let r = resolver(&fake, Duration::from_secs(30));
        assert_eq!(
            r.connect_share(UNC, Some(&login())).unwrap(),
            ShareStatus::Connected
        );
        assert_eq!(r.connected_by_us(), [ROOT]);
        r.disconnect();
        assert_eq!(fake.disconnects(), [ROOT]);
        drop(r);
        assert_eq!(fake.disconnects(), [ROOT], "not again on drop");
    }

    #[test]
    fn a_share_already_reachable_is_used_as_it_is_and_left_alone() {
        let fake = Arc::new(FakeConnector::new(Some(true), Ok(()), 0));
        let r = resolver(&fake, Duration::from_secs(30));
        assert_eq!(
            r.connect_share(UNC, Some(&login())).unwrap(),
            ShareStatus::Connected
        );
        assert!(fake.connects.lock().unwrap().is_empty());
        drop(r);
        assert!(fake.disconnects().is_empty());
    }

    #[test]
    fn a_wrong_password_fails_and_an_unreachable_server_skips() {
        let refused = Arc::new(FakeConnector::new(Some(true), Err(1326), 1326));
        let err = resolver(&refused, Duration::from_secs(30))
            .connect_share(UNC, Some(&login()))
            .unwrap_err();
        assert_eq!(
            err.0,
            tr!(
                "error-share-login-refused",
                share = ROOT,
                user = "replica-test"
            )
        );

        let gone = Arc::new(FakeConnector::new(Some(true), Err(67), 53));
        assert!(matches!(
            resolver(&gone, Duration::from_secs(30))
                .connect_share(UNC, Some(&login()))
                .unwrap(),
            ShareStatus::NotAvailable(_)
        ));
    }

    /// The probe's and the no-reason diagnostics come from Fluent, inside the outer message.
    #[test]
    fn replica_s_own_diagnostics_are_translated() {
        let silent = Arc::new(FakeConnector::new(Some(false), Err(67), 0));
        let status = resolver(&silent, Duration::from_secs(30))
            .connect_share(UNC, Some(&login()))
            .unwrap();
        let detail = tr!("error-share-smb-no-answer", server = "nas");
        assert!(!detail.starts_with("error-"), "the key exists");
        assert_eq!(
            status,
            ShareStatus::NotAvailable(tr!(
                "error-share-unreachable",
                share = ROOT,
                detail = detail
            ))
        );

        let no_code = Arc::new(FakeConnector::new(Some(true), Err(0), 0));
        let err = resolver(&no_code, Duration::from_secs(30))
            .connect_share(UNC, None)
            .unwrap_err();
        let reason = tr!("error-share-no-reason");
        assert!(!reason.starts_with("error-"), "the key exists");
        assert_eq!(
            err.0,
            tr!("error-share-failed", share = ROOT, detail = reason)
        );
    }

    #[test]
    fn connection_errors_sort_into_skip_and_fail() {
        assert_eq!(classify(0), Outcome::Connected);
        for wrong_login in [86, 1326, 1909] {
            assert_eq!(classify(wrong_login), Outcome::LoginRefused);
        }
        assert_eq!(classify(1219), Outcome::OtherLogin);
        assert_eq!(classify(5), Outcome::AccessDenied);
        for gone in [53, 67, 1203, 1231] {
            assert!(matches!(classify(gone), Outcome::Unreachable(_)), "{gone}");
        }
        assert!(matches!(classify(1450), Outcome::Failed(_)));
    }
}
