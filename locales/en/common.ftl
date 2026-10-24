# Strings used in more than one place: the app's name, run results and why a run was skipped.
# Conventions: docs/ARCHITECTURE.md, "Strings and languages".

app-name = Replica
# A build for another system: Replica v1 runs on Windows only.
app-windows-only = Replica runs on Windows only in this version.

## Run results: the CLI, the tray, History and notifications

result-ok = OK
result-failed = Failed
result-partly = Partly done
result-skipped = Skipped
result-running = Running
result-never-run = Never run

## Why a run or a target was skipped

skip-target-not-available = the target isn't available
skip-job-disabled = the job is turned off
skip-schedules-paused = schedules are paused
skip-already-running = the job is already running

## Waiting for a repository another run is using

# $job: the job using it; $target: the target's name.
wait-turn = Waiting for { $job } to finish with { $target }
wait-turn-unknown = Waiting for another job to finish with { $target }
wait-restic-lock = Waiting for another restic to finish with { $target }
wait-gave-way = It let a job queued behind it go first.
# Who holds a repository while the window sets it up, in other runs' "Waiting for … to finish".
holder-window = the Replica window

## Job modes

mode-versioned = Versioned
mode-mirror = Mirror
# A versioned job that keeps every backup, forever.
mode-versioned-forever = Versioned · Forever

## Where Replica keeps its files: the Settings tab and `replica status`

where-title = Where Replica keeps its files
where-mode = Mode
where-mode-portable = Portable: everything is beside the exe
where-mode-installed = Installed
where-folder = Folder
where-settings-file = Settings file
where-history = Run history
where-logs = Logs
where-run-logs = Run logs
where-not-writable = can't be written to
