//! `replica.exe` end to end, with the pinned restic, in temp folders only.
//!
//! Each test has its own `APPDATA` (so its own settings, history, logs and staging) and its own
//! Credential Manager name prefix (`REPLICA_CREDENTIAL_PREFIX`), whose entries it deletes when
//! it ends. Drive targets point at the temp folder's own volume, by its volume ID, so they
//! resolve the real way.
//!
//! The tests run side by side, as separate processes under nextest: Credential Manager
//! once lost a write when two processes wrote at once, even to different names, and every call
//! into it now takes a per-user named mutex and reads a change back.

#![cfg(windows)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use replica_core::platform::SecretStore;
use replica_win::credentials::CredentialManager;

struct Fixture {
    temp: tempfile::TempDir,
    prefix: String,
    restic: Option<PathBuf>,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("appdata/Replica")).unwrap();
        Fixture {
            prefix: format!("ReplicaTest-{}-{name}/", std::process::id()),
            restic: restic(),
            temp,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.temp.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn settings(&self, text: &str) {
        self.write("appdata/Replica/settings.toml", text);
    }

    /// A drive target on the temp folder's own volume, at `rel` in the temp folder.
    fn drive_target(&self, rel: &str) -> String {
        let full = std::fs::canonicalize(self.temp.path()).unwrap().join(rel);
        let text = full
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        let root = &text[..3];
        let id = replica_win::targets::volume_id_of(root).unwrap();
        format!(
            "kind = \"drive\"\nvolume_id = '{id}'\npath = '{}'\n",
            &text[3..]
        )
    }

    /// Runs `replica.exe` with `args`, `stdin` fed to it: (exit code, stdout, stderr).
    fn replica(&self, args: &[&str], stdin: &str) -> (i32, String, String) {
        self.replica_on(None, args, stdin)
    }

    /// Another PC's Credential Manager: under this fixture's prefix, so it's cleaned up too.
    fn prefix_of(&self, pc: Option<&str>) -> String {
        match pc {
            None => self.prefix.clone(),
            Some(pc) => format!("{}{pc}/", self.prefix),
        }
    }

    /// As [`Self::replica`], on another PC if `pc` is given: its own `APPDATA`
    /// (`<pc>/appdata`) and Credential Manager ([`Self::prefix_of`]).
    fn replica_on(&self, pc: Option<&str>, args: &[&str], stdin: &str) -> (i32, String, String) {
        let appdata = match pc {
            None => self.path("appdata"),
            Some(pc) => self.path(&format!("{pc}/appdata")),
        };
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_replica"));
        cmd.args(args)
            .env("APPDATA", appdata)
            // restic keeps its cache under LOCALAPPDATA: in the temp folder, not the user's.
            .env("LOCALAPPDATA", self.path("localappdata"))
            .env(replica::cli::CREDENTIAL_PREFIX_VAR, self.prefix_of(pc))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(restic) = &self.restic {
            cmd.env("REPLICA_RESTIC", restic);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let (code, out, err) = self.replica(args, "");
        assert!(code == 0 || code == 3, "{args:?}: {code}\n{out}\n{err}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}\n{out}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let store = CredentialManager::with_prefix(&self.prefix);
        for name in store.names().unwrap_or_default() {
            let _ = store.delete(&name);
        }
    }
}

/// The pinned restic, as the core tests find it.
fn restic() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("REPLICA_RESTIC") {
        return Some(PathBuf::from(path));
    }
    let bundled =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/restic/bin/restic.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    assert!(
        std::env::var("REPLICA_REQUIRE_RESTIC").as_deref() != Ok("1"),
        "restic is required (REPLICA_REQUIRE_RESTIC=1) but wasn't found"
    );
    None
}

fn the_password(init_output: &str) -> String {
    init_output
        .lines()
        .map(str::trim)
        .find(|l| l.len() == 35 && l.chars().filter(|&c| c == '-').count() == 5)
        .unwrap_or_else(|| panic!("no password shown in:\n{init_output}"))
        .to_string()
}

#[test]
fn init_run_snapshots_restore_verify_and_key_show() {
    let f = Fixture::new("flow");
    if f.restic.is_none() {
        return;
    }
    f.write("src/a.txt", "a version 1");
    f.write("src/sub/b.txt", "b");
    f.settings(&format!(
        "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = '{}'\nexclude = []\n[[jobs.targets]]\n{}\n\
         [[jobs.after]]\ntype = \"verify\"\nread_data_subset = \"100%\"\n",
        f.path("src").display(),
        f.drive_target("drive/repo")
    ));

    // init: shown once, stored only once confirmed.
    let (code, out, err) = f.replica(&["init", "hive"], "saved\n");
    assert_eq!(code, 0, "{out}\n{err}");
    let password = the_password(&out);
    assert!(
        f.path("drive/repo/config").is_file(),
        "the repository is made"
    );
    let guide = std::fs::read_to_string(f.path("drive/repo/HOW-TO-RESTORE.txt")).unwrap();
    assert!(guide.contains("restic -r"), "{guide}");
    assert!(guide.contains("repo"), "it names this repository: {guide}");
    assert!(!guide.contains(&password), "and never the password");
    let (code, out, _) = f.replica(&["init", "hive"], "");
    assert_eq!(code, 0);
    assert!(out.contains("set up already"), "{out}");

    // key show: the same password again.
    let (code, out, _) = f.replica(&["key", "show", "hive"], "");
    assert_eq!(code, 0);
    assert!(out.contains(&password), "{out}");

    // run --json, then snapshots: version 1 is backed up.
    let run = f.json(&["run", "hive", "--json"]);
    assert_eq!(run["schema"], 1);
    assert_eq!(run["outcome"], "ok");
    assert_eq!(run["exit_code"], 0);
    assert_eq!(run["jobs"][0]["job"], "hive");
    assert_eq!(run["jobs"][0]["targets"][0]["mode"], "versioned");
    assert!(
        run["jobs"][0]["targets"][0]["snapshot_id"].is_string(),
        "{run}"
    );
    assert!(run["jobs"][0]["log"].is_string());
    let snaps = f.json(&["snapshots", "hive", "--json"]);
    assert_eq!(snaps["schema"], 1);
    assert_eq!(snaps["snapshots"].as_array().unwrap().len(), 1, "{snaps}");
    assert_eq!(
        snaps["snapshots"][0]["id"],
        run["jobs"][0]["targets"][0]["snapshot_id"]
    );
    let oldest = snaps["snapshots"][0]["id"].as_str().unwrap().to_string();
    // The source moves on to version 2 (restored from version 1's snapshot below).
    f.write("src/a.txt", "a version 2");

    // restore: an older version, into a new folder; never over the original without --overwrite.
    let a = f.path("src/a.txt");
    let to = f.path("restored");
    let (code, out, err) = f.replica(
        &[
            "restore",
            "hive",
            &oldest,
            a.to_str().unwrap(),
            "--to",
            to.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        std::fs::read_to_string(to.join("a.txt")).unwrap(),
        "a version 1"
    );
    let src = f.path("src");
    let (code, _, err) = f.replica(
        &[
            "restore",
            "hive",
            &oldest,
            a.to_str().unwrap(),
            "--to",
            src.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 1, "refused: {err}");
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "a version 2",
        "the original is untouched"
    );
    let (code, _, err) = f.replica(
        &[
            "restore",
            "hive",
            &oldest,
            a.to_str().unwrap(),
            "--to",
            src.to_str().unwrap(),
            "--overwrite",
        ],
        "",
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "a version 1",
        "--overwrite restores in place"
    );

    // run (human) and --quiet.
    let (code, out, err) = f.replica(&["run", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("hive: OK"), "{out}");
    let (code, out, err) = f.replica(&["run", "hive", "--quiet"], "");
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (0, "", ""),
        "--quiet prints nothing on success"
    );

    // verify
    let (code, out, err) = f.replica(&["verify", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("checks out"), "{out}");

    // list and status
    let list = f.json(&["list", "--json"]);
    assert_eq!(list["schema"], 1);
    assert_eq!(list["jobs"][0]["name"], "hive");
    assert_eq!(list["jobs"][0]["mode"], "versioned");
    assert_eq!(list["jobs"][0]["last_run"]["outcome"], "ok");
    assert!(list["jobs"][0]["last_success"].is_string());
    assert!(
        list["jobs"][0].get("running").is_none(),
        "running is status's"
    );
    let status = f.json(&["status", "--json"]);
    assert_eq!(status["jobs"][0]["running"], false);
}

#[test]
fn a_missing_target_is_exit_3_and_run_all_takes_the_worst() {
    let f = Fixture::new("missing");
    f.write("src/a.txt", "a");
    f.settings(&format!(
        "[[jobs]]\nname = \"away\"\nmode = \"mirror\"\nmirror_deletes_confirmed = true\n\
         [[jobs.sources]]\npath = '{src}'\n[[jobs.targets]]\nkind = \"drive\"\nlabel = \"REPLICA-NOT-CONNECTED\"\npath = 'x'\n\
         [[jobs]]\nname = \"here\"\nmode = \"mirror\"\nmirror_deletes_confirmed = true\n\
         [[jobs.sources]]\npath = '{src}'\n[[jobs.targets]]\n{here}\n\
         [[jobs]]\nname = \"off\"\nenabled = false\nmode = \"mirror\"\nmirror_deletes_confirmed = true\n\
         [[jobs.sources]]\npath = '{src}'\n[[jobs.targets]]\n{off}\n",
        src = f.path("src").display(),
        here = f.drive_target("drive/mirror"),
        // Each mirror has a folder of its own: two may not share one.
        off = f.drive_target("drive/mirror-off")
    ));
    let (code, _, err) = f.replica(&["run", "away"], "");
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("isn't available"), "{err}");
    let (code, out, _) = f.replica(&["run", "here"], "");
    assert_eq!(code, 0, "{out}");
    let run = f.json(&["run", "--all", "--json"]);
    assert_eq!(run["exit_code"], 3, "ok and skipped: skipped wins");
    let jobs: Vec<&str> = run["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["job"].as_str().unwrap())
        .collect();
    assert_eq!(jobs, ["away", "here"], "--all leaves out jobs that are off");
}

#[test]
fn usage_and_settings_errors_are_exit_4() {
    let f = Fixture::new("usage");
    f.settings("[app\n");
    let (code, _, err) = f.replica(&["list"], "");
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("line 1"), "a clear error: {err}");
    // Its turn ends before the next fixture's starts.
    drop(f);

    let f = Fixture::new("usage2");
    f.settings("");
    for args in [
        &["run"][..],
        &["run", "a", "--all"],
        &["nonsense"],
        &["run", "no-such-job"],
        &["snapshots"],
    ] {
        let (code, _, err) = f.replica(args, "");
        assert_eq!(code, 4, "{args:?}: {err}");
    }
    let (code, out, _) = f.replica(&["--help"], "");
    assert_eq!(code, 0);
    assert!(out.contains("Exit codes"), "{out}");
    let (code, out, _) = f.replica(&["run", "--all"], "");
    assert_eq!(code, 0, "no jobs is nothing to do: {out}");
}

#[test]
fn init_stores_nothing_unless_confirmed() {
    let f = Fixture::new("noconfirm");
    if f.restic.is_none() {
        return;
    }
    f.write("src/a.txt", "a");
    f.settings(&format!(
        "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = '{}'\n[[jobs.targets]]\n{}\n",
        f.path("src").display(),
        f.drive_target("drive/repo")
    ));
    let (code, out, err) = f.replica(&["init", "hive"], "no\n");
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(err.contains("Not confirmed"), "{err}");
    assert!(!f.path("drive/repo/config").exists(), "no repository");
    let store = CredentialManager::with_prefix(&f.prefix);
    assert!(store.names().unwrap().is_empty(), "no password stored");
    let (code, _, _) = f.replica(&["key", "show", "hive"], "");
    assert_eq!(code, 1, "nothing to show");
}

/// The repository password stored on `pc`, by its name in Credential Manager.
fn stored_password(f: &Fixture, pc: Option<&str>) -> Option<String> {
    let store = CredentialManager::with_prefix(&f.prefix_of(pc));
    let name = store
        .names()
        .unwrap()
        .into_iter()
        .find(|n| n.starts_with("replica:repo:"))?;
    Some(store.get(&name).unwrap()?.secret.expose().to_owned())
}

/// A backup made on one PC is used from another with only its folder and the password the
/// user saved: `replica key set` puts it back, after checking it opens the repository, and
/// never replaces one that works unless forced.
#[test]
fn a_repository_made_on_one_pc_is_used_from_another_with_its_saved_password() {
    let f = Fixture::new("newpc");
    let Some(restic) = f.restic.clone() else {
        return;
    };
    f.write("src/a.txt", "made on the first PC");
    let settings = format!(
        "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = '{}'\n[[jobs.targets]]\n{}\n",
        f.path("src").display(),
        f.drive_target("drive/repo")
    );
    f.settings(&settings);
    let (code, out, err) = f.replica(&["init", "hive"], "saved\n");
    assert_eq!(code, 0, "{out}\n{err}");
    let password = the_password(&out);
    let (code, out, err) = f.replica(&["run", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");

    // The new PC: the same settings and the repository's folder, but no saved password.
    let pc = Some("new-pc");
    f.write("new-pc/appdata/Replica/settings.toml", &settings);
    let (code, _, err) = f.replica_on(pc, &["run", "hive"], "");
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("no saved repository password") && err.contains("replica key set"),
        "the failure says how to put it back: {err}"
    );

    let (code, out, err) = f.replica_on(pc, &["key", "set", "hive"], "not-the-password\n");
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(err.contains("doesn't open the backup"), "{err}");
    assert!(
        !out.contains("not-the-password") && !err.contains("not-the-password"),
        "what's typed isn't printed"
    );
    assert_eq!(stored_password(&f, pc), None, "a wrong one isn't saved");

    let (code, out, err) = f.replica_on(pc, &["key", "set", "hive"], &format!("{password}\n"));
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Saved the repository password"), "{out}");
    assert!(!out.contains(&password), "{out}");
    assert_eq!(stored_password(&f, pc).as_deref(), Some(password.as_str()));

    // It works from the new PC: a backup, and a restore of what the first PC backed up.
    f.write("src/a.txt", "changed on the new PC");
    let (code, out, err) = f.replica_on(pc, &["run", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    let (code, out, _) = f.replica_on(pc, &["snapshots", "hive"], "");
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 2, "both PCs' snapshots: {out}");
    let oldest = out.split_whitespace().next().unwrap().to_string();
    let a = f.path("src/a.txt");
    let to = f.path("restored");
    let (code, out, err) = f.replica_on(
        pc,
        &[
            "restore",
            "hive",
            &oldest,
            a.to_str().unwrap(),
            "--to",
            to.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        std::fs::read_to_string(to.join("a.txt")).unwrap(),
        "made on the first PC"
    );

    // A saved password that works isn't replaced, and nothing is asked for.
    let (code, out, err) = f.replica_on(pc, &["key", "set", "hive"], "");
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(err.contains("opens the backup there already"), "{err}");
    assert!(!out.contains("isn't shown as you type"), "no prompt: {out}");

    // One saved that doesn't open it (a stale entry) is replaced by one that does.
    let store = CredentialManager::with_prefix(&f.prefix_of(pc));
    let name = store
        .names()
        .unwrap()
        .into_iter()
        .find(|n| n.starts_with("replica:repo:"))
        .unwrap();
    store
        .set(
            &name,
            &replica_core::platform::Credential {
                username: None,
                secret: replica_core::platform::Secret::new("stale"),
            },
        )
        .unwrap();
    let (code, out, err) = f.replica_on(pc, &["key", "set", "hive"], &format!("{password}\n"));
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("in place of the one saved before"), "{out}");
    assert_eq!(stored_password(&f, pc).as_deref(), Some(password.as_str()));

    // --force replaces a working one, but only with another that opens the repository.
    let second = f.path("second-key.txt");
    std::fs::write(&second, "a-second-key").unwrap();
    let added = Command::new(&restic)
        .args(["key", "add", "--new-password-file"])
        .arg(&second)
        .env("RESTIC_REPOSITORY", f.path("drive/repo"))
        .env("RESTIC_PASSWORD", &password)
        .env("RESTIC_CACHE_DIR", f.path("restic-cache"))
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    let (code, _, err) = f.replica_on(pc, &["key", "set", "hive", "--force"], "wrong\n");
    assert_eq!(code, 1, "{err}");
    assert_eq!(stored_password(&f, pc).as_deref(), Some(password.as_str()));
    let (code, out, err) = f.replica_on(pc, &["key", "set", "hive", "--force"], "a-second-key\n");
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(stored_password(&f, pc).as_deref(), Some("a-second-key"));
    let (code, out, err) = f.replica_on(pc, &["run", "hive"], "");
    assert_eq!(code, 0, "it opens with the new one: {out}\n{err}");

    // The first PC still has its own, untouched.
    assert_eq!(
        stored_password(&f, None).as_deref(),
        Some(password.as_str())
    );
}

#[test]
fn key_set_needs_a_repository_and_a_versioned_job() {
    let f = Fixture::new("keyset");
    if f.restic.is_none() {
        return;
    }
    f.write("src/a.txt", "a");
    f.settings(&format!(
        "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = '{}'\n[[jobs.targets]]\n{}\n\
         [[jobs]]\nname = \"copy\"\nmode = \"mirror\"\nmirror_deletes_confirmed = true\n\
         [[jobs.sources]]\npath = '{}'\n[[jobs.targets]]\n{}\n",
        f.path("src").display(),
        f.drive_target("drive/repo"),
        f.path("src").display(),
        f.drive_target("drive/copy")
    ));
    let (code, _, err) = f.replica(&["key", "set", "hive"], "anything\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no restic repository"), "{err}");
    let (code, _, err) = f.replica(&["key", "set", "copy"], "anything\n");
    assert_eq!(code, 4, "a mirror has no password: {err}");
    let (code, _, err) = f.replica(&["key", "set", "hive", "--target", "nope"], "x\n");
    assert_eq!(code, 4, "{err}");
    assert_eq!(stored_password(&f, None), None);
}

#[test]
fn login_set_saves_a_share_login_typed_twice() {
    let f = Fixture::new("login");
    f.settings(
        "[[jobs]]\nname = \"nas\"\n[[jobs.sources]]\npath = 'C:\\Work'\n\
         [[jobs.targets]]\nkind = \"unc\"\npath = '\\\\nas\\backup'\ncredential = \"nas-login\"\n",
    );
    let store = CredentialManager::with_prefix(&f.prefix);

    let (code, out, err) = f.replica(
        &["login", "set", "nas-login", "--user", "bob"],
        "pw-1\npw-1\n",
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Saved the share login nas-login"), "{out}");
    assert!(
        !out.contains("pw-1") && !err.contains("pw-1"),
        "{out}\n{err}"
    );
    assert!(!err.contains("No share target"), "a target uses it: {err}");
    let saved = store.get("nas-login").unwrap().unwrap();
    assert_eq!(saved.username.as_deref(), Some("bob"));
    assert_eq!(saved.secret.expose(), "pw-1");

    // Asked for the user name; replacing one says so; a name no target uses is pointed out.
    let (code, out, err) = f.replica(&["login", "set", "nas-login"], "alice\npw-2\npw-2\n");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("in place of the one saved before"), "{out}");
    let saved = store.get("nas-login").unwrap().unwrap();
    assert_eq!(saved.username.as_deref(), Some("alice"));
    assert_eq!(saved.secret.expose(), "pw-2");
    let (code, _, err) = f.replica(&["login", "set", "typo", "--user", "x"], "p\np\n");
    assert_eq!(code, 0);
    assert!(err.contains("No share target"), "{err}");

    // Two different passwords, or none, save nothing.
    let (code, _, err) = f.replica(&["login", "set", "other", "--user", "x"], "a\nb\n");
    assert_eq!(code, 1, "{err}");
    let (code, _, err) = f.replica(&["login", "set", "other", "--user", "x"], "\n\n");
    assert_eq!(code, 1, "{err}");
    assert!(store.get("other").unwrap().is_none());

    // A repository password's name can't be taken by a login.
    let (code, _, err) = f.replica(
        &[
            "login",
            "set",
            "replica:repo:unc:\\\\nas\\backup",
            "--user",
            "x",
        ],
        "p\np\n",
    );
    assert_eq!(code, 4, "{err}");
}

#[test]
fn restore_latest_is_the_jobs_own_in_a_repository_two_jobs_share() {
    let f = Fixture::new("shared");
    if f.restic.is_none() {
        return;
    }
    f.write("src/a.txt", "alpha's version");
    let job = |name: &str| {
        format!(
            "[[jobs]]\nname = \"{name}\"\n[[jobs.sources]]\npath = '{}'\n[[jobs.targets]]\n{}\n",
            f.path("src").display(),
            f.drive_target("drive/repo")
        )
    };
    f.settings(&format!("{}{}", job("alpha"), job("beta")));
    let (code, out, err) = f.replica(&["init", "alpha"], "saved\n");
    assert_eq!(code, 0, "{out}\n{err}");
    let (code, out, _) = f.replica(&["init", "beta"], "");
    assert_eq!(code, 0);
    assert!(
        out.contains("set up already"),
        "one repository, one password: {out}"
    );

    assert_eq!(f.replica(&["run", "alpha"], "").0, 0);
    f.write("src/a.txt", "beta's newer version");
    let beta = f.json(&["run", "beta", "--json"]);
    let beta_id = beta["jobs"][0]["targets"][0]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    let a = f.path("src/a.txt");
    let to = f.path("restored");
    let (code, out, err) = f.replica(
        &[
            "restore",
            "alpha",
            "latest",
            a.to_str().unwrap(),
            "--to",
            to.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(
        std::fs::read_to_string(to.join("a.txt")).unwrap(),
        "alpha's version",
        "alpha's, not beta's newer"
    );

    // Beta's snapshot isn't alpha's to restore, even over the original with --overwrite.
    let (code, _, err) = f.replica(
        &[
            "restore",
            "alpha",
            &beta_id[..8],
            a.to_str().unwrap(),
            "--to",
            f.path("src").to_str().unwrap(),
            "--overwrite",
        ],
        "",
    );
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("has no snapshot"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "beta's newer version",
        "nothing was replaced"
    );

    // An item that isn't in the snapshot isn't reported restored, even with --overwrite over a
    // file of that name.
    f.write("src/missing.txt", "not in any snapshot");
    let missing = f.path("src/missing.txt");
    let (code, _, err) = f.replica(
        &[
            "restore",
            "alpha",
            "latest",
            missing.to_str().unwrap(),
            "--to",
            f.path("src").to_str().unwrap(),
            "--overwrite",
        ],
        "",
    );
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("wasn't restored"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&missing).unwrap(),
        "not in any snapshot"
    );
}

/// Pinning: `replica pin` keeps a snapshot whatever the retention, `replica snapshots`
/// shows it (and `--json` says `pinned`), and `replica unpin` lets the retention take it again.
/// `list --json` says whether a job keeps everything forever.
#[test]
fn pin_unpin_and_keep_forever_through_the_cli() {
    let f = Fixture::new("pins");
    if f.restic.is_none() {
        return;
    }
    f.write("src/a.txt", "one");
    f.settings(&format!(
        "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = '{src}'\nexclude = []\n[[jobs.targets]]\n{repo}\n\
         [jobs.retention]\ndaily = 1\nweekly = 0\nmonthly = 0\n\
         [[jobs]]\nname = \"forever\"\n[[jobs.sources]]\npath = '{src}'\nexclude = []\n[[jobs.targets]]\n{repo}\n\
         [jobs.retention]\nkeep_forever = true\n",
        src = f.path("src").display(),
        repo = f.drive_target("drive/repo")
    ));
    let (code, out, err) = f.replica(&["init", "hive"], "saved\n");
    assert_eq!(code, 0, "{out}\n{err}");
    let (code, out, err) = f.replica(&["run", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    let snaps = f.json(&["snapshots", "hive", "--json"]);
    let first = snaps["snapshots"][0]["short_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(snaps["snapshots"][0]["pinned"], false);

    let (code, out, err) = f.replica(&["pin", "hive", &first], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Pinned hive"), "{out}");
    let (_, listed, _) = f.replica(&["snapshots", "hive"], "");
    assert!(listed.contains("pinned"), "{listed}");

    // Another run keeps one snapshot, and the pinned one besides.
    f.write("src/a.txt", "two");
    let (code, out, err) = f.replica(&["run", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    let snaps = f.json(&["snapshots", "hive", "--json"]);
    let all = snaps["snapshots"].as_array().unwrap();
    assert_eq!(all.len(), 2, "{snaps}");
    let pinned: Vec<&serde_json::Value> = all.iter().filter(|s| s["pinned"] == true).collect();
    assert_eq!(pinned.len(), 1, "{snaps}");
    let pinned_id = pinned[0]["short_id"].as_str().unwrap().to_string();

    let (code, out, err) = f.replica(&["unpin", "hive", &pinned_id], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Unpinned hive"), "{out}");
    f.write("src/a.txt", "three");
    let (code, out, err) = f.replica(&["run", "hive"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    let snaps = f.json(&["snapshots", "hive", "--json"]);
    assert_eq!(
        snaps["snapshots"].as_array().unwrap().len(),
        1,
        "unpinned, the retention took it: {snaps}"
    );

    let (code, _, err) = f.replica(&["pin", "hive", "no-such-snapshot"], "");
    assert_eq!(code, 1, "{err}");

    let list = f.json(&["list", "--json"]);
    let keeps: Vec<(&str, bool)> = list["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| {
            (
                j["name"].as_str().unwrap(),
                j["keep_forever"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(keeps, [("hive", false), ("forever", true)]);
    let (_, human, _) = f.replica(&["list"], "");
    assert!(human.contains("forever · keeps everything"), "{human}");
}

/// `replica export` writes the jobs and no secret; `replica import` on another PC shows each
/// job's plan, imports them turned off and never imports one that doesn't validate.
#[test]
fn export_and_import_jobs_between_pcs() {
    use replica_core::platform::{Credential, Secret};
    use replica_core::settings::Settings;

    let f = Fixture::new("export");
    let jobs = format!(
        "[[jobs]]\nname = \"hive\"\n[[jobs.sources]]\npath = 'D:\\Work'\n[[jobs.targets]]\n{}\n\
         [[jobs.targets]]\nkind = \"unc\"\npath = '\\\\nas\\backup'\ncredential = \"nas-login\"\n\
         [[jobs]]\nname = \"photos\"\nschedule = \"daily 02:30\"\n[[jobs.sources]]\npath = 'D:\\Photos'\n\
         [[jobs.targets]]\n{}\n[[jobs.after]]\ntype = \"command\"\ncmd = \"echo done\"\n",
        f.drive_target("drive/hive"),
        f.drive_target("drive/photos"),
    );
    f.settings(&jobs);
    // This PC has hive's repository password and the share's login saved.
    let (password, login) = ("repo-secret-Q7v!", "login-secret-K2w?");
    let store = CredentialManager::with_prefix(&f.prefix);
    let settings = Settings::parse(&jobs).unwrap().settings;
    for target in &settings.jobs[0].targets {
        let secret = Credential {
            username: None,
            secret: Secret::new(password),
        };
        store
            .set(&replica_core::restic::password_secret_name(target), &secret)
            .unwrap();
    }
    let share = Credential {
        username: Some("backup-user".into()),
        secret: Secret::new(login),
    };
    store.set("nas-login", &share).unwrap();

    let file = f.path("jobs.replica-jobs.toml");
    let file_arg = file.to_str().unwrap();
    let (code, out, err) = f.replica(&["export", "--all", "-o", file_arg], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Exported 2 jobs"), "{out}");
    let text = std::fs::read_to_string(&file).unwrap();
    for secret in [password, login, "backup-user"] {
        assert!(!text.contains(secret), "{secret} in the export:\n{text}");
    }
    assert!(text.contains("credential = \"nas-login\""), "{text}");

    // It won't replace the file unless forced, and names or --all, not both or neither.
    let (code, _, err) = f.replica(&["export", "hive", "-o", file_arg], "");
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("already exists"), "{err}");
    let (code, out, err) = f.replica(&["export", "HIVE", "-o", file_arg, "--force"], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("Exported 1 job "), "{out}");
    for args in [
        &["export", "-o", file_arg][..],
        &["export", "hive", "--all", "-o", file_arg],
        &["export", "nope", "-o", file_arg],
    ] {
        let (code, _, err) = f.replica(args, "");
        assert_eq!(code, 4, "{args:?}: {err}");
    }
    let (code, _, err) = f.replica(&["export", "--all", "-o", file_arg, "--force"], "");
    assert_eq!(code, 0, "{err}");

    // Another PC, with a job called hive already and no passwords saved.
    let pc2 = "pc2/appdata/Replica/settings.toml";
    let here = format!(
        "[[jobs]]\nname = \"Hive\"\n[[jobs.sources]]\npath = 'D:\\Other'\n[[jobs.targets]]\n{}\n",
        f.drive_target("drive/other")
    );
    f.write(pc2, &here);
    let pc2_jobs = || {
        let text = std::fs::read_to_string(f.path(pc2)).unwrap();
        Settings::parse(&text).unwrap().settings.jobs
    };
    let (code, out, err) = f.replica_on(Some("pc2"), &["import", file_arg], "");
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.contains("hive: skipped, since a job here is already called Hive"),
        "{out}"
    );
    assert!(out.contains("photos: imported."), "{out}");
    assert!(out.contains("It runs the command: echo done"), "{out}");
    assert!(out.contains("has no saved repository password"), "{out}");
    let jobs = pc2_jobs();
    let names: Vec<(&str, bool)> = jobs.iter().map(|j| (j.name.as_str(), j.enabled)).collect();
    assert_eq!(
        names,
        [("Hive", true), ("photos", false)],
        "imported turned off"
    );

    let (code, out, err) = f.replica_on(
        Some("pc2"),
        &[
            "import",
            file_arg,
            "--only",
            "hive",
            "--on-conflict",
            "copy",
        ],
        "",
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("hive: imported as hive-2"), "{out}");
    let (code, out, err) = f.replica_on(
        Some("pc2"),
        &[
            "import",
            file_arg,
            "--only",
            "hive",
            "--on-conflict",
            "replace",
        ],
        "",
    );
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("in place of Hive"), "{out}");
    let jobs = pc2_jobs();
    let names: Vec<(&str, bool)> = jobs.iter().map(|j| (j.name.as_str(), j.enabled)).collect();
    assert_eq!(
        names,
        [("Hive", false), ("photos", false), ("hive-2", false)]
    );
    assert_eq!(jobs[0].sources[0].path, std::path::Path::new(r"D:\Work"));
    assert!(
        CredentialManager::with_prefix(&f.prefix_of(Some("pc2")))
            .names()
            .unwrap()
            .is_empty(),
        "importing saves no secret"
    );

    // A job that doesn't validate is never imported; settings.toml isn't a jobs file.
    let before = std::fs::read_to_string(f.path(pc2)).unwrap();
    f.write(
        "bad.replica-jobs.toml",
        "format = \"replica-jobs\"\nversion = 1\n[[jobs]]\nname = \"bad name\"\n",
    );
    let (code, _, err) = f.replica_on(
        Some("pc2"),
        &["import", f.path("bad.replica-jobs.toml").to_str().unwrap()],
        "",
    );
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("bad name: not imported"), "{err}");
    let (code, _, err) = f.replica_on(Some("pc2"), &["import", f.path(pc2).to_str().unwrap()], "");
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("isn't a file of Replica jobs"), "{err}");
    let (code, _, err) = f.replica_on(Some("pc2"), &["import", file_arg, "--only", "nope"], "");
    assert_eq!(code, 4, "{err}");
    assert_eq!(
        std::fs::read_to_string(f.path(pc2)).unwrap(),
        before,
        "nothing changed"
    );
}
