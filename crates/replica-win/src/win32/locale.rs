//! The user's UI languages, by name.

use windows::Win32::Globalization::{
    GetUserDefaultUILanguage, GetUserPreferredUILanguages, LCIDToLocaleName, MUI_LANGUAGE_NAME,
};
use windows::core::PWSTR;

/// `GetUserPreferredUILanguages`' list by name, as Windows gives it (double-null-terminated).
pub fn preferred_ui_languages() -> Option<Vec<u16>> {
    let mut count = 0u32;
    let mut len = 0u32;
    // SAFETY: a null buffer asks for the length it needs, in `len`; both counts are live.
    unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len) }.ok()?;
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: `buffer` holds the `len` characters asked for just above.
    unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
    }
    .ok()?;
    Some(buffer)
}

/// `GetUserDefaultUILanguage`'s LCID as a locale name, without its terminating null.
pub fn default_ui_language() -> Option<Vec<u16>> {
    // SAFETY: it takes nothing.
    let lcid = unsafe { GetUserDefaultUILanguage() };
    let mut buffer = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    // SAFETY: the slice gives the buffer and its length.
    let len = unsafe { LCIDToLocaleName(u32::from(lcid), Some(&mut buffer), 0) };
    let len = usize::try_from(len).ok()?.checked_sub(1)?;
    Some(buffer[..len].to_vec())
}
