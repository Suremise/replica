//! `replica.exe`: the command line, a console program. See [`replica::cli`].

#[cfg(windows)]
fn main() {
    std::process::exit(replica::cli::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("{}", replica_core::i18n::tr("app-windows-only"));
    std::process::exit(replica_core::run::EXIT_USAGE);
}
