//! The tray app's loop: the icon, its menu, the built-in timer, Task Scheduler sync, Start with
//! Windows, one job at a time on a worker thread, and the window ([`crate::ui`]), which asks
//! this loop to run jobs and is told how they're going.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local, Utc};
use replica_core::cancel::CancelToken;
use replica_core::history::History;
use replica_core::i18n::tr;
use replica_core::job::Job;
use replica_core::platform::{Level, Notification, Notifier};
use replica_core::repo_lock;
use replica_core::run::{Outcome, RunEvent, RunReport};
use replica_core::settings::{ScheduleMode, Settings};
use replica_core::tr;
use replica_win::autostart::AutoStart;
use replica_win::notify::TrayNotifier;
use replica_win::paths::AppPaths;
use replica_win::repo_locks::NamedMutexLocks;
use replica_win::scheduler::Scheduler;
use replica_win::tray::{MenuItem, SingleInstance, Tray, TrayEvent};

use super::icons;
use super::runner::{self, APP};
use super::state::{
    IconState, OverdueNotices, Tooltip, due_since, icon_state, is_due, is_overdue, notification_for,
};
use crate::ui::backend::{LastRun, LiveBackend, Overdue, Status, UiRequest};
use crate::ui::{FollowUp, Window};

/// How often the loop wakes when nothing happens.
const PUMP: Duration = Duration::from_millis(500);
/// How often the timer and overdue checks run.
const TICK: Duration = Duration::from_secs(15);

mod menu {
    pub const RUN_ALL: u32 = 1;
    pub const STOP: u32 = 2;
    pub const OPEN: u32 = 3;
    pub const PAUSE: u32 = 4;
    pub const AUTOSTART: u32 = 5;
    pub const QUIT: u32 = 6;
    /// `RUN_JOB + i` runs the i-th job.
    pub const RUN_JOB: u32 = 1000;
}

/// What the worker thread reports.
#[derive(Debug)]
enum WorkerEvent {
    Started(String),
    Progress(u32),
    /// The running job is waiting for a repository another run is using (the wait's text), or
    /// no longer is (`None`).
    Waiting(Option<String>),
    Finished(Box<RunReport>),
    /// It was waiting its turn for a repository and gave way to a job queued behind it that
    /// could run now: nothing was done or recorded. It goes back to the front.
    GaveWay(String),
    /// Stopped before it started: nothing was done.
    Cancelled(String),
}

/// Runs one job: [`runner::run_one`], or a test's own.
type RunFn = dyn Fn(
        &Job,
        &Settings,
        &mut History,
        &CancelToken,
        runner::QueueTurn<'_>,
        &mut dyn FnMut(&RunEvent),
    ) -> RunReport
    + Send
    + Sync;

/// The thread that runs jobs, one at a time, with its own history connection.
///
/// Each job's cancel token is made when it's handed over ([`Worker::run`]), not when the
/// thread gets to it, so Stop and Quit reach a job that's been dispatched but hasn't started:
/// the thread then skips it ([`WorkerEvent::Cancelled`]) without doing anything.
struct Worker {
    send: Option<Sender<(Job, Settings, CancelToken)>>,
    events: Receiver<WorkerEvent>,
    /// The tokens of the jobs handed over and not finished, oldest first.
    dispatched: Arc<Mutex<VecDeque<CancelToken>>>,
    /// The jobs queued behind the one it runs ([`Worker::set_behind`]): one waiting its turn
    /// for a repository gives way when one of these could run now.
    behind: Arc<Mutex<Vec<Job>>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn start(paths: AppPaths) -> Worker {
        let history_paths = paths.clone();
        Worker::with(
            move || runner::open_history(&history_paths),
            Arc::new(move |job, settings, history, cancel, turn, on_event| {
                runner::run_one(&paths, settings, job, history, cancel, turn, on_event)
            }),
        )
    }

