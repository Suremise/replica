//! The update check's logic: which release is the latest, whether it's newer than this one,
//! and when to ask. Platform-free: the Windows layer makes the one request ([`Http`]). The tray
//! asks when it starts and once a day while it runs, only while the user has "Check for
//! updates" on ([`Checker`]), and whenever the user checks by hand.
//!
//! The request is for [`LATEST_URL`], with redirects not followed: GitHub answers with a
//! redirect to the latest release's page (`…/releases/tag/v0.2.0`), so its `Location` says the
//! version, and that page is the link the user is shown. No API, no JSON, nothing downloaded.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

/// The project's latest release on GitHub, which redirects to that release's page.
pub const LATEST_URL: &str = "https://github.com/Suremise/replica/releases/latest";

/// Where every release is listed: the link when the latest can't be told.
pub const RELEASES_URL: &str = "https://github.com/Suremise/replica/releases";

/// A release version: major, minor, patch. A pre-release (`0.2.0-beta.1`) counts as before
/// its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The text after `-`, if any.
    pub pre: Option<String>,
}

impl Version {
    /// Reads `0.2.0`, `v0.2.0` or `0.2.0-beta.1` (a `+build` suffix is ignored).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_owned())),
            Some(_) => return None,
            None => (text, None),
        };
        let mut parts = core.split('.');
        let mut number = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let (major, minor, patch) = (number()?, number()?, number()?);
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// Whether this version comes after `other`.
    pub fn is_newer_than(&self, other: &Version) -> bool {
        let a = (self.major, self.minor, self.patch);
        let b = (other.major, other.minor, other.patch);
        if a != b {
            return a > b;
        }
        // The same numbers: a release comes after its pre-releases.
        match (&self.pre, &other.pre) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A newer release, and its page.
    Newer { version: Version, url: String },
    /// This is the latest (or newer).
    UpToDate,
}

/// The latest release from the redirect GitHub answered [`LATEST_URL`] with: its `Location`
/// (`https://github.com/Suremise/replica/releases/tag/v0.2.0`). `None` if it doesn't name a
/// release of this project (no release yet redirects back to the releases list).
pub fn latest_from_location(location: &str) -> Option<(Version, String)> {
    let location = location.trim();
    let tag = location.strip_prefix("https://github.com/Suremise/replica/releases/tag/")?;
    let tag = tag.split(['?', '#']).next()?;
    if tag.is_empty() || tag.contains('/') {
        return None;
    }
    Some((Version::parse(tag)?, location.to_owned()))
}

/// Compares the latest release with `current` (this build's version).
pub fn outcome(current: &str, latest: Version, url: String) -> Option<Outcome> {
    let current = Version::parse(current)?;
    Some(if latest.is_newer_than(&current) {
        Outcome::Newer {
            version: latest,
            url,
        }
    } else {
        Outcome::UpToDate
    })
}

/// The path of [`LATEST_URL`] on `github.com`, for the request.
pub const LATEST_PATH: &str = "/Suremise/replica/releases/latest";

/// How often the tray asks again while it runs, with the check on.
pub const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What came back from the request: its status, and its `Location` if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u32,
    pub location: Option<String>,
}

/// The one request: `GET` [`LATEST_URL`] with redirects off. `replica-win` makes it with
/// WinHTTP; tests give a fake, so none reaches the network.
pub trait Http: Send + Sync {
    /// The answer, or why there wasn't one in the system's words (offline, a timeout).
    fn latest(&self) -> Result<Answer, String>;
}

/// Why a check didn't work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// No answer: the system's reason (no connection, a timeout).
    Request(String),
    /// GitHub answered with something other than a redirect.
    Status(u32),
    /// This build's version can't be read.
    Version(String),
    /// GitHub's redirect named no release of Replica: no `Location`, or one that's neither a
    /// release's page nor the releases list (a sign-in page, a tag that isn't a version).
    Unrecognised(Option<String>),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Request(reason) => f.write_str(reason),
            CheckError::Status(status) => {
                f.write_str(&crate::tr!("update-failed-status", status = *status))
            }
            CheckError::Version(version) => f.write_str(&crate::tr!(
                "update-failed-version",
                version = version.as_str()
            )),
            CheckError::Unrecognised(_) => f.write_str(&crate::tr!("update-failed-answer")),
        }
    }
}

