# Changelog

All notable changes to Replica are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Replica follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-11-13

Schedules gets a page of its own where you set each job's schedule, every table fills the window
and is paged, and Replica can tell you when there's a new release.

**Not signed yet:** Windows SmartScreen may warn the first time you run the installer or the
exes ("Windows protected your PC"). Check the download against the release's `SHA256SUMS.txt`
first (`sha256sum -c SHA256SUMS.txt --ignore-missing`, or compare `Get-FileHash <file>` with
it), and if it matches, choose **More info**, then **Run anyway**.

Installing it over 0.3.0 keeps your settings, jobs, history and saved passwords.

**If you've used Replica before:** **Settings** has moved from the Tools menu to the window's
sidebar, beside a new **Schedules** page that holds Replica's Task Scheduler tasks (which were
in Settings). The Tools menu holds only Windows Task Scheduler. **Check for updates** is off
until you turn it on (Settings > General, or the tray menu); **Check now** in About asks once
whenever you like.

### Added

- The Schedules page sets a job's schedule: choose the job, then when it runs, with the job
  editor's own choices, and see when it next runs and what runs it. A versioned job's backup
  check has its schedule there too. Save writes it as the editor does; Edit job… opens the
  editor for the rest.
- **Check for updates**, off by default (in Settings, General, and the tray menu, beside
  Start with Windows). While it's on, Replica asks GitHub which release is the latest when it
  starts and once a day while it runs, and tells you of a newer one: a notification that opens
  the release's page, a note in the status bar and the same in About. **Check now** in About
  (or Help > Check for updates…) asks once whenever you like, and says whether you have the
  latest version or why it couldn't check. Replica never downloads or installs anything by
  itself: it links to the release on GitHub. This is the only time Replica goes on the
  internet.

### Changed

- Schedules and Settings are in the window's sidebar (Settings at its foot) and in the View
  menu, with Ctrl+4 for Schedules and Ctrl+, for Settings as before. The Tools menu keeps only
  Windows Task Scheduler.
- Replica's tasks in Task Scheduler have a page of their own, **Schedules**, rather than a section
  of Settings. Settings' tab for what runs the schedules and pausing them is now **Scheduling**,
  and links to the page.
- The sidebar no longer repeats Replica's logo and name at its top: the title bar has them.
- Every table (Jobs, History, Restore's snapshots, Schedules' tasks) shows a page of rows at a
  time, with a pager under it: which rows show ("51–100 of 312"), first, previous, next and
  last page, and **Rows per page** (25, 50 or 100, one choice for every table, remembered).
  Page Up and Page Down turn the page. Sorting, filtering and searching cover the whole list
  and go back to the first page. History reads only the page you're on, so it stays quick
  however long it gets, and its search now looks through every run, not just those loaded;
  **Show older** is gone. Restore's snapshots are no longer in a small box of their own.

### Fixed

- The window's title bar, taskbar button and Alt+Tab show Replica's icon rather than egui's
  default one, and keep it at every display scale.