    fn with(
        open_history: impl FnOnce() -> Result<History, String> + Send + 'static,
        run: Arc<RunFn>,
    ) -> Worker {
        let (send, requests) = channel::<(Job, Settings, CancelToken)>();
        let (report, events) = channel();
        let dispatched: Arc<Mutex<VecDeque<CancelToken>>> = Arc::default();
        let done = Arc::clone(&dispatched);
        let behind: Arc<Mutex<Vec<Job>>> = Arc::default();
        let queued = Arc::clone(&behind);
        let thread = std::thread::spawn(move || {
            let repo_locks = NamedMutexLocks::new();
            // When each job that gave way first tried: its wait counts from then.
            let mut first_tried: HashMap<String, Instant> = HashMap::new();
            let give_way = || {
                queued
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .any(|j| repo_lock::is_free(j, &repo_locks))
            };
            let mut history = match open_history() {
                Ok(h) => h,
                Err(error) => {
                    tracing::error!(%error, "the worker couldn't open the history");
                    return;
                }
            };
            for (job, settings, token) in requests {
                if token.is_cancelled() {
                    done.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                    let _ = report.send(WorkerEvent::Cancelled(job.name.clone()));
                    continue;
                }
                let _ = report.send(WorkerEvent::Started(job.name.clone()));
                let since = *first_tried
                    .entry(job.name.clone())
                    .or_insert_with(Instant::now);
                let turn = runner::QueueTurn {
                    waited: since.elapsed(),
                    give_way: Some(&give_way),
                };
                let result = run(
                    &job,
                    &settings,
                    &mut history,
                    &token,
                    turn,
                    &mut |e: &RunEvent| match e {
                        RunEvent::Waiting(wait) => {
                            let _ = report.send(WorkerEvent::Waiting(Some(wait.text())));
                        }
                        RunEvent::Waited => {
                            let _ = report.send(WorkerEvent::Waiting(None));
                        }
                        _ => {
                            if let Some(p) = runner::percent(e) {
                                let _ = report.send(WorkerEvent::Progress(p));
                            }
                        }
                    },
                );
                done.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                if result.gave_way {
                    let _ = report.send(WorkerEvent::GaveWay(job.name.clone()));
                    continue;
                }
                first_tried.remove(&job.name);
                let _ = report.send(WorkerEvent::Finished(Box::new(result)));
            }
        });
        Worker {
            send: Some(send),
            events,
            dispatched,
            behind,
            thread: Some(thread),
        }
    }

    /// The jobs now queued behind the one running, for it to give way to.
    fn set_behind(&self, jobs: Vec<Job>) {
        *self.behind.lock().unwrap_or_else(|e| e.into_inner()) = jobs;
    }

    /// Hands `job` over, with its own cancel token, made and registered first.
    fn run(&self, job: Job, settings: Settings) {
        let token = CancelToken::new();
        self.dispatched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(token.clone());
        if let Some(send) = &self.send {
            let _ = send.send((job, settings, token));
        }
    }