/// Asks through `http` for the latest release and compares it with `current` (this build's
/// version). Only a redirect to the releases list (no release yet) is up to date without a
/// release named; any other redirect that names none is [`CheckError::Unrecognised`], never
/// "the latest". Blocks for as long as the request: call it off the UI thread.
pub fn check(http: &dyn Http, current: &str) -> Result<Outcome, CheckError> {
    let answer = http.latest().map_err(CheckError::Request)?;
    if !(300..400).contains(&answer.status) {
        return Err(CheckError::Status(answer.status));
    }
    let location = answer.location.as_deref().map(str::trim);
    if location.is_some_and(is_releases_list) {
        return Ok(Outcome::UpToDate);
    }
    let Some((version, url)) = location.and_then(latest_from_location) else {
        return Err(CheckError::Unrecognised(answer.location));
    };
    outcome(current, version, url).ok_or_else(|| CheckError::Version(current.to_owned()))
}

/// Whether `location` is the project's releases list, where GitHub sends `…/releases/latest`
/// before there's a release.
fn is_releases_list(location: &str) -> bool {
    let location = location.split(['?', '#']).next().unwrap_or_default();
    location.trim_end_matches('/') == RELEASES_URL
}

/// A check's result, and whether the user asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub by_hand: bool,
    pub result: Result<Outcome, CheckError>,
}

/// When the tray asks: [`Checker::tick`] starts a check when the setting is on and none has
/// run for [`EVERY`] (so at the tray's start, then daily), and [`Checker::by_hand`] whenever the
/// user asks. Each check runs on a thread of its own, one at a time; [`Checker::poll`] takes
/// its result.
pub struct Checker {
    http: Arc<dyn Http>,
    current: String,
    /// When the last automatic check started.
    last: Option<Instant>,
    running: Option<(bool, Receiver<Result<Outcome, CheckError>>)>,
}

impl Checker {
    pub fn new(http: Arc<dyn Http>, current: impl Into<String>) -> Self {
        Self {
            http,
            current: current.into(),
            last: None,
            running: None,
        }
    }

    /// Starts an automatic check if `on` and it's due at `now`. True if it started one.
    pub fn tick(&mut self, on: bool, now: Instant) -> bool {
        if !on || self.running.is_some() || self.last.is_some_and(|l| now - l < EVERY) {
            return false;
        }
        self.last = Some(now);
        self.start(false);
        true
    }

    /// Starts a check the user asked for, whatever the setting, unless one is running (whose
    /// result then counts as theirs).
    pub fn by_hand(&mut self) {
        match &mut self.running {
            Some((by_hand, _)) => *by_hand = true,
            None => self.start(true),
        }
    }

    /// Whether a check is running.
    pub fn checking(&self) -> bool {
        self.running.is_some()
    }

