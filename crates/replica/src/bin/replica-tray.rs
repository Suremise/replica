//! `replica-tray.exe`: the tray and the window, a GUI program, so a
//! scheduled run never flashes a console window. See [`replica::tray`].

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    std::process::exit(replica::tray::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("{}", replica_core::i18n::tr("app-windows-only"));
    std::process::exit(replica_core::run::EXIT_USAGE);
}
