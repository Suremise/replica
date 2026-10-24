//! The screenshot harness: renders named screens with `egui_kittest`'s wgpu renderer
//! and writes PNGs to the git-ignored `target/ui-shots/<name>/`, for reviewing the window's
//! look without running it. Each screen is shot in light and dark, at 100% and 150%, at a
//! normal and a narrow (800 points) width: `<screen>-<theme>-<width>-<scale>.png`.
//!
//! It's ignored by a plain `cargo test`; run it with
//!
//! ```text
//! $env:REPLICA_UI_SHOTS = "before"         # the folder under target/ui-shots (default: latest)
//! $env:REPLICA_UI_SCREENS = "gallery,jobs" # only these screens (default: all)
//! cargo test -p replica --test ui_shots -- --ignored
//! ```
//!
//! Screens: `gallery` (every component, [`gallery`]) and the shell with each page (`jobs`,
//! `history`, `restore`, `settings`, `about`), on a temp app folder with two jobs, against the
//! real backend with a test-only Credential Manager prefix and Task Scheduler folder (nothing
//! is made in either). New screens are added to [`screens`].
//!
//! The same screens are also driven through the accessibility tree, as plain tests: the
//! sidebar is read as tabs and switches pages, the components are named, and the table answers
//! the keyboard.

#![cfg(windows)]

#[path = "ui_shots/fake.rs"]
mod fake;
#[path = "ui_shots/gallery.rs"]
mod gallery;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use eframe::egui::Theme;
use egui_kittest::{Harness, HarnessBuilder};
use fake::{Faked, Gate, task};
use replica::ui::Shared;
use replica::ui::backend::{Backend, LastRun, LiveBackend, Status, UiRequest};
use replica::ui::geometry::WindowState;
use replica::ui::settings::Page;
use replica::ui::shell::{Shell, Tab};
use replica_core::job::{AfterStep, BeforeStep, Job, Mode, Source, Target, VolumeRef};
use replica_core::run::Outcome;
use replica_core::settings::{ScheduleMode, Settings};
use replica_win::credentials::CredentialManager;
use replica_win::paths::AppPaths;
use replica_win::scheduler::{ScheduledTask, Scheduler};

/// One way a screen is shot.
#[derive(Debug, Clone, Copy)]
struct Variant {
    theme: Theme,
    scale: f32,
    narrow: bool,
}

impl Variant {
    fn all() -> Vec<Variant> {
        let mut all = Vec::new();
        for theme in [Theme::Light, Theme::Dark] {
            for narrow in [false, true] {
                for scale in [1.0, 1.5] {
                    all.push(Variant {
                        theme,
                        scale,
                        narrow,
                    });
                }
            }
        }
        all
    }

    fn width(self) -> f32 {
        if self.narrow { 800.0 } else { 1200.0 }
    }

    fn file(self, screen: &str) -> String {
        format!(
            "{screen}-{}-{}-{}.png",
            match self.theme {
                Theme::Light => "light",
                Theme::Dark => "dark",
            },
            if self.narrow { "narrow" } else { "normal" },
            (self.scale * 100.0) as u32
        )
    }

    /// The harness puts its own margin round the screen, which [`save`] crops off.
    fn builder<S>(self, height: f32) -> HarnessBuilder<S> {
        self.sized(self.width(), height)
    }

    /// The window at its first-run size, or narrow.
    fn default_size<S>(self) -> HarnessBuilder<S> {
        let [width, height] = replica::ui::geometry::DEFAULT_SIZE;
        self.sized(if self.narrow { self.width() } else { width }, height)
    }

    fn sized<S>(self, width: f32, height: f32) -> HarnessBuilder<S> {
        Harness::builder()
            .with_size([width + 2.0 * MARGIN, height + 2.0 * MARGIN])
            .with_pixels_per_point(self.scale)
            .with_theme(self.theme)
            .with_os(eframe::egui::os::OperatingSystem::Windows)
            .with_max_steps(64)
            .with_render_cursor(false)
            .with_context_setup(|ctx| {
                replica::ui::fonts::install(ctx).unwrap();
                replica::ui::style(ctx);
            })
            .wgpu()
    }
}

/// Where the PNGs go: `target/ui-shots/<REPLICA_UI_SHOTS or latest>/`.
fn out_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let card = std::env::var("REPLICA_UI_SHOTS").unwrap_or_else(|_| "latest".into());
    target.join("ui-shots").join(card)
}

/// A temp app folder with two jobs, and the real backend on it with its tasks faked.
struct Fixture {
    _temp: tempfile::TempDir,
    backend: Arc<Faked>,
    _requests: Receiver<UiRequest>,
}

fn job(name: &str, schedule: &str, mode: Mode, source: &str, label: &str) -> Job {
    Job {
        name: name.into(),
        mode,
        mirror_deletes_confirmed: mode == Mode::Mirror,
        schedule: Some(schedule.parse().unwrap()),
        sources: vec![Source {
            path: source.into(),
            exclude: vec![],
        }],
        targets: vec![Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: None,
                label: Some(label.into()),
            },
            path: "Replica".into(),
        }],
        ..Job::default()
    }
}

impl Fixture {
    fn new() -> Fixture {
        Fixture::with(ScheduleMode::Timer, Vec::new())
    }

    /// With the schedules run by `mode`, and Task Scheduler holding `tasks`.
    fn with(mode: ScheduleMode, tasks: Vec<ScheduledTask>) -> Fixture {
        Fixture::build(mode, |faked| Faked {
            tasks: tasks.into(),
            ..faked
        })
    }

    /// With the fake's history and snapshots as `data` makes them.
    fn with_data(data: impl FnOnce(Faked) -> Faked) -> Fixture {
        Fixture::build(ScheduleMode::Timer, data)
    }

    fn build(mode: ScheduleMode, data: impl FnOnce(Faked) -> Faked) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::resolve(
            "Replica",
            &temp.path().join("exe"),
            Some(&temp.path().join("appdata")),
        )
        .unwrap();
        let (send, requests) = channel();
        let tag = format!("ReplicaTest-{}-ui-shots", std::process::id());
        let live = Arc::new(LiveBackend::with(
            paths,
            send,
            Arc::default(),
            CredentialManager::with_prefix(&format!("{tag}/")),
            Scheduler::in_folder(&format!(r"\{tag}")),
        ));
        let backend = Arc::new(data(Faked::new(live, Vec::new())));
        let mut settings = Settings {
            jobs: vec![
                job(
                    "Documents",
                    "daily 02:30",
                    Mode::Versioned,
                    r"C:\Users\Alex\Documents",
                    "BACKUP",
                ),
                job("Photos", "every 6h", Mode::Mirror, r"D:\Photos", "PHOTOS"),
            ],
            ..Settings::default()
        };
        settings.app.schedule_mode = mode;
        settings.save(&backend.settings_file()).unwrap();
        Fixture {
            _temp: temp,
            backend,
            _requests: requests,
        }
    }

    fn shell(&self, tab: Tab) -> Shell {
        let backend = Arc::clone(&self.backend) as Arc<dyn Backend>;
        let mut shell = Shell::new(
            backend,
            Arc::new(Shared::default()),
            WindowState::default(),
            None,
        );
        shell.tab = tab;
        shell
    }
}

