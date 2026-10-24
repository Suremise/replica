//! Thin, safe wrappers round the Win32 calls Replica needs: volumes, Credential Manager,
//! network connections, the system's random numbers and job objects here; the UI languages
//! ([`locale`]), the console's echo ([`console`]), the registry ([`registry`]), the tray's
//! window, icon and menu ([`ui`]) and Task Scheduler ([`tasks`]).
//!
//! The only module (with its submodules) where `unsafe` is allowed: the workspace denies it
//! elsewhere. Each wrapper owns the buffers it passes to Windows, checks the result, copies
//! what comes back into Rust types and frees what Windows allocated, so callers never see a
//! pointer. Errors are `io::Error`s carrying the Win32 code (`raw_os_error()`).
#![allow(unsafe_code)]

pub mod console;
pub mod locale;
pub mod registry;
pub mod tasks;
pub mod ui;

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_NOT_FOUND, HANDLE, HLOCAL, LocalFree,
    WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::NetworkManagement::WNet::{
    CONNECT_TEMPORARY, NET_CONNECT_FLAGS, NETRESOURCEW, RESOURCETYPE_DISK, WNetAddConnection2W,
    WNetCancelConnection2W,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW,
    CredEnumerateW, CredFree, CredReadW, CredWriteW,
};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW, GetDriveTypeW,
    GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNamesForVolumeNameW,
    MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcessToken, OpenThread, ReleaseMutex, ResumeThread,
    THREAD_SUSPEND_RESUME, WaitForSingleObject,
};
use windows::core::{Error, HSTRING, PCWSTR, PWSTR};

/// `GetDriveTypeW`'s answer for a removable drive (a USB stick or card reader).
pub const DRIVE_REMOVABLE: u32 = 2;

pub(crate) fn io_error(error: &Error) -> io::Error {
    let hr = error.code().0 as u32;
    // A Win32 error wrapped as an HRESULT (facility 7) gives its code back; anything else stays.
    let code = if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    };
    io::Error::from_raw_os_error(code as i32)
}

fn is(error: &io::Error, code: windows::Win32::Foundation::WIN32_ERROR) -> bool {
    error.raw_os_error() == Some(code.0 as i32)
}

/// A wide string up to its first NUL.
fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// A NUL-terminated wide copy of `text`, for fields Windows wants as `PWSTR`.
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// --- Files -------------------------------------------------------------------------------------