- Tables (Jobs, History, Restore's snapshots and Schedules' tasks) fill the window's width
  when it's maximised or made wider, as the rest of the page does, and fit again when it's
  made narrower. Before, a table kept the column widths it was first shown with. A column
  you resize by hand keeps its width, and the widest column (a job's or task's name, or a
  snapshot's tags) takes up the rest.

## [0.3.0] - 2026-11-12

The window gets a menu bar, a status bar and a compact new look, and the SQLite built into
Replica gets a security update.

**Not signed yet:** Windows SmartScreen may warn the first time you run the installer or the
exes ("Windows protected your PC"). Check the download against the release's `SHA256SUMS.txt`
first (`sha256sum -c SHA256SUMS.txt --ignore-missing`, or compare `Get-FileHash <file>` with
it), and if it matches, choose **More info**, then **Run anyway**.

Installing it over 0.2.0 keeps your settings, jobs, history and saved passwords.

**If you've used Replica before:** Settings is now in the **Tools** menu (or Ctrl+,) and About
in the **Help** menu, rather than alongside Jobs, History and Restore.

### Changed

- The window has a menu bar (File, Jobs, View, Tools, Help) with keyboard shortcuts (Ctrl+N for
  a new job, Ctrl+1/2/3 for the pages, Ctrl+, for Settings), and a compact sidebar with Jobs,
  History and Restore. File > Exit Replica quits Replica as the tray's Quit does.
- A status bar along the bottom of the window shows what's running, the next scheduled run, jobs
  with problems and what runs the schedules. View > Status bar hides it, and Replica remembers
  that (`show_status_bar` in `settings.toml`).
- The window's pages have a compact, quieter look: smaller type, denser tables with a tinted
  header and hairlines between rows, flat command buttons, squarer status chips with a slim bar
  while a job runs, and paths, targets, snapshot IDs and sizes in monospace. A job's schedule has
  a column of its own, with what runs it (the built-in timer or Task Scheduler) on hover.

### Security

- The SQLite built into Replica is updated to 3.54.1 (rusqlite 0.41.1), for a security fix:
  older versions could return uninitialised memory when opening a database whose `-wal` file
  had been crafted with a different page size (RUSTSEC-2026-0459, CVE-2026-119375).

## [0.2.0] - 2026-11-02

Mirror runs now list every file/folder clash with what to do, and the tray icon's tooltip always shows.

**Not signed yet:** Windows SmartScreen may warn the first time you run the installer or the
exes ("Windows protected your PC"). Check the download against the release's `SHA256SUMS.txt`
first (`sha256sum -c SHA256SUMS.txt --ignore-missing`, or compare `Get-FileHash <file>` with
it), and if it matches, choose **More info**, then **Run anyway**.

Installing it over 0.1.0 keeps your settings, jobs, history and saved passwords.

### Changed

- A mirror run that finds a file in one place and a folder of the same name in the other still
  leaves it as it is, and now lists every such clash together once the run ends, each saying what
  to do (delete or rename the item in the mirror, then run again). History's run details and
  `replica run` show them all, and the notification shows the first.

### Fixed

- The tray icon's tooltip could stop showing on hover after the icon changed state or showed
  a notification, until its text next changed. It now always shows.

## [0.1.0] - 2026-10-24

The first release.

**Not signed yet:** Windows SmartScreen may warn the first time you run the installer or the
exes ("Windows protected your PC"). Check the download against the release's `SHA256SUMS.txt`
first (`sha256sum -c SHA256SUMS.txt --ignore-missing`, or compare `Get-FileHash <file>` with
it), and if it matches, choose **More info**, then **Run anyway**.

### Added

- **Versioned backups** with a bundled, pinned [restic](https://restic.net) 0.19.1: encrypted,
  deduplicated snapshots on a local or external drive (found by its volume ID or label, whatever
  its letter) or a NAS share (by UNC path, with its login in Windows Credential Manager), with
  daily, weekly and monthly retention and a regular `restic check`.
- **Mirror jobs**, an exact copy, only when asked for: they stop without deleting anything if a
  run would delete more than a set share of the target's files.
- **Jobs in `settings.toml`**, with every key documented: sources with exclusions, several
  targets, schedules, and before and after steps (a consistent SQLite database snapshot, commands
  that run in their own process tree with a timeout, and verify). A save, or an export over a
  file, appears whole at once: two at the same moment (the window and the tray, say) each leave a
  whole file rather than failing, and other files beside it are left alone.
- **The command line**, `replica.exe`: `run`, `list`, `status`, `snapshots`, `restore`, `verify`,
  `init`, `key show`, `key set`, `login set`, `export`, `import` and `tasks`, with exit codes for
  scripts (0 ok, 1 failed, 2 partly done, 3 skipped, 4 a usage or settings error) and `--json`
  output for other programs.
- **Restore** of a file or folder from any of a job's snapshots, into a new or empty folder, asked
  for by any of its names (its short 8.3 name, in another case or through a junction) whichever
  the source was backed up by, and a `HOW-TO-RESTORE.txt` beside each repository for restoring
  without Replica.
- **Repository passwords** made by Replica, shown once to be saved, and kept in Windows
  Credential Manager. A saved copy can be put back on a new PC (`replica key set`, or **Enter a
  saved password…** in the window), only once it's checked against the repository.
- **The tray app**, `replica-tray.exe`: run now, pause schedules, start with Windows, and an icon
  that shows running, failed and overdue; notifications on failure, and on success if a job asks.
- **Schedules** by the tray app's built-in timer, or optionally by Windows Task Scheduler (off by
  default), with every task visible and manageable.
- **The window:** every job listed and editable (with the mirror warning and the
  repository-password prompt), run history, browsing a snapshot to restore files and folders, a
  test restore, Settings with the Scheduled-tasks view, and About. Closing it keeps Replica in
  the notification area. Screen readers can read every control, and a choice made in a dropdown
  through one closes its list, so the next press opens it again. File dialogs open over the
  window. If the window is held up, the tray's Open brings it forward with any dialog it has
  open, and if it can't, or the window has stopped, Open says so rather than doing nothing; the
  log records the window's steps and any fault.
- **Installed or portable:** settings in `%APPDATA%\Replica\`, or beside the exe. A per-user MSI
  (no admin rights, adds `replica.exe` to the PATH) and a portable zip, each with the bundled
  restic, the licence and THIRD-PARTY-NOTICES, and SHA-256 checksums.
- **Jobs that share a backup folder take turns**, across the tray, schedules and the command line:
  a job waits (up to two hours, `repository_wait_minutes`) for the other to finish with the
  repository instead of failing on restic's lock, and shows **Waiting its turn**. A restic started
  outside Replica is waited for too, up to 10 minutes. Restores, test restores, verifies and
  setting a repository up take their turn too. Run all never stalls behind a busy
  folder: jobs on free ones go first. Mirror jobs whose folders overlap each other or a repository
  are refused, however the path is written. The Jobs page shows **Overdue** for a job whose last
  good backup is too old.
- **Keep forever and Pin this backup:** a job can keep every backup, forever, and any one
  snapshot can be pinned (the Restore page, or `replica pin`) so no retention removes it until it's
  unpinned.
- **Export and import jobs:** one, some or all jobs to a file (`replica export`), and jobs from
  one (`replica import`), with what happens to each shown first. The file holds no passwords, a
  job that doesn't validate is never imported, and imported jobs arrive turned off.
- **No console windows:** restic and command steps run without a window of their own, their
  output in the run's log, so nothing flashes up while a job runs.
- Translation-ready: every message comes from Fluent strings. English only for now.
- No telemetry and no network except the targets you set up.

[Unreleased]: https://github.com/Suremise/replica/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Suremise/replica/releases/tag/v0.2.0
[0.1.0]: https://github.com/Suremise/replica/releases/tag/v0.1.0