/// Takes Restore to `step`: 1, the snapshots listed; 2, one chosen, folders opened and files
/// chosen; 3, "Where they were" and Restore pressed, so its confirmation is up.
fn restore_steps(harness: &mut Harness<'_, Shell>, step: u32) {
    use egui_kittest::kittest::Queryable;
    const ROOT: &str = r"C:\Users\Alex\Documents";
    // Until `label` is there (snapshots and folders are read on threads), for at most 2 s.
    let settle = |h: &mut Harness<'_, Shell>, label: &str| {
        for _ in 0..400 {
            if h.query_all_by_label_contains(label).next().is_some() {
                // Then until the layout settles, so a click lands where the label is drawn.
                let _ = h.try_run();
                return;
            }
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("Restore didn't show {label}");
    };
    settle(harness, "4f2a91c0");
    if step < 2 {
        return;
    }
    harness.get_by_label_contains("4f2a91c0").click();
    settle(harness, ROOT);
    harness.get_by_label(ROOT).click();
    settle(harness, "Projects");
    harness.get_by_label("Projects").click();
    settle(harness, "plan.md");
    harness.get_by_label_contains("report.docx").click();
    harness.step();
    harness.get_by_label_contains("plan.md").click();
    harness.step();
    harness.get_by_label("All of Taxes").click();
    let _ = harness.try_run();
    if step < 3 {
        return;
    }
    harness
        .get_by_label("Where they were, replacing what's there now")
        .click();
    let _ = harness.try_run();
    harness.get_by_label("Restore 3 items").click();
    let _ = harness.try_run();
}

impl Fixture {
    /// A temp app folder with `jobs`, and the real backend on it reporting `status` as the
    /// tray's.
    fn with_jobs(jobs: Vec<Job>, status: Status) -> Fixture {
        Fixture::with_jobs_and_data(jobs, status, |faked| faked)
    }

    /// [`Fixture::with_jobs`], with the fake's history and snapshots as `data` makes them.
    fn with_jobs_and_data(
        jobs: Vec<Job>,
        status: Status,
        data: impl FnOnce(Faked) -> Faked,
    ) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::resolve(
            "Replica",
            &temp.path().join("exe"),
            Some(&temp.path().join("appdata")),
        )
        .unwrap();
        let (send, requests) = channel();
        let tag = format!("ReplicaTest-{}-ui-shots", std::process::id());
        let live = Arc::new(LiveBackend::with(
            paths,
            send,
            Arc::new(Mutex::new(status)),
            CredentialManager::with_prefix(&format!("{tag}/")),
            Scheduler::in_folder(&format!(r"\{tag}")),
        ));
        let backend = Arc::new(data(Faked::new(live, Vec::new())));
        Settings {
            jobs,
            ..Settings::default()
        }
        .save(&backend.settings_file())
        .unwrap();
        Fixture {
            _temp: temp,
            backend,
            _requests: requests,
        }
    }

    /// A drive target in this fixture's temp folder, on the volume it's on: it checks as
    /// connected and not set up, so saving a job with it shows the password (which is stored
    /// only on "I've saved a copy", never pressed here).
    fn temp_drive(&self) -> Target {
        let folder = self._temp.path().join("backup");
        std::fs::create_dir_all(&folder).unwrap();
        let volume = self
            .backend
            .volumes()
            .unwrap()
            .into_iter()
            .find(|v| v.mount_points.iter().any(|m| folder.starts_with(m)))
            .expect("the temp folder's volume");
        let root = volume
            .mount_points
            .iter()
            .find(|m| folder.starts_with(m))
            .unwrap()
            .clone();
        Target::Drive {
            name: None,
            volume: VolumeRef {
                volume_id: Some(volume.id.clone()),
                label: volume.label.clone(),
            },
            path: folder.strip_prefix(&root).unwrap().to_path_buf(),
        }
    }
}

/// Jobs that ended every way, one running and one waiting its turn.
fn busy_fixture() -> Fixture {
    busy_fixture_with(|_| {})
}

/// [`busy_fixture`], its status changed by `edit` first.
fn busy_fixture_with(edit: impl FnOnce(&mut Status)) -> Fixture {
    let now = chrono::Utc::now();
    let mut jobs = vec![
        job(
            "Documents",
            "daily 02:30",
            Mode::Versioned,
            r"C:\Users\Alex\Documents",
            "BACKUP",
        ),
        job("Photos", "every 6h", Mode::Mirror, r"D:\Photos", "PHOTOS"),
        job(
            "Music",
            "weekly sat 10:00",
            Mode::Versioned,
            r"D:\Music",
            "BACKUP",
        ),
        job(
            "Projects",
            "every 2h",
            Mode::Versioned,
            r"C:\Work",
            "BACKUP",
        ),
        job(
            "Old-laptop",
            "daily 21:00",
            Mode::Mirror,
            r"E:\Laptop",
            "ARCHIVE",
        ),
    ];
    jobs[3].targets.push(Target::Unc {
        name: Some("nas".into()),
        path: r"\\nas\backup\projects".into(),
        credential: None,
    });
    jobs[4].enabled = false;
    jobs.push(Job {
        schedule: None,
        ..job(
            "Scratch",
            "every 1h",
            Mode::Versioned,
            r"C:\Scratch",
            "BACKUP",
        )
    });
    let last = |name: &str, hours: i64, outcome, reason: Option<&str>| {
        (
            name.to_lowercase(),
            LastRun {
                at: now - chrono::TimeDelta::hours(hours),
                outcome,
                reason: reason.map(str::to_owned),
            },
        )
    };
    let status = Status {
        running: Some(("Photos".into(), 42)),
        waiting: None,
        queued: vec!["Scratch".into()],
        last: [
            last("Documents", 9, Outcome::Ok, None),
            last("Photos", 26, Outcome::Ok, None),
            last(
                "Music",
                50,
                Outcome::Failed,
                Some("BACKUP:Replica: restic couldn't open the repository: wrong password."),
            ),
            last(
                "Projects",
                1,
                Outcome::Skipped,
                Some("nas: the share isn't reachable"),
            ),
        ]
        .into(),
        since: Some(now - chrono::TimeDelta::hours(30)),
        overdue: Default::default(),
    };
    let mut status = status;
    edit(&mut status);
    Fixture::with_jobs(jobs, status)
}

/// Long values, to see each is cut tidily wherever it shows: a job's name near the
/// most core allows, a drive's label, a deep share and a snapshot's tag.
const LONG_JOB: &str = "documents-and-every-project-folder-from-the-old-laptop-2026";
const LONG_JOB_2: &str = "photos-from-every-phone-since-2012-including-the-old-iphone";
const LONG_LABEL: &str = "WD-MyPassport-4TB-Backup";
const LONG_UNC: &str =
    r"\\nas-backup-server.home.lan\backups\replica\documents-and-every-project-folder\2026";
const LONG_TAG: &str = "before-moving-everything-to-the-new-laptop-october-2026";

