//! One HTTPS request with WinHTTP, part of Windows (so no HTTP or TLS crate ships in
//! Replica): a `GET` whose redirect is read, not followed. Only the update check uses it
//! ([`crate::update`]); with [`crate::update`], the only code in Replica that touches the
//! network.

use std::time::Duration;

use windows::Win32::Networking::WinHttp::{
    ERROR_WINHTTP_HEADER_NOT_FOUND, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_DISABLE_REDIRECTS,
    WINHTTP_FLAG_SECURE, WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::core::{HRESULT, HSTRING, PCWSTR, w};

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut std::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn handle(raw: *mut std::ffi::c_void) -> Result<Handle, String> {
    if raw.is_null() {
        Err(windows::core::Error::from_thread().message())
    } else {
        Ok(Handle(raw))
    }
}

fn check(result: windows::core::Result<()>) -> Result<(), String> {
    result.map_err(|e| e.message())
}

/// `GET https://<host><path>` on port 443 with redirects off, as `agent`, each step (resolving,
/// connecting, sending, receiving) allowed `timeout`: the status and the `Location`. An error
/// is the system's own message (in the user's language).
pub fn redirect_location(
    host: &str,
    path: &str,
    agent: &str,
    timeout: Duration,
) -> Result<(u32, Option<String>), String> {
    let (agent, host, path) = (
        HSTRING::from(agent),
        HSTRING::from(host),
        HSTRING::from(path),
    );
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: each handle is checked and closed by `Handle`; the strings outlive the calls;
    // the buffers passed to WinHttpQueryHeaders are live and their sizes given.
    unsafe {
        let session = handle(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        check(WinHttpSetTimeouts(session.0, ms, ms, ms, ms))?;
        let connect = handle(WinHttpConnect(session.0, &host, 443, 0))?;
        let request = handle(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let no_redirects = WINHTTP_DISABLE_REDIRECTS;
        check(WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&no_redirects.to_ne_bytes()),
        ))?;
        check(WinHttpSendRequest(request.0, None, None, 0, 0, 0))?;
        check(WinHttpReceiveResponse(request.0, std::ptr::null_mut()))?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        check(WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        ))?;
        let mut buffer = vec![0u16; 2048];
        let mut bytes = (buffer.len() * 2) as u32;
        let found = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buffer.as_mut_ptr().cast()),
            &mut bytes,
            std::ptr::null_mut(),
        );
        // No `Location` is an answer (the caller decides what it means); any other failure to
        // read it (a header too long for the buffer, say) is an error, not "none".
        let location = match found {
            Ok(()) => {
                let len = (bytes as usize / 2).min(buffer.len());
                Some(String::from_utf16_lossy(&buffer[..len]))
            }
            Err(e) if e.code() == HRESULT::from_win32(ERROR_WINHTTP_HEADER_NOT_FOUND) => None,
            Err(e) => return Err(e.message()),
        };
        Ok((status, location))
    }
}
