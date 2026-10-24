//! The user's Windows display language, as a BCP 47 tag (`en-GB`, `fr-FR`), for
//! `replica_core::i18n::init`.

use crate::win32::locale;

/// The tag used when Windows gives none.
pub const FALLBACK: &str = "en";

/// The user's display language: the first of their preferred UI languages, else the default UI
/// language's name, else [`FALLBACK`].
pub fn display_language() -> String {
    locale::preferred_ui_languages()
        .and_then(|list| first_of_multi_sz(&list))
        .or_else(|| locale::default_ui_language().and_then(|name| tag(&name)))
        .unwrap_or_else(|| FALLBACK.to_owned())
}

/// The first string of a double-null-terminated list, if it isn't empty.
pub fn first_of_multi_sz(buffer: &[u16]) -> Option<String> {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    tag(&buffer[..end])
}

fn tag(name: &[u16]) -> Option<String> {
    let name = String::from_utf16(name).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn the_first_of_a_list_is_taken() {
        assert_eq!(
            first_of_multi_sz(&wide("en-GB\0fr-FR\0\0")).as_deref(),
            Some("en-GB")
        );
    }

    #[test]
    fn an_empty_list_gives_none() {
        assert_eq!(first_of_multi_sz(&wide("\0\0")), None);
        assert_eq!(first_of_multi_sz(&[]), None);
    }

    #[test]
    fn the_display_language_is_a_tag() {
        let tag = display_language();
        assert!(!tag.is_empty());
        assert!(
            tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{tag:?}"
        );
    }
}