/// [`LONG_JOB`] (to [`LONG_LABEL`] and [`LONG_UNC`]), [`LONG_JOB_2`] and a short one beside
/// them, with their last runs.
fn long_fixture(data: impl FnOnce(Faked) -> Faked) -> Fixture {
    let now = chrono::Utc::now();
    let mut long = job(
        LONG_JOB,
        "daily 02:30",
        Mode::Versioned,
        r"C:\Users\Alex\Documents\Projects\Clients\2026\Northwind\Specifications",
        LONG_LABEL,
    );
    long.targets.push(Target::Unc {
        name: None,
        path: LONG_UNC.into(),
        credential: None,
    });
    let jobs = vec![
        long,
        job(
            LONG_JOB_2,
            "every 6h",
            Mode::Mirror,
            r"D:\Photos",
            LONG_LABEL,
        ),
        job(
            "Music",
            "weekly sat 10:00",
            Mode::Versioned,
            r"D:\Music",
            "BACKUP",
        ),
    ];
    let last = |name: &str, hours: i64, outcome, reason: Option<String>| {
        (
            name.to_lowercase(),
            LastRun {
                at: now - chrono::TimeDelta::hours(hours),
                outcome,
                reason,
            },
        )
    };
    let status = Status {
        last: [
            last(LONG_JOB, 9, Outcome::Ok, None),
            last(
                LONG_JOB_2,
                26,
                Outcome::Skipped,
                Some(format!("{LONG_LABEL}:Replica: the drive isn't connected")),
            ),
        ]
        .into(),
        since: Some(now - chrono::TimeDelta::hours(30)),
        ..Status::default()
    };
    Fixture::with_jobs_and_data(jobs, status, data)
}

/// Screens of long values at the window's first-run size (or narrow): History with long names and a
/// run chosen, its job filter open, empty, and with filters that match nothing; the Jobs table,
/// the editor and the delete confirmation with long names; Restore browsing a long-named job.
fn long_name_screens<'a>() -> Vec<(&'static str, Shoot<'a>)> {
    use egui_kittest::kittest::{NodeT, Queryable};
    let history = |set: fn(&mut Shell), choose: bool| -> Shoot<'a> {
        Box::new(move |v: Variant, path: &Path| {
            let fixture = long_fixture(Faked::with_long_history);
            let mut shell = fixture.shell(Tab::History);
            set(&mut shell);
            let mut harness = v
                .default_size()
                .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
            let _ = harness.try_run();
            if choose {
                harness
                    .get_all_by_role(eframe::egui::accesskit::Role::ListBoxOption)
                    .find(|n| {
                        let name = n.accesskit_node().label().unwrap_or_default();
                        name.contains(LONG_JOB) && name.contains("Partly")
                    })
                    .expect("the long job's partly-done run")
                    .click();
            }
            harness.hover_at(eframe::egui::pos2(4.0, 4.0));
            save(&mut harness, v.scale, path);
        })
    };
    vec![
        ("long-history", history(|_| {}, true)),
        (
            "long-history-filtered",
            history(|shell| shell.history.job = Some(LONG_JOB.into()), true),
        ),
        (
            "long-history-filter-open",
            Box::new(|v: Variant, path: &Path| {
                let fixture = long_fixture(Faked::with_long_history);
                let mut harness = v.default_size().build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::History),
                );
                let _ = harness.try_run();
                harness.get_by_label("Job").click();
                let _ = harness.try_run();
                save(&mut harness, v.scale, path);
            }),
        ),
        (
            "history-empty-default",
            Box::new(|v: Variant, path: &Path| {
                let fixture = Fixture::new();
                let mut harness = v.default_size().build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::History),
                );
                save(&mut harness, v.scale, path);
            }),
        ),
        (
            "history-no-match",
            history(|shell| shell.history.result = Some(Outcome::Failed), false),
        ),
        (
            "long-jobs",
            Box::new(|v: Variant, path: &Path| {
                let fixture = long_fixture(|f| f);
                let mut harness = v.default_size().build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::Jobs),
                );
                save(&mut harness, v.scale, path);
            }),
        ),
        (
            "long-editor",
            Box::new(|v: Variant, path: &Path| {
                let fixture = long_fixture(|f| f);
                let mut shell = fixture.shell(Tab::Jobs);
                let job = shell.jobs.jobs()[0].clone();
                shell.jobs.edit(Some(job));
                let mut harness = v
                    .default_size()
                    .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
                save(&mut harness, v.scale, path);
            }),
        ),
        (
            "long-delete",
            Box::new(|v: Variant, path: &Path| {
                let fixture = long_fixture(|f| f);
                let mut harness = v.default_size().build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::Jobs),
                );
                let _ = harness.try_run();
                harness
                    .get_by_label(&format!("More actions for {LONG_JOB}"))
                    .click();
                let _ = harness.try_run();
                harness.get_by_label(&format!("Delete {LONG_JOB}")).click();
                let _ = harness.try_run();
                harness.hover_at(eframe::egui::pos2(4.0, 4.0));
                save(&mut harness, v.scale, path);
            }),
        ),
        (
            "long-restore",
            Box::new(|v: Variant, path: &Path| {
                let fixture = long_fixture(Faked::with_long_snapshots);
                let mut shell = fixture.shell(Tab::Restore);
                shell.restore.job = Some(LONG_JOB.into());
                let mut harness = v
                    .default_size()
                    .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
                restore_steps(&mut harness, 2);
                harness.hover_at(eframe::egui::pos2(4.0, 4.0));
                save(&mut harness, v.scale, path);
            }),
        ),
    ]
}

/// A job of `mode` with every section filled in.
fn full_job(mode: Mode) -> Job {
    let mut job = match mode {
        Mode::Versioned => job(
            "Documents",
            "daily 02:30",
            Mode::Versioned,
            r"C:\Users\Alex\Documents",
            "BACKUP",
        ),
        Mode::Mirror => job("Photos", "every 6h", Mode::Mirror, r"D:\Photos", "PHOTOS"),
    };
    job.sources[0].exclude = vec!["node_modules".into(), "*.tmp".into(), "~$*".into()];
    job.sources.push(Source {
        path: r"C:\Users\Alex\Desktop".into(),
        exclude: vec![],
    });
    job.targets.push(Target::Unc {
        name: Some("nas".into()),
        path: r"\\nas\backup\documents".into(),
        credential: Some(r"share:\\nas\backup".into()),
    });
    if mode == Mode::Versioned {
        job.before.push(BeforeStep::SqliteSnapshot {
            db: r"C:\Users\Alex\Documents\notes.db".into(),
        });
        job.after.push(AfterStep::Verify {
            schedule: Some("weekly sun 03:00".parse().unwrap()),
            read_data_subset: "5%".into(),
        });
    } else {
        job.before.push(BeforeStep::Command {
            cmd: r"C:\Tools\export-catalogue.cmd".into(),
            timeout_secs: 600,
        });
    }
    job
}

/// The Jobs screens: the Jobs page busy and empty, the editor for a versioned and a mirror job
/// with a message from core showing, and the password step; and the mirror warning.
fn jobs_screens<'a>() -> Vec<(&'static str, Shoot<'a>)> {
    let page = |fixture: fn() -> Fixture| -> Shoot<'a> {
        Box::new(move |v: Variant, path: &Path| {
            let fixture = fixture();
            let mut harness = v.builder(760.0).build_ui_state(
                |ui, shell: &mut Shell| shell.show(ui),
                fixture.shell(Tab::Jobs),
            );
            save(&mut harness, v.scale, path);
        })
    };
    vec![
        ("jobs-busy", page(busy_fixture)),
        (
            "jobs-empty",
            page(|| Fixture::with_jobs(vec![], Status::default())),
        ),
        (
            "editor-versioned",
            Box::new(|v, path| editor_shot(v, path, Mode::Versioned)),
        ),
        (
            "editor-mirror",
            Box::new(|v, path| editor_shot(v, path, Mode::Mirror)),
        ),
        ("editor-mirror-warning", Box::new(mirror_warning_shot)),
        // Keep: the newest backups (with the counts) or everything, forever.
        (
            "editor-keep-newest",
            Box::new(|v, path| keep_shot(v, path, false)),
        ),
        (
            "editor-keep-forever",
            Box::new(|v, path| keep_shot(v, path, true)),
        ),
        ("password", Box::new(password_shot)),
    ]
}