/// Renames `from` to `to` unless `to` exists (`MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`):
/// one step of the file system's, on every file system, so an existing `to` is refused
/// (`ERROR_ALREADY_EXISTS` or `ERROR_FILE_EXISTS`, `io::ErrorKind::AlreadyExists`) and left as
/// it was.
pub fn move_file_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    // SAFETY: both HSTRINGs are NUL-terminated wide strings that outlive the call.
    unsafe {
        MoveFileExW(
            &HSTRING::from(from.as_os_str()),
            &HSTRING::from(to.as_os_str()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| io_error(&e))
}

// --- Volumes -----------------------------------------------------------------------------------

/// The volume GUID path (`\\?\Volume{…}\`) of the volume mounted at `mount_point` (`C:\`).
pub fn volume_name_for_mount_point(mount_point: &str) -> io::Result<String> {
    let mut point = mount_point.to_owned();
    if !point.ends_with('\\') {
        point.push('\\');
    }
    let mut buffer = [0u16; 64];
    // SAFETY: the mount point is a NUL-terminated HSTRING that outlives the call, and the
    // output buffer is a live slice whose length the wrapper passes.
    unsafe { GetVolumeNameForVolumeMountPointW(&HSTRING::from(point), &mut buffer) }
        .map_err(|e| io_error(&e))?;
    Ok(from_wide(&buffer))
}

/// Every volume's GUID path.
pub fn volume_names() -> io::Result<Vec<String>> {
    let mut buffer = [0u16; 64];
    // SAFETY: the buffer is a live slice; the handle is closed below on every path.
    let handle: HANDLE = unsafe { FindFirstVolumeW(&mut buffer) }.map_err(|e| io_error(&e))?;
    let mut names = vec![from_wide(&buffer)];
    let result = loop {
        buffer.fill(0);
        // SAFETY: `handle` came from FindFirstVolumeW and isn't closed yet.
        match unsafe { FindNextVolumeW(handle, &mut buffer) } {
            Ok(()) => names.push(from_wide(&buffer)),
            Err(e) => {
                let error = io_error(&e);
                break if is(&error, ERROR_NO_MORE_FILES) {
                    Ok(names)
                } else {
                    Err(error)
                };
            }
        }
    };
    // SAFETY: closes the search handle once.
    let _ = unsafe { FindVolumeClose(handle) };
    result
}

/// Where a volume is mounted: drive letters (`E:\`) and folders.
pub fn mount_points(volume_name: &str) -> io::Result<Vec<PathBuf>> {
    let name = HSTRING::from(volume_name);
    let mut buffer = vec![0u16; 256];
    loop {
        let mut needed = 0u32;
        // SAFETY: the name outlives the call; the buffer is a live slice and `needed` a live u32.
        match unsafe { GetVolumePathNamesForVolumeNameW(&name, Some(&mut buffer), &mut needed) } {
            Ok(()) => break,
            Err(e) => {
                let error = io_error(&e);
                if is(&error, ERROR_MORE_DATA) && (needed as usize) > buffer.len() {
                    buffer = vec![0u16; needed as usize];
                } else {
                    return Err(error);
                }
            }
        }
    }
    // A list of NUL-terminated strings, ended by an empty one.
    Ok(buffer
        .split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf16_lossy(s)))
        .collect())
}

/// A volume's label and file system, from its root (a GUID path or `C:\`).
pub fn volume_information(root: &str) -> io::Result<(String, String)> {
    let mut label = [0u16; 261];
    let mut file_system = [0u16; 261];
    // SAFETY: the root outlives the call and both buffers are live slices.
    unsafe {
        GetVolumeInformationW(
            &HSTRING::from(root),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut file_system),
        )
    }
    .map_err(|e| io_error(&e))?;
    Ok((from_wide(&label), from_wide(&file_system)))
}

/// `GetDriveTypeW` for a root: [`DRIVE_REMOVABLE`], fixed (3), network (4), CD (5) and so on.
pub fn drive_type(root: &str) -> u32 {
    // SAFETY: the root is a NUL-terminated HSTRING that outlives the call.
    unsafe { GetDriveTypeW(&HSTRING::from(root)) }
}

/// A volume's (total, free) bytes.
pub fn disk_space(root: &str) -> io::Result<(u64, u64)> {
    let (mut total, mut free) = (0u64, 0u64);
    // SAFETY: the root outlives the call; the out pointers point at live u64s.
    unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(root),
            Some(&mut free),
            Some(&mut total),
            None,
        )
    }
    .map_err(|e| io_error(&e))?;
    Ok((total, free))
}

// --- Credential Manager ------------------------------------------------------------------------

/// A generic credential: its user name and its secret's bytes.
pub struct StoredCredential {
    pub username: Option<String>,
    pub blob: Vec<u8>,
}

/// Reads the generic credential `target`, or `None` if there isn't one.
pub fn cred_read(target: &str) -> io::Result<Option<StoredCredential>> {
    let mut found: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: the name outlives the call and `found` is a live out pointer.
    match unsafe { CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut found) } {
        Ok(()) => {}
        Err(e) => {
            let error = io_error(&e);
            return if is(&error, ERROR_NOT_FOUND) {
                Ok(None)
            } else {
                Err(error)
            };
        }
    }
    // SAFETY: on success Windows hands back one CREDENTIALW it allocated; it's read here,
    // copied out, and freed once with CredFree.
    unsafe {
        let credential = &*found;
        let username = if credential.UserName.is_null() {
            None
        } else {
            Some(credential.UserName.to_string().unwrap_or_default()).filter(|u| !u.is_empty())
        };
        let blob = if credential.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
            .to_vec()
        };
        CredFree(found as *const c_void);
        Ok(Some(StoredCredential { username, blob }))
    }
}