    /// Stops the running job and any handed over but not started.
    fn stop_all(&self) {
        for token in self
            .dispatched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            token.cancel();
        }
    }

    /// Stops everything and waits (bounded) for the thread to finish.
    fn shut_down(&mut self) {
        self.stop_all();
        self.send = None;
        if let Some(thread) = self.thread.take() {
            let until = Instant::now() + Duration::from_secs(30);
            while !thread.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

// What the tray knows of a job between runs.
#[derive(Debug, Clone, Default)]
struct Known {
    last_run: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
    last_outcome: Option<Outcome>,
    /// Why the last run didn't succeed, for the window.
    last_reason: Option<String>,
}

struct App {
    paths: AppPaths,
    settings: Settings,
    stamp: Option<SystemTime>,
    history: History,
    tray: Tray,
    instance: SingleInstance,
    notifier: TrayNotifier,
    worker: Worker,
    since: DateTime<Utc>,
    known: HashMap<String, Known>,
    queue: VecDeque<String>,
    running: Option<(String, u32)>,
    /// To see which queued jobs' repositories are free, so they go first.
    repo_locks: NamedMutexLocks,
    /// What the running job is waiting for, if it's waiting its turn for a repository.
    waiting: Option<String>,
    overdue: OverdueNotices,
    /// The jobs overdue at the last tick, for the window (`overdue_now` counts them).
    overdue_jobs: HashMap<String, Overdue>,
    overdue_now: usize,
    scheduler: Scheduler,
    exe: PathBuf,
    autostart: AutoStart,
    shown: Option<(IconState, usize)>,
    frame: usize,
    last_tick: Option<Instant>,
    /// The window, once it has been opened.
    window: Option<Window>,
    ui_send: Sender<UiRequest>,
    ui_requests: Receiver<UiRequest>,
    /// What the window shows of the runs: written here, read by the window.
    status: Arc<Mutex<Status>>,
    /// The window's failure to start has been notified.
    window_failure_told: bool,
}

/// Runs the tray app until Quit, with its window open first when `show`. Returns the exit code.
/// When a tray already runs, that one is asked to bring its window forward (whatever `show`
/// is) and this one ends.
pub fn run_tray(show: bool) -> i32 {
    let instance = match SingleInstance::acquire(&runner::instance_name()) {
        Ok(replica_win::tray::Instance::First(i)) => i,
        // Another tray runs: it's been asked to show itself.
        Ok(replica_win::tray::Instance::Second) => return 0,
        Err(error) => {
            tracing::error!(%error, "couldn't check for another instance");
            return 1;
        }
    };
    let paths = match AppPaths::detect(APP) {
        Ok(p) => p,
        Err(error) => {
            tracing::error!(%error, "couldn't work out the app's folder");
            return replica_core::run::EXIT_USAGE;
        }
    };
    super::start_logging(&paths);
    let tray = match Tray::new(icons::IDLE, &tr("tray-tooltip-idle")) {
        Ok(t) => t,
        Err(error) => {
            tracing::error!(%error, "couldn't add the tray icon");
            return 1;
        }
    };
    let history = match runner::open_history(&paths) {
        Ok(h) => h,
        Err(error) => {
            tracing::error!(%error, "couldn't open the run history");
            let _ = tray.balloon(
                &tr("tray-settings-error-title"),
                &error,
                replica_win::tray::BalloonKind::Error,
            );
            return 1;
        }
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("replica-tray.exe"));
    let (ui_send, ui_requests) = channel();
    let mut app = App {
        worker: Worker::start(paths.clone()),
        paths,
        settings: Settings::default(),
        stamp: None,
        history,
        tray,
        instance,
        notifier: TrayNotifier::default(),
        since: Utc::now(),
        known: HashMap::new(),
        queue: VecDeque::new(),
        running: None,
        repo_locks: NamedMutexLocks::new(),
        waiting: None,
        overdue: OverdueNotices::default(),
        overdue_jobs: HashMap::new(),
        overdue_now: 0,
        scheduler: runner::scheduler(),
        exe,
        autostart: super::autostart(APP),
        shown: None,
        frame: 0,
        last_tick: None,
        window: None,
        ui_send,
        ui_requests,
        status: Arc::default(),
        window_failure_told: false,
    };
    app.check_folder();
    app.take_installer_choices();
    app.reload_settings(true);
    app.prune_run_logs();
    if show {
        app.open_window();
    }
    let code = app.run();
    app.worker.shut_down();
    code
}

impl App {
    fn run(&mut self) -> i32 {
        loop {
            for event in self.tray.pump(PUMP, Some(&self.instance)) {
                match event {
                    TrayEvent::Menu { x, y } => {
                        if self.menu(x, y) {
                            tracing::info!("quitting: Quit in the tray's menu");
                            return 0;
                        }
                    }
                    TrayEvent::Activate | TrayEvent::ShowRequested | TrayEvent::BalloonClicked => {
                        tracing::info!(?event, "opening the window");
                        self.open_window()
                    }
                    // An uninstall or upgrade (`replica-tray --quit`): as the menu's Quit.
                    TrayEvent::QuitRequested => {
                        tracing::info!("quitting: asked to by another process");
                        return 0;
                    }
                }
            }
            self.drain_worker();
            self.drain_window();
            self.follow_up_window();
            if self.last_tick.is_none_or(|t| t.elapsed() >= TICK) {
                self.last_tick = Some(Instant::now());
                self.reload_settings(false);
                self.tick(Utc::now());
            }
            self.start_next();
            self.notifier.show_pending(&self.tray);
            self.show_state();
            self.publish_status();
        }
    }

    /// Does what the window asked.
    fn drain_window(&mut self) {
        while let Ok(request) = self.ui_requests.try_recv() {
            match request {
                UiRequest::Reload => self.reload_settings(true),
                UiRequest::Run(names) => names.iter().for_each(|n| self.enqueue(n)),
                UiRequest::Stop => {
                    self.queue.clear();
                    self.worker.stop_all();
                }
            }
        }
        self.start_next();
    }

    /// Tells the window what's running and how each job last ended, when that changed.
    fn publish_status(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        if !self.window_failure_told
            && let Some(error) = window.failed()
        {
            self.window_failure_told = true;
            self.notify(Level::Error, tr("tray-window-error-title"), error);
            return;
        }
        let status = Status {
            running: self.running.clone(),
            waiting: self.waiting.clone(),
            queued: self.queue.iter().cloned().collect(),
            overdue: self.overdue_jobs.clone(),
            last: self
                .known
                .iter()
                .filter_map(|(name, k)| {
                    let (at, outcome) = (k.last_run?, k.last_outcome?);
                    let reason = k.last_reason.clone();
                    Some((
                        name.clone(),
                        LastRun {
                            at,
                            outcome,
                            reason,
                        },
                    ))
                })
                .collect(),
            since: Some(self.since),
        };
        let mut shared = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *shared != status {
            *shared = status;
            drop(shared);
            window.repaint();
        }
    }

    fn notify(&self, level: Level, title: String, body: String) {
        let _ = self.notifier.notify(&Notification {
            level,
            job: None,
            title,
            body,
        });
    }

    /// The first start after an install: the installer's "Start with Windows" choice becomes
    /// the setting. Only when there are no settings yet, so a choice made later in the app
    /// always wins.
    fn take_installer_choices(&self) {
        let path = self.paths.settings_file();
        if self.paths.is_portable() || !self.paths.is_writable() || path.exists() {
            return;
        }
        let Some(settings) =
            first_settings(replica_win::installer::choice(APP, "StartWithWindows"))
        else {
            return;
        };
        if let Err(error) = settings.save(&path) {
            tracing::warn!(%error, "couldn't save the installer's choices");
        }
    }

    /// Says so, once at start, if the app's folder can't be written to.
    fn check_folder(&self) {
        if self.paths.is_writable() {
            return;
        }
        let path = self.paths.root().display().to_string();
        let body = if self.paths.is_portable() {
            tr!("error-portable-not-writable", path = path)
        } else {
            tr!("error-app-folder-not-writable", path = path)
        };
        self.notify(Level::Error, tr("tray-folder-title"), body);
    }

    /// Loads the settings when they've changed (or `force`), then brings the tasks, Start with
    /// Windows and the job history up to date with them.
    fn reload_settings(&mut self, force: bool) {
        let path = self.paths.settings_file();
        let stamp = runner::settings_stamp(&path);
        if !force && stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        match runner::load_settings(&self.paths) {
            Ok(loaded) => {
                for warning in &loaded.warnings {
                    self.notify(
                        Level::Warning,
                        tr("tray-settings-warning-title"),
                        warning.to_string(),
                    );
                }
                self.settings = loaded.settings;
            }
            Err(error) => {
                // Keep running with the settings it had.
                self.notify(
                    Level::Error,
                    tr("tray-settings-error-title"),
                    error.to_string(),
                );
                return;
            }
        }
        self.load_history();
        self.sync_tasks();
        self.apply_autostart();
    }

    fn load_history(&mut self) {
        self.known.clear();
        for job in &self.settings.jobs {
            let last = self
                .history
                .recent(Some(&job.name), 1)
                .ok()
                .and_then(|r| r.into_iter().next());
            let known = Known {
                last_run: last.as_ref().map(|r| r.started_at),
                last_outcome: last.as_ref().map(|r| r.outcome),
                last_reason: last.as_ref().and_then(|r| {
                    LastRun::reason_of(
                        r.error.as_deref(),
                        r.targets
                            .iter()
                            .map(|t| (t.target.as_str(), t.reason.as_deref())),
                    )
                }),
                last_success: self.history.last_success(&job.name).ok().flatten(),
            };
            self.known.insert(job.name.to_lowercase(), known);
        }
    }

    /// Makes Replica's tasks follow the saved schedule mode ([`runner::reconcile_tasks`]), at
    /// start and on every settings change.
    fn sync_tasks(&mut self) {
        match runner::reconcile_tasks(&self.scheduler, &self.settings, &self.exe) {
            Ok(done) => tracing::info!(?done, "brought the scheduled tasks up to date"),
            Err(error) => self.notify(Level::Error, tr("tray-schedule-error-title"), error.0),
        }
    }

    fn apply_autostart(&self) {
        if let Err(error) = self
            .autostart
            .apply(self.settings.app.start_with_windows, &self.exe)
        {
            self.notify(
                Level::Error,
                tr("tray-autostart-error-title"),
                error.to_string(),
            );
        }
    }

    fn prune_run_logs(&self) {
        if !self.paths.is_writable() {
            return;
        }
        let days = u64::from(self.settings.app.log_retention_days.max(1));
        let retention = replica_win::logging::Retention {
            keep: usize::MAX,
            max_age: Some(Duration::from_secs(days * 24 * 60 * 60)),
        };
        let dir = self.paths.run_logs_dir();
        if dir.is_dir()
            && let Err(error) = replica_win::logging::prune(&dir, retention)
        {
            tracing::warn!(%error, "couldn't prune the run logs");
        }
    }

    /// The timer and the overdue check. The timer runs only with the option on the built-in
    /// timer and schedules not paused; with Task Scheduler on, its tasks run the jobs.
    fn tick(&mut self, now: DateTime<Utc>) {
        // Runs by Task Scheduler or the CLI write the same history: read it again, so failed
        // and overdue reflect them too. A job queued but not started yet is due again here,
        // and `enqueue` doesn't queue it twice.
        self.load_history();
        let local = |t: &DateTime<Utc>| t.with_timezone(&Local);
        let (now_l, since_l) = (local(&now), local(&self.since));
        let paused = self.settings.app.pause_schedules;
        let timer = self.settings.app.schedule_mode == ScheduleMode::Timer;
        let mut due = Vec::new();
        let mut overdue = 0;
        self.overdue_jobs.clear();
        for job in &self.settings.jobs {
            let key = job.name.to_lowercase();
            let known = self.known.get(&key).cloned().unwrap_or_default();
            let last_run = known.last_run.as_ref().map(local);
            if timer && !paused && is_due(job, last_run.as_ref(), &since_l, &now_l) {
                due.push(job.name.clone());
            }
            let last_success = known.last_success.as_ref().map(local);
            if is_overdue(job, last_success.as_ref(), &since_l, &now_l) {
                overdue += 1;
                if let Some(due) = due_since(job, last_success.as_ref(), &since_l) {
                    self.overdue_jobs.insert(
                        key.clone(),
                        Overdue {
                            due: due.with_timezone(&Utc),
                            last_success: known.last_success,
                        },
                    );
                }
                if !paused && self.overdue.take(&job.name, now_l.date_naive()) {
                    let due = due_since(job, last_success.as_ref(), &since_l)
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    let body = match last_success {
                        Some(s) => tr!(
                            "notify-overdue-body",
                            since = s.format("%Y-%m-%d %H:%M").to_string(),
                            reason = tr!("notify-overdue-reason", due = due.as_str())
                        ),
                        None => tr!("notify-overdue-never", due = due.as_str()),
                    };
                    self.notify(
                        Level::Warning,
                        tr!("notify-overdue-title", job = job.name.as_str()),
                        body,
                    );
                }
            }
        }
        self.overdue_now = overdue;
        for name in due {
            self.enqueue(&name);
            // Counted as run now, so the next tick doesn't queue it again.
            self.known.entry(name.to_lowercase()).or_default().last_run = Some(now);
        }
    }

    fn enqueue(&mut self, name: &str) {
        let busy = self
            .running
            .as_ref()
            .is_some_and(|(r, _)| r.eq_ignore_ascii_case(name))
            || self.queue.iter().any(|q| q.eq_ignore_ascii_case(name));
        if !busy {
            self.queue.push_back(name.to_owned());
        }
    }

    fn job_named(&self, name: &str) -> Option<&Job> {
        self.settings
            .jobs
            .iter()
            .find(|j| j.name.eq_ignore_ascii_case(name))
    }

    /// Starts the first queued job whose repositories are free, else the first, which waits its
    /// turn and gives way when one behind it could run. Tells the worker what's behind.
    fn start_next(&mut self) {
        if self.running.is_none() {
            self.queue.retain(|n| {
                self.settings
                    .jobs
                    .iter()
                    .any(|j| j.name.eq_ignore_ascii_case(n))
            });
            let jobs: Vec<&Job> = self
                .queue
                .iter()
                .filter_map(|n| self.job_named(n))
                .collect();
            if let Some(at) = repo_lock::pick_next(jobs, &self.repo_locks) {
                let name = self.queue.remove(at).expect("picked from the queue");
                let job = self.job_named(&name).expect("kept above").clone();
                self.running = Some((job.name.clone(), 0));
                self.worker.run(job, self.settings.clone());
            }
        }
        let behind = self
            .queue
            .iter()
            .filter_map(|n| self.job_named(n).cloned())
            .collect();
        self.worker.set_behind(behind);
    }

    fn drain_worker(&mut self) {
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                WorkerEvent::Started(name) => {
                    self.running = Some((name, 0));
                    self.waiting = None;
                }
                WorkerEvent::Progress(p) => {
                    if let Some(r) = &mut self.running {
                        r.1 = p;
                    }
                }
                WorkerEvent::Waiting(wait) => self.waiting = wait,
                WorkerEvent::GaveWay(name) => {
                    tracing::info!(job = %name, "gave way to a job behind it");
                    self.running = None;
                    self.waiting = None;
                    self.queue.push_front(name);
                }
                WorkerEvent::Finished(report) => {
                    self.running = None;
                    self.waiting = None;
                    self.finished(&report);
                }
                WorkerEvent::Cancelled(name) => {
                    tracing::info!(job = %name, "stopped before it started");
                    self.running = None;
                    self.waiting = None;
                }
            }
        }
    }

    fn finished(&mut self, report: &RunReport) {
        let key = report.job.to_lowercase();
        let known = self.known.entry(key).or_default();
        known.last_run = Some(report.started_at);
        known.last_outcome = Some(report.outcome);
        known.last_reason = LastRun::reason_of(
            report.error.as_deref(),
            report
                .targets
                .iter()
                .map(|t| (t.target.as_str(), t.reason.as_deref())),
        );
        if report.outcome == Outcome::Ok {
            known.last_success = Some(report.started_at);
            self.overdue.clear(&report.job);
        }
        let job = self
            .settings
            .jobs
            .iter()
            .find(|j| j.name.eq_ignore_ascii_case(&report.job));
        if let Some(n) = notification_for(report, job) {
            let _ = self.notifier.notify(&n);
        }
    }

    fn failed_job(&self) -> Option<&str> {
        self.settings
            .jobs
            .iter()
            .find(|j| {
                self.known
                    .get(&j.name.to_lowercase())
                    .is_some_and(|k| k.last_outcome == Some(Outcome::Failed))
            })
            .map(|j| j.name.as_str())
    }

    fn show_state(&mut self) {
        let failed = self.failed_job();
        let state = icon_state(
            self.running.is_some(),
            failed.is_some(),
            self.overdue_now > 0,
        );
        let tip = match (&self.running, failed) {
            (Some((job, percent)), _) => match &self.waiting {
                Some(wait) => Tooltip::Waiting {
                    job,
                    wait,
                    queued: self.queue.len(),
                },
                None => Tooltip::Running {
                    job,
                    percent: *percent,
                    queued: self.queue.len(),
                },
            },
            (None, Some(job)) => Tooltip::Failed { job },
            (None, None) if self.overdue_now > 0 => Tooltip::Overdue {
                count: self.overdue_now,
            },
            (None, None) if self.settings.app.pause_schedules => Tooltip::Paused,
            (None, None) => Tooltip::Idle,
        }
        .text();
        let _ = self.tray.set_tooltip(&tip);
        if state == IconState::Running {
            self.frame = self.frame.wrapping_add(1);
        }
        let shown = (
            state,
            if state == IconState::Running {
                self.frame % 2
            } else {
                0
            },
        );
        if self.shown != Some(shown) {
            if let Err(error) = self.tray.set_icon(icons::for_state(state, shown.1)) {
                tracing::warn!(%error, "couldn't change the tray icon");
            }
            self.shown = Some(shown);
        }
    }

    /// Checks that Open got the window up, and tells the user when it couldn't. A
    /// window that's gone is told by [`Self::publish_status`], through [`Window::failed`].
    fn follow_up_window(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let why = match window.follow_up(Instant::now()) {
            FollowUp::NotResponding => tr("window-not-responding"),
            FollowUp::NotStarted => tr("window-not-started"),
            _ => return,
        };
        self.notify(Level::Error, tr("tray-window-error-title"), why);
    }

    /// Shows the window, starting it the first time.
    fn open_window(&mut self) {
        match &self.window {
            // It can't be started again: a process gets one window event loop.
            Some(window) => match window.failed() {
                Some(error) => {
                    tracing::warn!(%error, "the window can't open");
                    self.notify(Level::Error, tr("tray-window-error-title"), error)
                }
                None => window.show(),
            },
            None => {
                let backend = LiveBackend::new(
                    self.paths.clone(),
                    self.ui_send.clone(),
                    Arc::clone(&self.status),
                );
                self.window = Some(Window::start(Arc::new(backend)));
                self.publish_status();
            }
        }
    }

    /// Shows the menu and does what was chosen. Returns true to quit.
    fn menu(&mut self, x: i32, y: i32) -> bool {
        let jobs = &self.settings.jobs;
        let run_items: Vec<MenuItem> = if jobs.is_empty() {
            vec![MenuItem::Command {
                id: 0,
                text: tr("tray-run-no-jobs"),
                checked: false,
                enabled: false,
            }]
        } else {
            jobs.iter()
                .enumerate()
                .map(|(i, j)| MenuItem::Command {
                    id: menu::RUN_JOB + i as u32,
                    text: j.name.clone(),
                    checked: false,
                    enabled: true,
                })
                .collect()
        };
        let mut items = vec![
            MenuItem::Command {
                id: menu::RUN_ALL,
                text: tr("tray-run-all"),
                checked: false,
                enabled: jobs.iter().any(|j| j.enabled),
            },
            MenuItem::Submenu {
                text: tr("tray-run-job"),
                items: run_items,
                enabled: true,
            },
        ];
        if let Some((job, _)) = &self.running {
            items.push(MenuItem::Command {
                id: menu::STOP,
                text: tr!("tray-stop", job = job.as_str()),
                checked: false,
                enabled: true,
            });
        }
        items.extend([
            MenuItem::Separator,
            MenuItem::Command {
                id: menu::OPEN,
                text: tr("tray-open"),
                checked: false,
                enabled: true,
            },
            MenuItem::Command {
                id: menu::PAUSE,
                text: tr("tray-pause-schedules"),
                checked: self.settings.app.pause_schedules,
                enabled: self.paths.is_writable(),
            },
            MenuItem::Command {
                id: menu::AUTOSTART,
                text: tr("tray-start-with-windows"),
                checked: self.settings.app.start_with_windows,
                enabled: self.paths.is_writable(),
            },
            MenuItem::Separator,
            MenuItem::Command {
                id: menu::QUIT,
                text: tr("tray-quit"),
                checked: false,
                enabled: true,
            },
        ]);
        let chosen = match self.tray.show_menu(&items, x, y) {
            Ok(Some(id)) => id,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(%error, "couldn't show the menu");
                return false;
            }
        };
        match chosen {
            menu::RUN_ALL => {
                let names: Vec<String> = self
                    .settings
                    .jobs
                    .iter()
                    .filter(|j| j.enabled)
                    .map(|j| j.name.clone())
                    .collect();
                names.iter().for_each(|n| self.enqueue(n));
            }
            menu::STOP => {
                self.queue.clear();
                self.worker.stop_all();
            }
            menu::OPEN => {
                tracing::info!("opening the window: Open in the tray's menu");
                self.open_window()
            }
            menu::PAUSE => self.change_settings(|s| s.app.pause_schedules = !s.app.pause_schedules),
            menu::AUTOSTART => {
                self.change_settings(|s| s.app.start_with_windows = !s.app.start_with_windows);
                self.apply_autostart();
            }
            menu::QUIT => return true,
            id if id >= menu::RUN_JOB => {
                if let Some(job) = self.settings.jobs.get((id - menu::RUN_JOB) as usize) {
                    let name = job.name.clone();
                    self.enqueue(&name);
                }
            }
            _ => {}
        }
        false
    }

    /// Changes a setting and saves the file.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        let path = self.paths.settings_file();
        match self.settings.save(&path) {
            Ok(()) => self.stamp = runner::settings_stamp(&path),
            Err(error) => self.notify(
                Level::Error,
                tr("tray-settings-error-title"),
                error.to_string(),
            ),
        }
    }
}

