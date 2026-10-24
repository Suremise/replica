//! Replica's platform-free core.
//!
//! Everything here builds and tests on any OS. The platform is reached only
//! through small traits that `replica-win` implements ([`platform`]).

pub mod cancel;
pub mod exclude;
pub mod history;
pub mod job;
pub mod jobs_file;
pub mod mirror;
pub mod paths;
pub mod platform;
pub mod process;
pub mod repo_lock;
pub mod restic;
pub mod run;
pub mod schedule;
pub mod settings;
pub mod snapshot;
pub mod steps;
pub mod test_restore;

pub mod i18n;

/// Replica's version, as built.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