/// Writes the generic credential `target` for this user on this PC (it doesn't roam),
/// replacing any there.
pub fn cred_write(target: &str, username: Option<&str>, blob: &[u8]) -> io::Result<()> {
    let mut name = to_wide(target);
    let mut user = username.map(to_wide);
    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: user
            .as_mut()
            .map_or(PWSTR::null(), |u| PWSTR(u.as_mut_ptr())),
        ..Default::default()
    };
    // SAFETY: every pointer in `credential` points into `name`, `user` or `blob`, which live
    // until after the call; CredWriteW only reads them.
    unsafe { CredWriteW(&credential, 0) }.map_err(|e| io_error(&e))
}

/// Deletes the generic credential `target`. Returns false if there wasn't one.
pub fn cred_delete(target: &str) -> io::Result<bool> {
    // SAFETY: the name is a NUL-terminated HSTRING that outlives the call.
    match unsafe { CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(true),
        Err(e) => {
            let error = io_error(&e);
            if is(&error, ERROR_NOT_FOUND) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

/// The names of this user's credentials matching `filter` (a name ending in `*`, such as
/// `Replica/*`).
pub fn cred_list(filter: &str) -> io::Result<Vec<String>> {
    let mut count = 0u32;
    let mut list: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: the filter outlives the call; `count` and `list` are live out pointers.
    match unsafe { CredEnumerateW(&HSTRING::from(filter), None, &mut count, &mut list) } {
        Ok(()) => {}
        Err(e) => {
            let error = io_error(&e);
            return if is(&error, ERROR_NOT_FOUND) {
                Ok(Vec::new())
            } else {
                Err(error)
            };
        }
    }
    // SAFETY: on success `list` is an array of `count` credential pointers that Windows
    // allocated as one block; names are copied out and the block freed once.
    unsafe {
        let names = std::slice::from_raw_parts(list, count as usize)
            .iter()
            .map(|c| (**c).TargetName.to_string().unwrap_or_default())
            .collect();
        CredFree(list as *const c_void);
        Ok(names)
    }
}

// --- Network connections -----------------------------------------------------------------------

/// Connects `remote` (`\\server\share`) for this user, without a drive letter and without
/// remembering it after logoff. Returns the Win32 code (0 on success).
pub fn net_add_connection(remote: &str, username: Option<&str>, password: Option<&str>) -> u32 {
    let mut remote_wide = to_wide(remote);
    let resource = NETRESOURCEW {
        dwType: RESOURCETYPE_DISK,
        lpRemoteName: PWSTR(remote_wide.as_mut_ptr()),
        ..Default::default()
    };
    let mut password_wide = password.map(to_wide);
    let username_wide = username.map(to_wide);
    let pcwstr = |w: &Option<Vec<u16>>| w.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
    // SAFETY: the resource's remote name, the password and the user name all point into
    // buffers that live until after the call; Windows only reads them.
    let result = unsafe {
        WNetAddConnection2W(
            &resource,
            pcwstr(&password_wide),
            pcwstr(&username_wide),
            CONNECT_TEMPORARY,
        )
    };
    // Don't leave the password lying in freed memory.
    if let Some(p) = password_wide.as_mut() {
        p.fill(0);
    }
    result.0
}

/// Disconnects a connection made by [`net_add_connection`], unless files are open on it.
pub fn net_cancel_connection(remote: &str) -> u32 {
    // SAFETY: the name is a NUL-terminated HSTRING that outlives the call.
    unsafe { WNetCancelConnection2W(&HSTRING::from(remote), NET_CONNECT_FLAGS(0), false) }.0
}

// --- Named mutexes -----------------------------------------------------------------------------

/// A held named mutex: one holder at a time across every process that opens the same name
/// (`Global\…`: every session on this PC; `Local\…`: this logon session). Released when
/// dropped, on the thread that took it (so it isn't `Send`).
pub struct NamedMutexGuard {
    handle: HANDLE,
    _not_send: std::marker::PhantomData<*const ()>,
}

/// Takes the named mutex `name` (with its namespace, such as `Global\Replica.X`), waiting up
/// to `timeout`. A mutex left abandoned by a process that died holding it is taken over. It's
/// created with the default security of this user's token, so no admin rights are needed.
pub fn lock_named_mutex(name: &str, timeout: std::time::Duration) -> io::Result<NamedMutexGuard> {
    let full = HSTRING::from(name);
    // SAFETY: the name outlives the call; no security attributes are passed.
    let handle = unsafe { CreateMutexW(None, false, &full) }.map_err(|e| io_error(&e))?;
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: `handle` is a live mutex handle from CreateMutexW.
    let waited = unsafe { WaitForSingleObject(handle, millis) };
    if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
        return Ok(NamedMutexGuard {
            handle,
            _not_send: std::marker::PhantomData,
        });
    }
    // SAFETY: closes the handle once; it isn't owned, so there's nothing to release.
    let _ = unsafe { CloseHandle(handle) };
    Err(if waited == WAIT_TIMEOUT {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{name} is held by another process"),
        )
    } else {
        io::Error::last_os_error()
    })
}

impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        // SAFETY: this thread owns the mutex (taken in lock_named_mutex, and the guard can't
        // move to another thread); it's released and its handle closed once.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

// --- The current user --------------------------------------------------------------------------

/// The SID of the user this process runs as, such as `S-1-5-21-…-1001`: the same in every
/// logon session of that user.
pub fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needn't be closed; `token` is a
    // live out pointer, and the token it receives is closed below on every path.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| io_error(&e))?;
    let result = token_user_sid(token);
    // SAFETY: closes the token handle once.
    let _ = unsafe { CloseHandle(token) };
    result
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: a size query: no buffer, and `needed` is a live out pointer. It fails with
    // "insufficient buffer" by design, so its result is ignored.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    // u64s so the buffer is aligned for TOKEN_USER.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    let length = (buffer.len() * 8) as u32;
    // SAFETY: the buffer is live, writable and `length` bytes long.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut needed,
        )
    }
    .map_err(|e| io_error(&e))?;
    let mut text = PWSTR::null();
    // SAFETY: on success the buffer holds a TOKEN_USER whose SID points into the same buffer,
    // which outlives the call; Windows allocates `text`, which is copied and freed once with
    // LocalFree.
    unsafe {
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(|e| io_error(&e))?;
        let sid = text.to_string().unwrap_or_default();
        LocalFree(Some(HLOCAL(text.0.cast())));
        Ok(sid)
    }
}

