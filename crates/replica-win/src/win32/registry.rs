//! Reading and writing values under `HKEY_CURRENT_USER` (no admin rights needed).

use std::io;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW,
    RegGetValueW, RegSetKeyValueW,
};
use windows::core::HSTRING;

fn check(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code.0 as i32))
    }
}

fn missing(code: WIN32_ERROR) -> bool {
    code == ERROR_FILE_NOT_FOUND
}

/// A DWORD value under `HKCU\<key>`, or `None` if it (or the key) isn't there.
pub fn get_dword(key: &str, value: &str) -> io::Result<Option<u32>> {
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the names outlive the call; `data` and `size` are live and `size` is its size.
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if missing(code) {
        return Ok(None);
    }
    check(code).map(|()| Some(data))
}

/// A string value under `HKCU\<key>`, or `None` if it (or the key) isn't there.
pub fn get_string(key: &str, value: &str) -> io::Result<Option<String>> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut size = 0u32;
    // SAFETY: a size query: no buffer, and `size` is a live out pointer.
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if missing(code) {
        return Ok(None);
    }
    check(code)?;
    let mut buffer = vec![0u16; (size as usize).div_ceil(2).max(1)];
    let mut bytes = (buffer.len() * 2) as u32;
    // SAFETY: the buffer is live, writable and `bytes` long.
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    check(code)?;
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..end])))
}

/// Sets a string value under `HKCU\<key>`, creating the key if needed.
pub fn set_string(key: &str, value: &str, data: &str) -> io::Result<()> {
    let wide: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the names outlive the call; `wide` is live and its length in bytes is passed.
    check(unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
            REG_SZ.0,
            Some(wide.as_ptr().cast()),
            (wide.len() * 2) as u32,
        )
    })
}

/// Deletes a value under `HKCU\<key>`. Returns false if it wasn't there.
pub fn delete_value(key: &str, value: &str) -> io::Result<bool> {
    // SAFETY: the names outlive the call.
    let code = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(value),
        )
    };
    if missing(code) {
        return Ok(false);
    }
    check(code).map(|()| true)
}

/// Deletes `HKCU\<key>` with everything under it. Returns false if it wasn't there.
pub fn delete_tree(key: &str) -> io::Result<bool> {
    // SAFETY: the name outlives the call.
    let code = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    if missing(code) {
        return Ok(false);
    }
    check(code).map(|()| true)
}