/// The first settings from the installer's "Start with Windows" choice (1 on, else off), if it
/// made one.
fn first_settings(start_with_windows: Option<u32>) -> Option<Settings> {
    let mut settings = Settings::default();
    settings.app.start_with_windows = start_with_windows? == 1;
    Some(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn the_installer_s_choice_makes_the_first_settings() {
        assert!(
            first_settings(None).is_none(),
            "no installer, nothing to take"
        );
        assert!(first_settings(Some(1)).unwrap().app.start_with_windows);
        assert!(!first_settings(Some(0)).unwrap().app.start_with_windows);
        let first = first_settings(Some(1)).unwrap();
        assert!(first.jobs.is_empty());
        assert_eq!(first.app.schedule_mode, ScheduleMode::Timer);
    }

    fn job(name: &str) -> Job {
        Job {
            name: name.into(),
            ..Job::default()
        }
    }

    fn report(job: &Job, outcome: Outcome) -> RunReport {
        RunReport {
            id: None,
            job: job.name.clone(),
            started_at: Utc::now(),
            duration: Duration::ZERO,
            outcome,
            error: None,
            targets: Vec::new(),
            log_path: None,
            waits: Vec::new(),
            gave_way: false,
        }
    }

    /// A run function that records which jobs it started, and, for `hold`, waits until the
    /// test lets go (or the run is cancelled).
    fn recording(
        hold: Option<(&'static str, mpsc::Receiver<()>)>,
    ) -> (Arc<RunFn>, Arc<Mutex<Vec<String>>>) {
        let started: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&started);
        let hold = Mutex::new(hold);
        let run: Arc<RunFn> = Arc::new(
            move |job: &Job,
                  _: &Settings,
                  _: &mut History,
                  cancel: &CancelToken,
                  _: runner::QueueTurn<'_>,
                  _: &mut dyn FnMut(&RunEvent)| {
                log.lock().unwrap().push(job.name.clone());
                let mut held = hold.lock().unwrap();
                if let Some((name, gate)) = held.as_ref()
                    && *name == job.name
                {
                    while gate.recv_timeout(Duration::from_millis(10)).is_err()
                        && !cancel.is_cancelled()
                    {}
                    held.take();
                }
                let outcome = if cancel.is_cancelled() {
                    Outcome::Failed
                } else {
                    Outcome::Ok
                };
                report(job, outcome)
            },
        );
        (run, started)
    }

    /// Collects the worker's events until `done` says so, within a generous bound.
    fn events_until(worker: &Worker, done: impl Fn(&[WorkerEvent]) -> bool) -> Vec<WorkerEvent> {
        let mut seen = Vec::new();
        let until = Instant::now() + Duration::from_secs(30);
        while !done(&seen) {
            assert!(Instant::now() < until, "timed out: {seen:?}");
            if let Ok(e) = worker.events.recv_timeout(Duration::from_millis(20)) {
                seen.push(e);
            }
        }
        seen
    }

    /// Stop after a job was handed over but before the worker started it (here, while its
    /// history is slow to open): the job never starts.
    #[test]
    fn stop_reaches_a_job_handed_over_but_not_started() {
        let (open, opened) = mpsc::channel::<()>();
        let (run, started) = recording(None);
        let mut worker = Worker::with(
            move || {
                let _ = opened.recv();
                History::in_memory().map_err(|e| e.to_string())
            },
            run,
        );
        worker.run(job("hive"), Settings::default());
        worker.stop_all();
        open.send(()).unwrap();
        let events = events_until(&worker, |e| !e.is_empty());
        assert!(
            matches!(events.as_slice(), [WorkerEvent::Cancelled(name)] if name == "hive"),
            "{events:?}"
        );
        assert!(
            started.lock().unwrap().is_empty(),
            "the backup never started"
        );
        worker.shut_down();
    }

    /// Stop while one job runs and another waits behind it: the running one is cancelled and
    /// the waiting one never starts.
    #[test]
    fn stop_cancels_the_running_job_and_skips_the_next() {
        let (_release, gate) = mpsc::channel();
        let (run, started) = recording(Some(("first", gate)));
        let mut worker = Worker::with(|| History::in_memory().map_err(|e| e.to_string()), run);
        worker.run(job("first"), Settings::default());
        worker.run(job("second"), Settings::default());
        events_until(&worker, |e| matches!(e, [WorkerEvent::Started(_), ..]));
        worker.stop_all();
        let events = events_until(&worker, |e| {
            e.iter().any(|e| matches!(e, WorkerEvent::Cancelled(_)))
        });
        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                WorkerEvent::Finished(r) => Some((r.job.as_str(), r.outcome)),
                _ => None,
            })
            .collect();
        assert_eq!(finished, [("first", Outcome::Failed)], "cancelled mid-run");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WorkerEvent::Cancelled(n) if n == "second"))
        );
        assert_eq!(*started.lock().unwrap(), ["first"]);
        worker.shut_down();
    }

    /// A job waiting its turn is offered to give way while a job behind it could run:
    /// it gives way, nothing is finished, and the next try counts its wait from the first.
    #[test]
    fn a_waiting_job_gives_way_to_one_behind_it_that_could_run() {
        let tries: Arc<Mutex<Vec<Duration>>> = Arc::default();
        let seen = Arc::clone(&tries);
        let run: Arc<RunFn> = Arc::new(
            move |job: &Job,
                  _: &Settings,
                  _: &mut History,
                  _: &CancelToken,
                  turn: runner::QueueTurn<'_>,
                  _: &mut dyn FnMut(&RunEvent)| {
                seen.lock().unwrap().push(turn.waited);
                // As a run waiting for a busy repository does: asks, and gives way if told to.
                let mut r = report(job, Outcome::Ok);
                r.gave_way = turn.give_way.is_some_and(|ask| ask());
                r
            },
        );
        let mut worker = Worker::with(|| History::in_memory().map_err(|e| e.to_string()), run);
        // A job with no repositories behind it is always free to run.
        worker.set_behind(vec![job("behind")]);
        worker.run(job("waiting"), Settings::default());
        let events = events_until(&worker, |e| {
            e.iter().any(|e| matches!(e, WorkerEvent::GaveWay(_)))
        });
        assert!(
            !events.iter().any(|e| matches!(e, WorkerEvent::Finished(_))),
            "{events:?}"
        );
        // Tried again with nothing behind it: it runs to the end.
        worker.set_behind(Vec::new());
        worker.run(job("waiting"), Settings::default());
        events_until(&worker, |e| {
            e.iter().any(|e| matches!(e, WorkerEvent::Finished(_)))
        });
        let tries = tries.lock().unwrap().clone();
        assert_eq!(tries.len(), 2);
        assert!(
            tries[1] >= tries[0],
            "the wait counts from the first try: {tries:?}"
        );
        worker.shut_down();
    }

    /// Quit stops what was handed over, too.
    #[test]
    fn shutting_down_stops_a_job_handed_over() {
        let (open, opened) = mpsc::channel::<()>();
        let (run, started) = recording(None);
        let mut worker = Worker::with(
            move || {
                let _ = opened.recv();
                History::in_memory().map_err(|e| e.to_string())
            },
            run,
        );
        worker.run(job("hive"), Settings::default());
        let token = worker
            .dispatched
            .lock()
            .unwrap()
            .front()
            .cloned()
            .expect("registered when handed over");
        // Lets the worker go only once Quit has cancelled the job (or after a bound for a hung
        // test), so the worker can't start it first however the threads are scheduled.
        let release = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(30);
            while !token.is_cancelled() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = open.send(());
            token.is_cancelled()
        });
        worker.shut_down();
        assert!(release.join().unwrap(), "Quit cancelled the dispatched job");
        assert!(started.lock().unwrap().is_empty(), "and it never started");
    }
}
