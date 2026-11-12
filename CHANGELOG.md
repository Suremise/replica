# Changelog

All notable changes to Replica are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Replica follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The window has a menu bar (File, Jobs, View, Tools, Help) with keyboard shortcuts (Ctrl+N for
  a new job, Ctrl+1/2/3 for the pages, Ctrl+, for Settings), a status bar showing what's running,
  the next scheduled run, jobs with problems and what runs the schedules (View > Status bar hides
  it; `show_status_bar` in `settings.toml` remembers that), and a compact sidebar with Jobs,
  History and Restore. Settings and About are now in the Tools and Help menus. File > Exit Replica
  quits Replica as the tray's Quit does.
- The window's pages have a compact, quieter look: smaller type, denser tables with a tinted
  header and hairlines between rows, flat command buttons, squarer status chips with a slim bar
  while a job runs, and paths, targets, snapshot IDs and sizes in monospace. A job's schedule has
  a column of its own, with what runs it (the built-in timer or Task Scheduler) on hover.

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
