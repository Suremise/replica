# Changelog

All notable changes to Replica are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Replica follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first version, in development towards v1.

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
  `init`, `key show`, `key set`, `login set`, `export`, `import` and `tasks`, with exit codes for scripts (0 ok, 1 failed, 2 partly done, 3
  skipped, 4 a usage or settings error) and `--json` output for other programs.
- **Restore** of a file or folder from any of a job's snapshots, into a new or empty folder, asked
  for by any of its names (its short 8.3 name, in another case or through a junction) whichever
  the source was backed up by, and a `HOW-TO-RESTORE.txt` beside each repository for restoring without Replica.
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

[Unreleased]: https://github.com/Suremise/replica/commits/main
