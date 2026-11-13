//! The update check's one request: a `GET` of Replica's latest-release page on GitHub
//! ([`replica_core::update::LATEST_URL`]) with redirects off, reading the redirect's
//! `Location`. Nothing is downloaded or installed; Replica shows a link.
//!
//! The request is `win32::http`'s, with WinHTTP, part of Windows: no HTTP or TLS crate ships
//! in Replica. These two modules are the only ones in Replica that touch the network, and the
//! tray only makes the request while "Check for updates" is on or when the user checks by hand
//! (`replica_core::update::Checker`).

use std::time::Duration;

use replica_core::update::{Answer, Http, LATEST_PATH};

use crate::win32::http;

/// The request, as [`WinHttp`] makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub host: &'static str,
    pub path: &'static str,
    /// Always over HTTPS, on port 443.
    pub secure: bool,
    /// Never: the redirect's `Location` is the answer.
    pub follow_redirects: bool,
    /// How long each step (resolving, connecting, sending, receiving) may take.
    pub timeout: Duration,
    /// The `User-Agent`: the app and its version, nothing about the PC.
    pub agent: String,
}

/// The one request Replica makes.
pub fn request() -> Request {
    Request {
        host: "github.com",
        path: LATEST_PATH,
        secure: true,
        follow_redirects: false,
        timeout: Duration::from_secs(15),
        agent: format!("Replica/{}", env!("CARGO_PKG_VERSION")),
    }
}

/// [`request`] with WinHTTP.
#[derive(Debug, Clone, Copy, Default)]
pub struct WinHttp;

impl Http for WinHttp {
    fn latest(&self) -> Result<Answer, String> {
        let r = request();
        // `http::redirect_location` is HTTPS with redirects off: what `r` says.
        debug_assert!(r.secure && !r.follow_redirects);
        let (status, location) = http::redirect_location(r.host, r.path, &r.agent, r.timeout)?;
        Ok(Answer { status, location })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_github_s_latest_release_with_redirects_off() {
        let r = request();
        assert_eq!(
            format!("https://{}{}", r.host, r.path),
            replica_core::update::LATEST_URL
        );
        assert!(r.secure);
        assert!(!r.follow_redirects);
        assert_eq!(r.timeout, Duration::from_secs(15));
        assert_eq!(r.agent, format!("Replica/{}", env!("CARGO_PKG_VERSION")));
    }
}