/// The editor on a full job of `mode`, saved once with a mistake in it so core's message
/// shows: a source that isn't a full path (versioned), a name that isn't allowed (mirror).
/// Taller than the window, to show every section.
fn editor_shot(v: Variant, path: &Path, mode: Mode) {
    use egui_kittest::kittest::Queryable;
    let job = full_job(mode);
    let fixture = Fixture::with_jobs(vec![job.clone()], Status::default());
    let mut shell = fixture.shell(Tab::Jobs);
    shell.jobs.edit(Some(job));
    {
        let form = &mut shell.jobs.editor_mut().unwrap().form;
        match mode {
            Mode::Versioned => form.sources[1].path = "Desktop".into(),
            Mode::Mirror => form.name = "my photos!".into(),
        }
    }
    let mut harness = v
        .builder(3000.0)
        .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
    let _ = harness.try_run();
    harness.get_by_label("Save").click();
    let _ = harness.try_run();
    harness.hover_at(eframe::egui::pos2(4.0, 2990.0));
    save(&mut harness, v.scale, path);
}

/// The editor on a full versioned job keeping the newest backups, or everything forever.
fn keep_shot(v: Variant, path: &Path, forever: bool) {
    let mut job = full_job(Mode::Versioned);
    job.retention.keep_forever = forever;
    let fixture = Fixture::with_jobs(vec![job.clone()], Status::default());
    let mut shell = fixture.shell(Tab::Jobs);
    shell.jobs.edit(Some(job));
    let mut harness = v
        .builder(3000.0)
        .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
    let _ = harness.try_run();
    save(&mut harness, v.scale, path);
}

/// The editor on a versioned job with Mirror just chosen: the warning before it's used.
fn mirror_warning_shot(v: Variant, path: &Path) {
    use egui_kittest::kittest::Queryable;
    let job = full_job(Mode::Versioned);
    let fixture = Fixture::with_jobs(vec![job.clone()], Status::default());
    let mut shell = fixture.shell(Tab::Jobs);
    shell.jobs.edit(Some(job));
    let mut harness = v
        .builder(760.0)
        .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
    let _ = harness.try_run();
    harness
        .get_by_label("Mirror: keeps an exact copy, and deletions are copied too")
        .click();
    let _ = harness.try_run();
    harness.get_by_label("A mirror copies deletions too");
    harness.hover_at(eframe::egui::pos2(4.0, 750.0));
    save(&mut harness, v.scale, path);
}

