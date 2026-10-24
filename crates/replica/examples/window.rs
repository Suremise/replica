//! Replica's window on its own, for trying it by hand or with a screen reader, without the
//! tray, so nothing real is touched:
//!
//! - the app's folder is a temp folder (or the one given), with `settings.toml` in it;
//! - Credential Manager entries go under `ReplicaTest-<pid>-window/`, and Task Scheduler tasks
//!   in `\ReplicaTest-<pid>-window`, both removed when it ends. To open jobs `replica.exe` set
//!   up, give it their `ReplicaTest-…` prefix in `REPLICA_CREDENTIAL_PREFIX`: that prefix is
//!   used and left for its owner to remove;
//! - "Run" requests are printed, not run.
//!
//! ```text
//! cargo run -p replica --example window [folder]
//! ```
//!
//! Closing the window hides it, as in the tray. Creating `show` in the folder shows it again;
//! creating `quit` ends the example.

#[cfg(windows)]
fn main() {
    use std::sync::Arc;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use replica::ui::Window;
    use replica::ui::backend::LiveBackend;
    use replica_core::platform::SecretStore;
    use replica_win::credentials::CredentialManager;
    use replica_win::paths::AppPaths;
    use replica_win::scheduler::Scheduler;

    replica_win::tray::set_dpi_aware();
    replica_core::i18n::init(&replica_win::language::display_language());

    let temp = tempfile::tempdir().expect("a temp folder");
    let folder = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temp.path().to_path_buf());
    let paths = AppPaths::resolve("Replica", &folder, Some(&folder)).expect("the app's folder");
    let tag = format!("ReplicaTest-{}-window", std::process::id());
    // A test prefix of `replica.exe`'s (`REPLICA_CREDENTIAL_PREFIX`), to open jobs it set up.
    let prefix = std::env::var(replica::cli::CREDENTIAL_PREFIX_VAR)
        .ok()
        .filter(|p| p.starts_with("ReplicaTest-"))
        .unwrap_or_else(|| format!("{tag}/"));
    let secrets = CredentialManager::with_prefix(&prefix);
    let scheduler = Scheduler::in_folder(&format!(r"\{tag}"));
    println!("app folder: {}", paths.root().display());

    let (send, requests) = channel();
    let backend = LiveBackend::with(
        paths.clone(),
        send,
        Arc::default(),
        secrets.clone(),
        scheduler.clone(),
    );
    let window = Window::start(Arc::new(backend));
    loop {
        while let Ok(request) = requests.try_recv() {
            println!("request: {request:?}");
        }
        if let Some(error) = window.failed() {
            eprintln!("the window failed: {error}");
            break;
        }
        if std::fs::remove_file(folder.join("show")).is_ok() {
            window.show();
        }
        let followed = window.follow_up(std::time::Instant::now());
        if followed != replica::ui::FollowUp::None {
            println!("show: {followed:?}");
        }
        if std::fs::remove_file(folder.join("quit")).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Only its own prefix: one lent by `REPLICA_CREDENTIAL_PREFIX` is its owner's to remove.
    if prefix.starts_with(&tag) {
        for name in secrets.names().unwrap_or_default() {
            let _ = secrets.delete(&name);
        }
    }
    let _ = scheduler.delete_all();
    std::process::exit(0);
}

#[cfg(not(windows))]
fn main() {}
