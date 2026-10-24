//! What Replica's two exes share: the command line ([`cli`]), the tray ([`tray`]) and the
//! window ([`ui`]).

#[cfg(windows)]
pub mod cli;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
pub mod ui;