/// A new job's first save: its target's password, shown once.
fn password_shot(v: Variant, path: &Path) {
    use egui_kittest::kittest::Queryable;
    let fixture = Fixture::with_jobs(vec![], Status::default());
    let mut shell = fixture.shell(Tab::Jobs);
    shell.jobs.edit(None);
    {
        let form = &mut shell.jobs.editor_mut().unwrap().form;
        form.name = "Documents".into();
        form.add_source(PathBuf::from(r"C:\Users\Alex\Documents"));
        let job = Job {
            targets: vec![fixture.temp_drive()],
            ..Job::default()
        };
        form.targets = replica::ui::jobs::form::JobForm::from_job(&job).targets;
    }
    let mut harness = v
        .builder(760.0)
        .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
    let _ = harness.try_run();
    harness.get_by_label("Save").click();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while harness
        .query_by_label("Save a copy of this password")
        .is_none()
    {
        assert!(std::time::Instant::now() < until, "no password step");
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // The pointer off the page's buttons.
    harness.hover_at(eframe::egui::pos2(4.0, 750.0));
    save(&mut harness, v.scale, path);
}

/// `egui_kittest`'s margin round a screen, in points.
const MARGIN: f32 = 8.0;

/// Lets `harness` settle (threads finish, layouts that need a second pass), then writes it,
/// without the harness's margin.
fn save<S>(harness: &mut Harness<'_, S>, scale: f32, path: &Path) {
    for _ in 0..8 {
        if harness.try_run().is_ok() {
            break;
        }
    }
    let image = harness.render().expect("rendered with wgpu");
    let m = (MARGIN * scale).round() as u32;
    let (w, h) = image.dimensions();
    image::imageops::crop_imm(&image, m, m, w - 2 * m, h - 2 * m)
        .to_image()
        .save(path)
        .unwrap();
}

/// Shoots a screen in a variant to a file.
type Shoot<'a> = Box<dyn Fn(Variant, &Path) + 'a>;

/// The screens, by name.
fn screens(fixture: &Fixture) -> Vec<(&'static str, Shoot<'_>)> {
    let mut screens: Vec<(&'static str, Shoot<'_>)> = vec![(
        "gallery",
        Box::new(|v: Variant, path: &Path| {
            let mut harness = v.builder(2500.0).build_ui_state(
                |ui, g: &mut gallery::Gallery| g.ui(ui),
                gallery::Gallery::default(),
            );
            save(&mut harness, v.scale, path);
        }),
    )];
    for (name, tab) in [
        ("jobs", Tab::Jobs),
        ("history", Tab::History),
        ("restore", Tab::Restore),
        ("settings", Tab::Settings),
        ("about", Tab::About),
    ] {
        screens.push((
            name,
            Box::new(move |v: Variant, path: &Path| {
                let mut harness = v
                    .builder(760.0)
                    .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), fixture.shell(tab));
                save(&mut harness, v.scale, path);
            }),
        ));
    }
    // The whole Settings page with Task Scheduler's tasks (on, with two), none, and an
    // orphan; About in full.
    let docs = || task("Documents", "daily 02:30", true, Some(0));
    let photos = || task("Photos", "every 6h", false, Some(2));
    let orphan = || ScheduledTask {
        orphan: true,
        job: "Music".into(),
        ..task("Music", "weekly mon 03:00", true, Some(1))
    };
    for (name, mode, tasks) in [
        (
            "settings-tasks",
            ScheduleMode::TaskScheduler,
            vec![docs(), photos()],
        ),
        ("settings-none", ScheduleMode::Timer, vec![]),
        (
            "settings-orphan",
            ScheduleMode::TaskScheduler,
            vec![docs(), photos(), orphan()],
        ),
    ] {
        screens.push((
            name,
            Box::new(move |v: Variant, path: &Path| {
                let fixture = Fixture::with(mode, tasks.clone());
                let mut harness = v.builder(2100.0).build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::Settings),
                );
                save(&mut harness, v.scale, path);
            }),
        ));
    }
    // History with mixed results and a run chosen, and empty; each step of Restore, and
    // the confirmation before restoring over the originals.
    screens.push((
        "history-mixed",
        Box::new(|v: Variant, path: &Path| {
            use egui_kittest::kittest::{NodeT, Queryable};
            let fixture = Fixture::with_data(Faked::with_history);
            let mut harness = v.builder(1200.0).build_ui_state(
                |ui, shell: &mut Shell| shell.show(ui),
                fixture.shell(Tab::History),
            );
            let _ = harness.try_run();
            harness
                .get_all_by_role(eframe::egui::accesskit::Role::ListBoxOption)
                .find(|n| {
                    let name = n.accesskit_node().label().unwrap_or_default();
                    name.contains("Photos") && name.contains("Partly")
                })
                .expect("Photos' partly-done run")
                .click();
            save(&mut harness, v.scale, path);
        }),
    ));
    screens.push((
        "history-empty",
        Box::new(|v: Variant, path: &Path| {
            let fixture = Fixture::new();
            let mut harness = v.builder(760.0).build_ui_state(
                |ui, shell: &mut Shell| shell.show(ui),
                fixture.shell(Tab::History),
            );
            save(&mut harness, v.scale, path);
        }),
    ));
    for (name, step) in [
        ("restore-choose", 1),
        ("restore-browse", 2),
        ("restore-confirm", 3),
    ] {
        screens.push((
            name,
            Box::new(move |v: Variant, path: &Path| {
                let fixture = Fixture::with_data(Faked::with_snapshots);
                let mut shell = fixture.shell(Tab::Restore);
                shell.restore.job = Some("Documents".into());
                let mut harness = v
                    .builder(1500.0)
                    .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
                restore_steps(&mut harness, step);
                save(&mut harness, v.scale, path);
            }),
        ));
    }
    // A pinned snapshot in the table, an unpinned one chosen (Pin this backup), and the
    // pinned one chosen with Unpin's confirmation up.
    for (name, unpin) in [("restore-pinned", false), ("restore-unpin-confirm", true)] {
        screens.push((
            name,
            Box::new(move |v: Variant, path: &Path| {
                use egui_kittest::kittest::Queryable;
                let fixture = Fixture::with_data(|f| {
                    let f = f.with_snapshots();
                    f.snapshots.lock().unwrap()[1]
                        .tags
                        .push(replica_core::restic::PINNED_TAG.to_string());
                    f
                });
                let mut shell = fixture.shell(Tab::Restore);
                shell.restore.job = Some("Documents".into());
                let mut harness = v
                    .builder(1500.0)
                    .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
                restore_steps(&mut harness, 1);
                let chosen = if unpin { "9b31d7e2" } else { "4f2a91c0" };
                harness.get_by_label_contains(chosen).click();
                let _ = harness.try_run();
                if unpin {
                    harness.get_by_label("Unpin…").click();
                    let _ = harness.try_run();
                }
                save(&mut harness, v.scale, path);
            }),
        ));
    }
    screens.push((
        "about-full",
        Box::new(move |v: Variant, path: &Path| {
            use egui_kittest::kittest::Queryable;
            let mut harness = v.builder(1300.0).build_ui_state(
                |ui, shell: &mut Shell| shell.show(ui),
                fixture.shell(Tab::About),
            );
            // With the notices open.
            let _ = harness.try_run();
            harness.get_by_label("Third-party notices").click();
            save(&mut harness, v.scale, path);
        }),
    ));
    screens.extend(jobs_screens());
    // Each of Settings' tabs (Schedules with Task Scheduler on and two tasks), and Jobs,
    // at the window's first-run size.
    for (name, page) in [
        ("settings-tab-schedules", Page::Schedules),
        ("settings-tab-general", Page::General),
        ("settings-tab-restic", Page::Restic),
        ("settings-tab-storage", Page::Storage),
    ] {
        screens.push((
            name,
            Box::new(move |v: Variant, path: &Path| {
                use egui_kittest::kittest::Queryable;
                let fixture = Fixture::with(ScheduleMode::TaskScheduler, vec![docs(), photos()]);
                let mut shell = fixture.shell(Tab::Settings);
                shell.settings.page = page;
                let mut harness = v
                    .default_size()
                    .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
                // restic's version is found on a thread.
                let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while page == Page::Restic
                    && std::time::Instant::now() < end
                    && harness.query_by_label_contains("In use:").is_none()
                {
                    harness.step();
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                save(&mut harness, v.scale, path);
            }),
        ));
    }
    screens.push((
        "jobs-default-size",
        Box::new(|v: Variant, path: &Path| {
            let fixture = busy_fixture();
            let mut harness = v.default_size().build_ui_state(
                |ui, shell: &mut Shell| shell.show(ui),
                fixture.shell(Tab::Jobs),
            );
            save(&mut harness, v.scale, path);
        }),
    ));
    // The README's screenshots (`docs/screenshots/jobs-<theme>.png`, the normal 150% shots): the
    // first-run width, only as tall as the table needs.
    screens.push((
        "readme-jobs",
        Box::new(|v: Variant, path: &Path| {
            let fixture = busy_fixture();
            let width = replica::ui::geometry::DEFAULT_SIZE[0];
            let mut harness = v
                .sized(if v.narrow { v.width() } else { width }, 580.0)
                .build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::Jobs),
                );
            save(&mut harness, v.scale, path);
        }),
    ));
    // Music, failing for days, is overdue; Photos is running but waiting its turn for a
    // repository Documents is using.
    screens.push((
        "jobs-overdue",
        Box::new(|v: Variant, path: &Path| {
            let now = chrono::Utc::now();
            let fixture = busy_fixture_with(|status| {
                status.waiting = Some("Waiting for Documents to finish with BACKUP:Replica".into());
                status.overdue.insert(
                    "music".into(),
                    replica::ui::backend::Overdue {
                        due: now - chrono::TimeDelta::days(2),
                        last_success: Some(now - chrono::TimeDelta::days(9)),
                    },
                );
            });
            let mut harness = v.builder(760.0).build_ui_state(
                |ui, shell: &mut Shell| shell.show(ui),
                fixture.shell(Tab::Jobs),
            );
            save(&mut harness, v.scale, path);
        }),
    ));
    screens.extend(long_name_screens());
    screens.extend(transfer_screens());
    screens
}

/// The fixture's temp drive, at `folder` on it.
fn temp_drive_at(fixture: &Fixture, folder: &str) -> Target {
    match fixture.temp_drive() {
        Target::Drive {
            volume, mut path, ..
        } => {
            path.push(folder);
            Target::Drive {
                name: None,
                volume,
                path,
            }
        }
        other => other,
    }
}