    /// The finished check's result, once.
    pub fn poll(&mut self) -> Option<Checked> {
        let (by_hand, receive) = self.running.as_ref()?;
        let result = match receive.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(CheckError::Request(crate::tr!("update-failed-stopped")))
            }
        };
        let by_hand = *by_hand;
        self.running = None;
        Some(Checked { by_hand, result })
    }

    fn start(&mut self, by_hand: bool) {
        let (send, receive) = channel();
        let (http, current) = (Arc::clone(&self.http), self.current.clone());
        std::thread::spawn(move || {
            let _ = send.send(check(http.as_ref(), &current));
        });
        self.running = Some((by_hand, receive));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn reads_versions_with_or_without_a_v() {
        assert_eq!(
            v("v0.2.0"),
            Version {
                major: 0,
                minor: 2,
                patch: 0,
                pre: None
            }
        );
        assert_eq!(v("1.10.3").to_string(), "1.10.3");
        assert_eq!(v("0.2.0-beta.1").pre.as_deref(), Some("beta.1"));
        assert_eq!(v("0.2.0+abc").to_string(), "0.2.0");
        for bad in [
            "", "v", "1.2", "1.2.3.4", "1.x.3", "1.2.3-", "latest", "-1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn newer_compares_numbers_then_pre_releases() {
        assert!(v("0.2.0").is_newer_than(&v("0.1.9")));
        assert!(v("0.10.0").is_newer_than(&v("0.9.0")));
        assert!(v("1.0.0").is_newer_than(&v("0.99.99")));
        assert!(!v("0.1.0").is_newer_than(&v("0.1.0")));
        assert!(!v("0.1.0").is_newer_than(&v("0.2.0")));
        assert!(v("0.2.0").is_newer_than(&v("0.2.0-beta.1")));
        assert!(!v("0.2.0-beta.1").is_newer_than(&v("0.2.0")));
        assert!(v("0.2.0-beta.2").is_newer_than(&v("0.2.0-beta.1")));
    }

    #[test]
    fn the_latest_comes_from_the_redirect() {
        let url = "https://github.com/Suremise/replica/releases/tag/v0.2.0";
        assert_eq!(
            latest_from_location(url),
            Some((v("0.2.0"), url.to_owned()))
        );
        // No release yet: GitHub sends the list back.
        assert_eq!(
            latest_from_location("https://github.com/Suremise/replica/releases"),
            None
        );
        // Another project's release, or something that isn't a version, isn't taken.
        assert_eq!(
            latest_from_location("https://github.com/someone/else/releases/tag/v9.0.0"),
            None
        );
        assert_eq!(
            latest_from_location("https://github.com/Suremise/replica/releases/tag/nightly"),
            None
        );
    }

    /// Answers as GitHub would, counting the requests.
    struct Fake {
        answer: Result<Answer, String>,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl Fake {
        fn redirect(to: &str) -> Arc<Fake> {
            Arc::new(Fake {
                answer: Ok(Answer {
                    status: 302,
                    location: Some(to.to_owned()),
                }),
                asked: Default::default(),
            })
        }

        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Http for Fake {
        fn latest(&self) -> Result<Answer, String> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.answer.clone()
        }
    }

    const V040: &str = "https://github.com/Suremise/replica/releases/tag/v0.4.0";

    #[test]
    fn a_check_reads_the_redirect_and_says_why_it_failed() {
        assert_eq!(
            check(Fake::redirect(V040).as_ref(), "0.3.0"),
            Ok(Outcome::Newer {
                version: v("0.4.0"),
                url: V040.to_owned()
            })
        );
        assert_eq!(
            check(Fake::redirect(V040).as_ref(), "0.4.0"),
            Ok(Outcome::UpToDate)
        );
        // No release yet: GitHub sends the list back.
        assert_eq!(
            check(Fake::redirect(RELEASES_URL).as_ref(), "0.3.0"),
            Ok(Outcome::UpToDate)
        );
        let offline = Fake {
            answer: Err("The server name or address could not be resolved".into()),
            asked: Default::default(),
        };
        assert_eq!(
            check(&offline, "0.3.0"),
            Err(CheckError::Request(
                "The server name or address could not be resolved".into()
            ))
        );
        let not_found = Fake {
            answer: Ok(Answer {
                status: 404,
                location: None,
            }),
            asked: Default::default(),
        };
        let error = check(&not_found, "0.3.0").unwrap_err();
        assert_eq!(error, CheckError::Status(404));
        assert!(error.to_string().contains("404"), "{error}");
        assert_eq!(
            check(Fake::redirect(V040).as_ref(), "dev"),
            Err(CheckError::Version("dev".into()))
        );
    }

    #[test]
    fn a_redirect_that_names_no_release_is_never_the_latest() {
        // The releases list, as GitHub sends it before the first release.
        for list in [
            RELEASES_URL.to_owned(),
            format!("{RELEASES_URL}/"),
            format!("{RELEASES_URL}?page=1"),
        ] {
            assert_eq!(
                check(Fake::redirect(&list).as_ref(), "0.3.0"),
                Ok(Outcome::UpToDate),
                "{list}"
            );
        }
        // Anything else names no release: the check failed, it didn't find this the latest.
        for unread in [
            "https://github.com/login",
            "https://github.com/Suremise/replica/releases/tag/nightly",
            "https://github.com/someone/else/releases/tag/v9.0.0",
            "https://github.com/Suremise/replica/releases/other",
        ] {
            let error = check(Fake::redirect(unread).as_ref(), "0.3.0").unwrap_err();
            assert_eq!(error, CheckError::Unrecognised(Some(unread.into())));
            assert_eq!(
                error.to_string(),
                "GitHub's answer didn't name a release of Replica."
            );
        }
        let no_location = Fake {
            answer: Ok(Answer {
                status: 302,
                location: None,
            }),
            asked: Default::default(),
        };
        assert_eq!(
            check(&no_location, "0.3.0"),
            Err(CheckError::Unrecognised(None))
        );
    }

    /// Waits for the running check's result (each runs on a thread).
    fn finished(checker: &mut Checker) -> Checked {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(c) = checker.poll() {
                return c;
            }
            assert!(Instant::now() < end, "the check finished");
            std::thread::yield_now();
        }
    }

    #[test]
    fn with_the_setting_off_nothing_is_asked() {
        let fake = Fake::redirect(V040);
        let mut checker = Checker::new(fake.clone(), "0.3.0");
        let start = Instant::now();
        for hours in [0, 1, 25, 49, 24 * 30] {
            assert!(!checker.tick(false, start + Duration::from_secs(hours * 3600)));
        }
        assert!(!checker.checking());
        assert_eq!(checker.poll(), None);
        assert_eq!(fake.asked(), 0);
    }

    #[test]
    fn with_it_on_the_tray_asks_at_its_start_then_once_a_day() {
        let fake = Fake::redirect(V040);
        let mut checker = Checker::new(fake.clone(), "0.3.0");
        let start = Instant::now();
        assert!(checker.tick(true, start));
        assert!(!checker.tick(true, start), "one at a time");
        let checked = finished(&mut checker);
        assert!(!checked.by_hand);
        assert_eq!(
            checked.result,
            Ok(Outcome::Newer {
                version: v("0.4.0"),
                url: V040.to_owned()
            })
        );
        assert!(!checker.tick(true, start + Duration::from_secs(60)));
        assert!(!checker.tick(true, start + EVERY - Duration::from_secs(1)));
        assert_eq!(fake.asked(), 1);
        assert!(checker.tick(true, start + EVERY));
        finished(&mut checker);
        assert_eq!(fake.asked(), 2);
    }

    #[test]
    fn a_check_by_hand_asks_whatever_the_setting() {
        let fake = Fake::redirect(V040);
        let mut checker = Checker::new(fake.clone(), "0.4.0");
        checker.by_hand();
        let checked = finished(&mut checker);
        assert!(checked.by_hand);
        assert_eq!(checked.result, Ok(Outcome::UpToDate));
        assert_eq!(fake.asked(), 1);
        // Asked while an automatic one runs: that one's result is theirs.
        assert!(checker.tick(true, Instant::now()));
        checker.by_hand();
        assert!(finished(&mut checker).by_hand);
        assert_eq!(fake.asked(), 2);
    }

    #[test]
    fn the_outcome_says_newer_only_when_it_is() {
        let url = "https://github.com/Suremise/replica/releases/tag/v0.2.0".to_owned();
        assert_eq!(
            outcome("0.1.0", v("0.2.0"), url.clone()),
            Some(Outcome::Newer {
                version: v("0.2.0"),
                url: url.clone()
            })
        );
        assert_eq!(
            outcome("0.2.0", v("0.2.0"), url.clone()),
            Some(Outcome::UpToDate)
        );
        assert_eq!(outcome("0.3.0", v("0.2.0"), url), Some(Outcome::UpToDate));
    }
}