// --- Random numbers ----------------------------------------------------------------------------

/// Fills `buffer` from the system's cryptographic random number generator.
pub fn random_bytes(buffer: &mut [u8]) -> io::Result<()> {
    // SAFETY: the buffer is a live, writable slice whose length the wrapper passes.
    let status = unsafe { BCryptGenRandom(None, buffer, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "BCryptGenRandom failed: {:#x}",
            status.0
        )))
    }
}

// --- Job objects (the process tree of a command step or a restic command) ----------------------

/// A job object with kill-on-close: closing its last handle ends every process in it. Dropped,
/// it closes its handle.
pub struct Job(HANDLE);

// The handle is only ever used through `&self` calls Windows allows from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// A new, unnamed job object whose processes all end when it's closed.
    pub fn new_kill_on_close() -> io::Result<Job> {
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| io_error(&e))?;
        let job = Job(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| io_error(&e))?;
        Ok(job)
    }

    /// Puts a process (by its handle, as `std::process::Child` holds it) in the job.
    pub fn assign(&self, process: std::os::windows::io::RawHandle) -> io::Result<()> {
        unsafe { AssignProcessToJobObject(self.0, HANDLE(process)) }.map_err(|e| io_error(&e))
    }

    /// Ends every process in the job.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        unsafe { TerminateJobObject(self.0, exit_code) }.map_err(|e| io_error(&e))
    }

    /// The IDs of the processes in the job now, as the job reports them.
    pub fn process_ids(&self) -> io::Result<Vec<u32>> {
        // The list is a header (two u32 counts) and then one usize per process; a buffer of
        // usizes keeps it aligned. Grow it until the whole list fits.
        let mut capacity = 64usize;
        loop {
            let mut buffer = vec![0usize; 1 + capacity];
            let result = unsafe {
                QueryInformationJobObject(
                    Some(self.0),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    (buffer.len() * std::mem::size_of::<usize>()) as u32,
                    None,
                )
            };
            let list = buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            let (assigned, listed) = unsafe {
                (
                    (*list).NumberOfAssignedProcesses as usize,
                    (*list).NumberOfProcessIdsInList as usize,
                )
            };
            match result {
                Ok(()) if listed >= assigned => {
                    return Ok(buffer[1..1 + listed].iter().map(|&id| id as u32).collect());
                }
                Ok(()) => capacity = assigned + 16,
                Err(e) => {
                    let e = io_error(&e);
                    if is(&e, ERROR_MORE_DATA) {
                        capacity = assigned.max(capacity * 2) + 16;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// How many processes in the job are still running.
    pub fn active_processes(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(self.0),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
        }
        .map_err(|e| io_error(&e))?;
        Ok(info.ActiveProcesses)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Kill-on-close: this ends anything still in the job.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Resumes every thread of a process created suspended (`CREATE_SUSPENDED`), once it's in its
/// job. An error if no thread was resumed.
pub fn resume_process(pid: u32) -> io::Result<()> {
    let snapshot =
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(|e| io_error(&e))?;
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = 0;
    let mut failure = None;
    let mut more = unsafe { Thread32First(snapshot, &mut entry) }.is_ok();
    while more {
        if entry.th32OwnerProcessID == pid {
            match unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) } {
                Ok(thread) => {
                    if unsafe { ResumeThread(thread) } == u32::MAX {
                        failure = Some(io::Error::last_os_error());
                    } else {
                        resumed += 1;
                    }
                    let _ = unsafe { CloseHandle(thread) };
                }
                Err(e) => failure = Some(io_error(&e)),
            }
        }
        more = unsafe { Thread32Next(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    match (resumed, failure) {
        (0, Some(e)) => Err(e),
        (0, None) => Err(io::Error::other(format!(
            "no thread of process {pid} to resume"
        ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_bytes_differ() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        random_bytes(&mut a).unwrap();
        random_bytes(&mut b).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_named_mutex_has_one_holder_at_a_time() {
        use std::time::Duration;
        let name = format!(r"Global\ReplicaTest-{}-mutex", std::process::id());
        let held = lock_named_mutex(&name, Duration::from_secs(5)).unwrap();
        let other = {
            let name = name.clone();
            std::thread::spawn(move || {
                lock_named_mutex(&name, Duration::from_millis(100))
                    .map(|_| ())
                    .map_err(|e| e.kind())
            })
        };
        assert_eq!(other.join().unwrap(), Err(io::ErrorKind::TimedOut));
        drop(held);
        let again =
            std::thread::spawn(move || lock_named_mutex(&name, Duration::from_secs(5)).is_ok());
        assert!(again.join().unwrap());
    }

    /// The SID is this user's, as `whoami` reports it: the same whatever the session.
    #[test]
    fn the_current_user_sid_is_whoami_s() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let whoami = std::process::Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&whoami.stdout);
        assert!(text.contains(&format!("\"{sid}\"")), "{text} vs {sid}");
    }

    #[test]
    fn a_missing_credential_is_none_not_an_error() {
        let name = format!("ReplicaTest-{}-missing/none", std::process::id());
        assert!(cred_read(&name).unwrap().is_none());
        assert!(!cred_delete(&name).unwrap());
        assert!(cred_list(&format!("{name}*")).unwrap().is_empty());
    }
}