/// Export… with a filter typed, and Import… with a job of every status.
fn transfer_screens<'a>() -> Vec<(&'static str, Shoot<'a>)> {
    const LONG: &str = "Projects-and-client-work-from-the-studio-archive-2019-to-2026";
    vec![
        (
            "export-filter",
            Box::new(|v: Variant, path: &Path| {
                use egui_kittest::kittest::Queryable;
                let jobs = vec![
                    job(
                        "Documents",
                        "daily 02:30",
                        Mode::Versioned,
                        r"C:\Users\Alex\Documents",
                        "BACKUP",
                    ),
                    job("Photos", "every 6h", Mode::Mirror, r"D:\Photos", "PHOTOS"),
                    job(
                        "Music",
                        "weekly sun 03:00",
                        Mode::Versioned,
                        r"D:\Music",
                        "BACKUP",
                    ),
                    job(
                        LONG,
                        "daily 23:00",
                        Mode::Versioned,
                        r"D:\Projects",
                        "NAS-COLD",
                    ),
                    job("Code", "every 2h", Mode::Versioned, r"D:\Code", "BACKUP"),
                ];
                let fixture = Fixture::with_jobs(jobs, Status::default());
                let mut harness = v.builder(760.0).build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::Jobs),
                );
                let _ = harness.try_run();
                harness.get_by_label("Export…").click_accesskit();
                let _ = harness.try_run();
                harness.get_by_label("Filter the jobs").focus();
                harness.get_by_label("Filter the jobs").type_text("o");
                let _ = harness.try_run();
                harness
                    .get_by_label("Code, Versioned, to BACKUP:Replica")
                    .click_accesskit();
                harness.hover_at(eframe::egui::pos2(4.0, 750.0));
                save(&mut harness, v.scale, path);
            }),
        ),
        (
            "import-statuses",
            Box::new(|v: Variant, path: &Path| {
                use egui_kittest::kittest::Queryable;
                let fixture = Fixture::with_jobs(vec![], Status::default());
                let here = Job {
                    targets: vec![temp_drive_at(&fixture, "documents")],
                    ..job(
                        "Documents",
                        "daily 02:30",
                        Mode::Versioned,
                        r"C:\Users\Alex\Documents",
                        "BACKUP",
                    )
                };
                Settings {
                    jobs: vec![here.clone()],
                    ..Settings::default()
                }
                .save(&fixture.backend.settings_file())
                .unwrap();
                // New (a mirror needs no password), Name exists and Needs a password, Drive
                // not found with a command, a long name, and two that are invalid.
                let music = Job {
                    targets: vec![temp_drive_at(&fixture, "music-mirror")],
                    ..job("Music", "weekly sun 03:00", Mode::Mirror, r"D:\Music", "X")
                };
                let mut offsite = job(
                    "Offsite",
                    "daily 23:00",
                    Mode::Versioned,
                    r"D:\Work",
                    "OFFSITE-HDD",
                );
                offsite.before.push(BeforeStep::Command {
                    cmd: r#""C:\Tools\stop-db.cmd" --quiet"#.into(),
                    timeout_secs: 120,
                });
                let long = Job {
                    targets: vec![temp_drive_at(&fixture, "projects")],
                    ..job(LONG, "daily 23:00", Mode::Versioned, r"D:\Projects", "X")
                };
                let overlap = Job {
                    targets: vec![temp_drive_at(&fixture, r"documents\copy")],
                    ..job("Docs-mirror", "every 6h", Mode::Mirror, r"D:\Docs", "X")
                };
                let text = format!(
                    "{}\n[[jobs]]\nname = \"my photos!\"\n[[jobs.sources]]\npath = 'D:\\Photos'\n\
                     [[jobs.targets]]\nkind = \"drive\"\nlabel = \"PHOTOS\"\npath = \"x\"\n",
                    replica_core::jobs_file::to_toml(&[music, here, offsite, long, overlap])
                );
                let file = fixture
                    .backend
                    .settings_file()
                    .with_file_name("from-laptop.replica-jobs.toml");
                std::fs::write(&file, text).unwrap();
                *fixture.backend.jobs_file.lock().unwrap() = Some(file);
                let mut harness = v.builder(900.0).build_ui_state(
                    |ui, shell: &mut Shell| shell.show(ui),
                    fixture.shell(Tab::Jobs),
                );
                let _ = harness.try_run();
                harness.get_by_label("Import…").click_accesskit();
                let _ = harness.try_run();
                harness
                    .get_by_label("What to do with Documents, whose name is taken")
                    .click_accesskit();
                let _ = harness.try_run();
                harness.get_by_label("Import as a copy").click_accesskit();
                let _ = harness.try_run();
                harness.hover_at(eframe::egui::pos2(4.0, 890.0));
                save(&mut harness, v.scale, path);
            }),
        ),
    ]
}

#[test]
#[ignore = "writes PNGs for review: run with --ignored (see the module's docs)"]
fn ui_shots() {
    let out = out_dir();
    std::fs::create_dir_all(&out).unwrap();
    let only: Option<Vec<String>> = std::env::var("REPLICA_UI_SCREENS")
        .ok()
        .map(|s| s.split(',').map(|n| n.trim().to_owned()).collect());
    let fixture = Fixture::new();
    let mut written = 0;
    for (name, shoot) in screens(&fixture) {
        if only.as_ref().is_some_and(|o| !o.iter().any(|n| n == name)) {
            continue;
        }
        for v in Variant::all() {
            shoot(v, &out.join(v.file(name)));
            written += 1;
        }
    }
    assert!(written > 0, "no screen matched REPLICA_UI_SCREENS");
    println!("{written} screenshots in {}", out.display());
}

// --- Through the accessibility tree (what Narrator reads) ----------------------------------

fn shell_harness(fixture: &Fixture) -> Harness<'_, Shell> {
    shell_harness_at(fixture, 1200.0)
}

fn shell_harness_at(fixture: &Fixture, width: f32) -> Harness<'_, Shell> {
    Harness::builder()
        .with_size([width, 760.0])
        .with_max_steps(64)
        .with_context_setup(|ctx| {
            replica::ui::fonts::install(ctx).unwrap();
            replica::ui::style(ctx);
        })
        .build_ui_state(
            |ui, shell: &mut Shell| shell.show(ui),
            fixture.shell(Tab::Jobs),
        )
}

#[test]
fn the_sidebar_is_read_as_tabs_and_switches_pages() {
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};

    let fixture = Fixture::new();
    let mut harness = shell_harness(&fixture);
    let _ = harness.try_run();
    harness.get_by_role_and_label(Role::TabList, "Pages");
    let tabs: Vec<String> = harness
        .get_all_by_role(Role::Tab)
        .map(|t| t.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(tabs, ["Jobs", "History", "Restore", "Settings", "About"]);
    let selected = |h: &Harness<'_, Shell>, name: &str| {
        h.get_by_role_and_label(Role::Tab, name)
            .accesskit_node()
            .is_selected()
    };
    assert_eq!(selected(&harness, "Jobs"), Some(true));
    assert_eq!(selected(&harness, "About"), Some(false));

    harness.get_by_role_and_label(Role::Tab, "About").click();
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::About);
    assert_eq!(selected(&harness, "About"), Some(true));
    // The page's header and the page itself.
    harness.get_by_label("The version, the licence and the third-party notices");
    harness.get_by_label_contains(env!("CARGO_PKG_VERSION"));
    harness.check_accessibility();
}

