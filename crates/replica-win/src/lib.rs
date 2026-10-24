//! Replica's Windows layer.
//!
//! It implements `replica-core`'s platform traits. Everything is behind
//! `cfg(windows)`, so the crate is empty on other platforms.

#[cfg(windows)]
pub mod autostart;
#[cfg(windows)]
pub mod console;
#[cfg(windows)]
pub mod credentials;
#[cfg(windows)]
pub mod files;
#[cfg(windows)]
pub mod installer;
#[cfg(windows)]
pub mod jobs;
#[cfg(windows)]
pub mod keys;
#[cfg(windows)]
pub mod language;
#[cfg(windows)]
pub mod logging;
#[cfg(windows)]
pub mod notify;
#[cfg(windows)]
pub mod paths;
#[cfg(windows)]
pub mod repo_locks;
#[cfg(windows)]
pub mod scheduler;
#[cfg(windows)]
pub mod targets;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
mod win32;
#[cfg(windows)]
pub mod window;