#[test]
fn the_gallerys_components_are_named_and_the_table_answers_the_keyboard() {
    use eframe::egui::Key;
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};

    let mut harness = Harness::builder()
        .with_size([1200.0, 2500.0])
        .with_context_setup(|ctx| {
            replica::ui::fonts::install(ctx).unwrap();
            replica::ui::style(ctx);
        })
        .build_ui_state(
            |ui, g: &mut gallery::Gallery| g.ui(ui),
            gallery::Gallery::default(),
        );
    let _ = harness.try_run();
    harness.check_accessibility();
    // A card's title is a heading, so a screen reader can jump between cards; a setting row's
    // control is named by its label, which isn't read again beside it.
    harness.get_by_role_and_label(Role::Heading, "Setting rows");
    harness.get_by_role_and_label(Role::CheckBox, "Pause schedules");
    assert_eq!(harness.query_all_by_label("Pause schedules").count(), 1);
    // Named by the whole path, though the field shows it cut.
    let path = r"\\nas\backups\Replica\Documents\2026\October\Projects\Northwind\design\notes\final-final.docx";
    harness.get_by_label(path);
    assert_eq!(harness.get_all_by_label("Copy the full path").count(), 2);
    // An error bar is an alert, its text read from what's in it (once: the bar isn't named
    // with it too).
    harness.get_by_role(Role::Alert);
    harness.get_by_label("restic couldn't open the repository: wrong password.");

    let selected = |h: &Harness<'_, gallery::Gallery>| -> Vec<String> {
        h.get_all_by_role(Role::ListBoxOption)
            .filter(|n| n.accesskit_node().is_selected() == Some(true))
            .map(|n| n.accesskit_node().label().unwrap_or_default())
            .collect()
    };
    assert_eq!(selected(&harness), ["Photos"]);
    // Rows with room round their cells.
    for row in harness.get_all_by_role(Role::ListBoxOption) {
        let height = row.rect().height();
        assert!(
            height >= 40.0,
            "{:?} is {height} high",
            row.accesskit_node().label()
        );
    }
    harness.get_by_role_and_label(Role::ListBox, "Runs").focus();
    let _ = harness.try_run();
    harness.key_press(Key::ArrowDown);
    let _ = harness.try_run();
    assert_eq!(selected(&harness), ["Music"]);
    harness.key_press(Key::End);
    let _ = harness.try_run();
    assert_eq!(selected(&harness), ["Old laptop"]);
    harness.key_press(Key::Home);
    let _ = harness.try_run();
    assert_eq!(selected(&harness), ["Documents"]);
    // A click selects too.
    harness
        .get_by_role_and_label(Role::ListBoxOption, "Projects")
        .click();
    let _ = harness.try_run();
    assert_eq!(selected(&harness), ["Projects"]);
}

#[test]
fn settings_tabs_are_read_as_a_tab_list_and_answer_the_arrow_keys() {
    use eframe::egui::Key;
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};

    let fixture = Fixture::new();
    let mut harness = shell_harness(&fixture);
    harness.state_mut().tab = Tab::Settings;
    let _ = harness.try_run();
    harness.get_by_role_and_label(Role::TabList, "Settings sections");
    let selected = |h: &Harness<'_, Shell>| -> Vec<String> {
        h.get_all_by_role(Role::Tab)
            .filter(|n| n.accesskit_node().is_selected() == Some(true))
            .map(|n| n.accesskit_node().label().unwrap_or_default())
            .collect()
    };
    // The sidebar's Settings, and Settings' own first tab.
    assert_eq!(selected(&harness), ["Settings", "Schedules"]);
    harness.get_by_label("Scheduled tasks");

    harness.get_by_role_and_label(Role::Tab, "Storage").click();
    let _ = harness.try_run();
    assert_eq!(harness.state().settings.page, Page::Storage);
    harness.get_by_label("Where Replica keeps its files");
    assert!(harness.query_by_label("Scheduled tasks").is_none());

    // From the focused tab, the arrows move along (wrapping round), Home and End to the ends.
    harness.get_by_role_and_label(Role::Tab, "Storage").focus();
    let _ = harness.try_run();
    for (key, page) in [
        (Key::ArrowLeft, Page::Restic),
        (Key::ArrowRight, Page::Storage),
        (Key::ArrowRight, Page::Schedules),
        (Key::End, Page::Storage),
        (Key::Home, Page::Schedules),
    ] {
        harness.key_press(key);
        let _ = harness.try_run();
        assert_eq!(harness.state().settings.page, page, "after {key:?}");
    }
    assert_eq!(selected(&harness), ["Settings", "Schedules"]);
    // Still on the tabs: the arrows didn't take the focus off them.
    assert!(
        harness
            .get_by_role_and_label(Role::Tab, "Schedules")
            .accesskit_node()
            .is_focused()
    );

    // Kept while the app runs: another page, then back.
    harness.get_by_role_and_label(Role::Tab, "General").click();
    let _ = harness.try_run();
    harness.get_by_role_and_label(Role::Tab, "Jobs").click();
    let _ = harness.try_run();
    harness.get_by_role_and_label(Role::Tab, "Settings").click();
    let _ = harness.try_run();
    assert_eq!(harness.state().settings.page, Page::General);
    harness.check_accessibility();
}

#[test]
fn a_narrow_window_has_a_rail_with_the_pages_names() {
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};

    let fixture = Fixture::new();
    let mut harness = shell_harness_at(&fixture, 800.0);
    let _ = harness.try_run();
    let tabs: Vec<(String, f32)> = harness
        .get_all_by_role(Role::Tab)
        .map(|t| {
            (
                t.accesskit_node().label().unwrap_or_default(),
                t.rect().width(),
            )
        })
        .collect();
    let names: Vec<&str> = tabs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Jobs", "History", "Restore", "Settings", "About"]);
    // Only the icons: each item is a rail's width, not the sidebar's.
    assert!(tabs.iter().all(|(_, w)| *w <= 48.0), "{tabs:?}");
    harness.get_by_role_and_label(Role::Tab, "Settings").click();
    let _ = harness.try_run();
    assert_eq!(harness.state().tab, Tab::Settings);
    harness.check_accessibility();
}

#[test]
fn go_to_restics_section_scrolls_the_notices_there() {
    use egui_kittest::kittest::Queryable;

    let fixture = Fixture::new();
    let mut harness = shell_harness(&fixture);
    harness.state_mut().tab = Tab::About;
    let _ = harness.try_run();
    harness.get_by_label("Third-party notices").click();
    let _ = harness.try_run();
    let buttons = harness.get_by_label("Copy").rect().bottom();
    let restic = |h: &Harness<'_, Shell>| h.get_by_label_contains("PART 2: RESTIC").rect().top();
    assert!(
        restic(&harness) > buttons + 1000.0,
        "restic's part starts far down"
    );
    harness.get_by_label("Go to restic's section").click();
    let _ = harness.try_run();
    let top = restic(&harness);
    assert!(
        top > buttons && top < buttons + 100.0,
        "restic's part is at the viewer's top: {top} after the buttons at {buttons}"
    );
}

#[test]
fn a_table_row_in_the_window_can_be_clicked() {
    use egui_kittest::kittest::Queryable;

    // Restore's snapshots, in the whole window. A row's node is on its first cell: the
    // middle of the row can be on a column's resize handle, where a click doesn't reach it.
    let fixture = Fixture::with_data(Faked::with_snapshots);
    let mut shell = fixture.shell(Tab::Restore);
    shell.restore.job = Some("Documents".into());
    let mut harness = Harness::builder()
        .with_size([1200.0, 1500.0])
        .with_max_steps(64)
        .with_context_setup(|ctx| {
            replica::ui::fonts::install(ctx).unwrap();
            replica::ui::style(ctx);
        })
        .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while harness.query_by_label_contains("4f2a91c0").is_none() {
        assert!(
            std::time::Instant::now() < until,
            "the snapshots are listed"
        );
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = harness.try_run();
    harness.get_by_label_contains("4f2a91c0").click();
    let _ = harness.try_run();
    assert_eq!(
        harness
            .state()
            .restore
            .snapshot
            .as_ref()
            .map(|s| s.short_id.as_str()),
        Some("4f2a91c0")
    );
}

// --- Restore's choices, as they change -----------------------------------------------------

/// A versioned job backing up to drives named `targets`.
fn job_to(name: &str, targets: &[&str]) -> Job {
    Job {
        name: name.into(),
        targets: targets
            .iter()
            .map(|t| Target::Drive {
                name: Some((*t).into()),
                volume: VolumeRef {
                    volume_id: None,
                    label: Some((*t).into()),
                },
                path: "Replica".into(),
            })
            .collect(),
        ..job("x", "daily 02:30", Mode::Versioned, r"C:\Work", "x")
    }
}

/// Restore on Documents (to drives A and B) and Photos (to C), both versioned.
fn restore_two_jobs(data: impl FnOnce(Faked) -> Faked) -> (Fixture, Harness<'static, Shell>) {
    let fixture = Fixture::with_data(|f| data(f.with_snapshots()));
    let file = fixture.backend.settings_file();
    let mut settings = Settings::load(&file).unwrap().settings;
    settings.jobs = vec![job_to("Documents", &["A", "B"]), job_to("Photos", &["C"])];
    settings.save(&file).unwrap();
    let mut shell = fixture.shell(Tab::Restore);
    shell.restore.job = Some("Documents".into());
    let harness = Harness::builder()
        .with_size([1200.0, 1500.0])
        .with_max_steps(64)
        .with_context_setup(|ctx| {
            replica::ui::fonts::install(ctx).unwrap();
            replica::ui::style(ctx);
        })
        .build_ui_state(|ui, shell: &mut Shell| shell.show(ui), shell);
    (fixture, harness)
}

/// Steps until `done`, for at most 10 s (the snapshots come on a thread).
fn until(harness: &mut Harness<'_, Shell>, done: impl Fn(&Harness<'_, Shell>) -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(harness) {
        assert!(std::time::Instant::now() < end, "timed out");
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = harness.try_run();
}

/// The snapshots listed: each row's name, which has the tags the fake gives (job, target).
fn listed(harness: &Harness<'_, Shell>) -> Vec<String> {
    use eframe::egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    harness
        .query_all_by_role(Role::ListBoxOption)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect()
}

/// Picks `option` in the drop-down named `picker`.
fn pick(harness: &mut Harness<'_, Shell>, picker: &str, option: &str) {
    use egui_kittest::kittest::Queryable;
    harness.get_by_label(picker).click();
    let _ = harness.try_run();
    harness.get_by_label(option).click();
    let _ = harness.try_run();
}

#[test]
fn switching_the_job_lists_the_new_jobs_snapshots_only() {
    let (fixture, mut harness) = restore_two_jobs(|f| f);
    until(&mut harness, |h| {
        listed(h).iter().any(|l| l.contains("Documents, A"))
    });
    pick(&mut harness, "Job", "Photos");
    until(&mut harness, |h| {
        listed(h).iter().any(|l| l.contains("Photos, C"))
    });
    assert_eq!(harness.state().restore.job.as_deref(), Some("Photos"));
    assert!(
        listed(&harness).iter().all(|l| l.contains("Photos, C")),
        "{:?}",
        listed(&harness)
    );
    let requests = fixture.backend.snapshot_requests.lock().unwrap().clone();
    assert_eq!(
        requests.last(),
        Some(&("Photos".to_string(), "C".to_string())),
        "{requests:?}"
    );
    // Asked for once each: nothing for Documents after the switch.
    assert_eq!(requests.len(), 2, "{requests:?}");
}

/// Lets a [`Gate`]'s answer go when dropped, so a worker still waiting is never left behind
/// (a test that fails part-way included).
struct Release(Option<std::sync::mpsc::Sender<()>>);

impl Release {
    fn now(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        self.now();
    }
}

#[test]
fn an_answer_for_the_job_before_doesnt_land_on_the_new_one() {
    use std::sync::atomic::{AtomicBool, Ordering};

    // Documents' snapshots are held back until the test lets them go.
    let started = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let (fixture, mut harness) = restore_two_jobs(|f| {
        *f.gate.lock().unwrap() = Some(Gate {
            job: "Documents".into(),
            started: Arc::clone(&started),
            release: rx,
        });
        f
    });
    let mut release = Release(Some(tx));
    let backend = Arc::clone(&fixture.backend);
    // Documents' request is under way: its worker holds a clone of the backend.
    until(&mut harness, |_| started.load(Ordering::SeqCst));
    let with_worker = Arc::strong_count(&backend);

    // Photos is chosen meanwhile, and its own snapshots listed (its worker done, too).
    pick(&mut harness, "Job", "Photos");
    until(&mut harness, |h| {
        listed(h).iter().any(|l| l.contains("Photos, C"))
            && Arc::strong_count(&backend) == with_worker
    });

    // Now Documents' answer comes, and its worker finishes (dropping its clone).
    release.now();
    until(&mut harness, |_| {
        Arc::strong_count(&backend) == with_worker - 1
    });
    let _ = harness.try_run();
    assert!(
        listed(&harness).iter().all(|l| l.contains("Photos, C")),
        "{:?}",
        listed(&harness)
    );
    assert_eq!(
        *fixture.backend.snapshot_requests.lock().unwrap(),
        [
            ("Documents".to_string(), "A".to_string()),
            ("Photos".to_string(), "C".to_string())
        ]
    );
}

#[test]
fn switching_the_target_lists_its_snapshots() {
    let (fixture, mut harness) = restore_two_jobs(|f| f);
    until(&mut harness, |h| {
        listed(h).iter().any(|l| l.contains("Documents, A"))
    });
    pick(&mut harness, "Backup", "B");
    until(&mut harness, |h| {
        listed(h).iter().any(|l| l.contains("Documents, B"))
    });
    assert!(
        listed(&harness).iter().all(|l| l.contains("Documents, B")),
        "{:?}",
        listed(&harness)
    );
    let requests = fixture.backend.snapshot_requests.lock().unwrap().clone();
    assert_eq!(
        requests.last(),
        Some(&("Documents".to_string(), "B".to_string()))
    );
}

#[test]
fn the_last_test_restore_shown_is_the_chosen_targets() {
    use egui_kittest::kittest::Queryable;
    use replica_core::history::TestRestoreRecord;
    use replica_core::run::Outcome;

    // On Documents: A's test failed, then B's passed. Photos' C was never tested.
    let test = |id: i64, target: &str, hours: i64, outcome: Outcome| TestRestoreRecord {
        id,
        job: "Documents".into(),
        target: target.into(),
        snapshot_id: None,
        started_at: chrono::Utc::now() - chrono::TimeDelta::hours(hours),
        duration: std::time::Duration::from_secs(5),
        outcome,
        checked: 10,
        differing: if outcome == Outcome::Ok { 0 } else { 2 },
        detail: None,
    };
    let (_fixture, mut harness) = restore_two_jobs(|f| {
        *f.tests.lock().unwrap() = vec![
            test(1, "A", 5, Outcome::Failed),
            test(2, "B", 1, Outcome::Ok),
        ];
        f
    });
    until(&mut harness, |h| {
        listed(h).iter().any(|l| l.contains("Documents, A"))
    });
    let shows = |h: &Harness<'_, Shell>, text: &str| h.query_by_label(text).is_some();
    assert!(
        shows(&harness, "Failed") && !shows(&harness, "OK"),
        "A's own test"
    );

    pick(&mut harness, "Backup", "B");
    until(&mut harness, |h| shows(h, "OK"));
    assert!(!shows(&harness, "Failed"), "B's own test");

    pick(&mut harness, "Job", "Photos");
    until(&mut harness, |h| shows(h, "Never run"));
    assert!(!shows(&harness, "OK") && !shows(&harness, "Failed"));
}
