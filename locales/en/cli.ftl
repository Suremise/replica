# replica.exe: messages and results. --json keys are never translated.
# $job is a job's name, $target a target's name or path, $duration a ready-made duration such as "2 min 5 s".

## replica run

cli-run-started = Running { $job }…
cli-run-target-ok = { $job } → { $target }: OK ({ $duration })
cli-run-target-failed = { $job } → { $target }: failed: { $reason }
cli-run-target-skipped = { $job } → { $target }: skipped, { $reason }
cli-run-job-ok = { $job }: OK
cli-run-job-failed = { $job }: failed
cli-run-job-partly = { $job }: partly done, { $ok } of { $total } targets OK
cli-run-job-skipped = { $job }: skipped, { $reason }
cli-run-all-summary = { $ok } OK, { $failed } failed, { $partly } partly done, { $skipped } skipped
cli-run-no-jobs = There are no jobs to run. Add one in Replica's window or in settings.toml.

## replica list and replica status

cli-list-job = Job
cli-list-last-result = Last result
cli-list-last-run = Last run
cli-list-next-run = Next run
cli-list-not-scheduled = not scheduled
cli-list-overdue = overdue
cli-list-disabled = off
# After a job's name in `replica list` when it keeps every backup, forever.
cli-list-forever = keeps everything
cli-status-running = { $job } is running ({ $percent }%).
cli-status-idle = Nothing is running.

## replica snapshots, restore and verify

cli-snapshots-none = { $job } has no snapshots yet.
# After a snapshot's line in `replica snapshots`.
cli-snapshots-pinned = pinned
# $snapshot: the snapshot's short ID, new: restic gives a snapshot a new ID when it's pinned.
cli-pin-done = Pinned { $job } → { $target }: kept whatever the job's retention. Its ID is now { $snapshot }.
cli-unpin-done = Unpinned { $job } → { $target }: the job's retention can delete it again. Its ID is now { $snapshot }.
cli-restore-done = Restored { $path } from snapshot { $snapshot } to { $to }.
cli-verify-ok = { $job } → { $target }: the repository checks out.
cli-verify-failed = { $job } → { $target }: the check found problems: { $reason }

## replica init, replica key show and replica key set

cli-init-done = Created the repository for { $job } at { $target }.
cli-key-show = The repository password for { $job } → { $target }:
cli-key-set-prompt = The repository password you saved for { $target } (it isn't shown as you type):
cli-key-set-saved = Saved the repository password for { $job } → { $target }: it opens the backup there.
cli-key-set-replaced = Saved the repository password for { $job } → { $target } in place of the one saved before: it opens the backup there.
cli-key-set-unchanged = That's the repository password saved for { $job } → { $target } already: nothing changed.
# $detail: why the input couldn't be read, from Windows.
cli-secret-not-read = Nothing was saved: the password couldn't be read ({ $detail }).

## replica login set

cli-login-user-prompt = The user name for the share login { $name }:
cli-login-password-prompt = Its password (it isn't shown as you type):
cli-login-password-again = The password again:
cli-login-password-empty = Nothing was saved: the password is empty.
cli-login-password-mismatch = Nothing was saved: the two passwords aren't the same.
cli-login-set-saved = Saved the share login { $name } in Windows Credential Manager.
cli-login-set-replaced = Saved the share login { $name } in Windows Credential Manager, in place of the one saved before.
cli-login-set-unused = No share target in the settings uses the login { $name } yet: a share target names it with credential = "{ $name }".
cli-login-name-empty = Give the login a name: the credential a share target names in settings.toml.
cli-login-name-reserved = { $name } can't be a login's name: Replica keeps repository passwords under names that start with replica:repo:.

## replica export and replica import

# $count: how many jobs; $path: the jobs file.
cli-export-done = Exported { $count ->
    [one] 1 job
   *[other] { $count } jobs
} to { $path }. It holds no passwords: on another PC, enter each repository's saved password once it's imported.
cli-export-names-or-all = Name the jobs to export, or use --all, but not both.
cli-export-no-jobs = There are no jobs to export.
cli-import-from = { $path } has { $count ->
    [one] 1 job
   *[other] { $count } jobs
}:
cli-import-not-in-file = { $path } has no job called { $job }.
cli-import-new = { $job }: imported.
# $name: the copy's new name; $existing: the job here with the name it had.
cli-import-copy = { $job }: imported as { $name }, since a job here is already called { $existing }.
cli-import-replace = { $job }: imported in place of { $name } here, which keeps its history.
cli-import-skip = { $job }: skipped, since a job here is already called { $existing }. Use --on-conflict copy or replace to import it.
# Followed by one line per reason.
cli-import-refused = { $job }: not imported, it has problems:
cli-import-needs-password = { "  " }{ $target } has no saved repository password on this PC: enter the one you saved with replica key set { $job }, or in the job's editor.
cli-import-drive-not-found = { "  " }The drive for { $target } isn't connected now.
# $cmd: a command one of the job's steps runs.
cli-import-runs = { "  " }It runs the command: { $cmd }
cli-import-summary = { $imported } imported, { $skipped } skipped, { $refused } not imported.
cli-import-turned-off = Imported jobs are off: turn each on in Replica's window, or with enabled = true in settings.toml, once you've checked it.
cli-import-not-saved = Nothing was imported: the settings couldn't be saved.

## Usage and settings errors (exit code 4)

cli-usage-error = { $detail }
cli-see-help = Run `replica --help` for the commands.

## HOW-TO-RESTORE.txt, written beside each versioned target's repository by replica init and the window
# $repository: the repository's folder.
restore-guide =
    This folder is a restic backup repository, made by Replica.

    Its contents are encrypted. Any copy of restic (https://restic.net) can read them with the
    repository password, which you saved when this backup was set up. Replica isn't needed.

    To list the snapshots:
        restic -r "{ $repository }" snapshots

    To restore the latest snapshot into a new, empty folder:
        restic -r "{ $repository }" restore latest --target "C:\Restored"

    To restore one snapshot, use its ID from the list instead of "latest".
    restic asks for the password each time. This file holds no password or other secret.

## replica --help (and the rest of the command line)

cli-help-about = Versioned backups and mirrors for Windows.
cli-help-exit-codes =
    Exit codes: 0 ok, 1 failed, 2 partly done, 3 skipped (a target isn't available),
    4 a usage or settings error. --json output is described in docs/cli-json.md.
cli-help-run = Run a job now, or every job that's on.
cli-help-list = List the jobs: their last result, last run and next run.
cli-help-status = As list, and which jobs are running now.
cli-help-snapshots = List a versioned job's snapshots.
cli-help-restore = Restore a file or folder from a snapshot into a new or empty folder.
cli-help-verify = Check a versioned job's repositories, reading back part of their data.
cli-help-pin = Pin a snapshot: it's kept whatever the job's retention.
cli-help-unpin = Unpin a snapshot, so the job's retention can delete it again.
cli-help-init = Set up a job's targets: the folder, and for a versioned job the repository and its password.
cli-help-key = A versioned target's repository password.
cli-help-key-show = Show a versioned target's repository password.
cli-help-key-set = Save a repository password you kept (on a new PC, say), once it's checked against the repository.
cli-help-login = A share's login, for share targets.
cli-help-login-set = Save a share login in Windows Credential Manager.
cli-help-arg-job = The job's name.
cli-help-arg-all = Every job that's on.
cli-help-arg-quiet = Print nothing unless something goes wrong (for Run on close).
cli-help-arg-json = Print JSON for other programs (see docs/cli-json.md).
cli-help-arg-snapshot = The snapshot's ID, or "latest".
cli-help-arg-path = The file or folder, as it was backed up.
cli-help-arg-to = The folder to restore into: it must be new or empty.
cli-help-arg-target = Only this target, by its name.
cli-help-arg-overwrite = Allow restoring into the original place (or into a folder with files), replacing what's there.
cli-help-arg-force = Replace a saved password even though it opens the repository (the new one must open it too).
cli-help-arg-login = The login's name: the credential a share target names in settings.toml.
cli-help-arg-user = The user name (without it, you're asked).
cli-help-export = Export jobs to a jobs file, to import on another PC or keep as a copy. It holds no passwords.
cli-help-import = Import jobs from a jobs file. They arrive turned off.
cli-help-arg-export-jobs = The jobs to export, by name.
cli-help-arg-export-all = Every job.
cli-help-arg-output = The jobs file to write (such as jobs.replica-jobs.toml).
cli-help-arg-replace-file = Replace the file if it exists.
cli-help-arg-import-file = The jobs file to import.
cli-help-arg-only = Import only these jobs, by name.
cli-help-arg-on-conflict = What to do with a job whose name is taken here: skip it (the default), import it as a copy under a new name, or replace the job here.

# A duration: $s seconds (with one decimal), or $min minutes and $s seconds.
cli-duration-seconds = { $s } s
cli-duration-minutes = { $min } min { $s } s
cli-run-job-or-all = Name a job, or use --all, but not both.
cli-run-target-partly = { $job } → { $target }: partly done, { $reason }
cli-status-running-job = { $job } is running.
cli-not-versioned = { $job } is a mirror: this works only for versioned jobs.
cli-no-target = None of { $job }'s targets could be used.
cli-init-folder-done = Created the folder for { $job } at { $target }.
cli-init-exists = { $job } → { $target } is set up already.
# The word the user types to confirm they've saved the password.
cli-init-confirm-word = saved
cli-init-confirm = Type "{ $word }" and press Enter once you've saved a copy:
cli-init-not-confirmed = Not confirmed, so nothing was saved and no repository was made. Run replica init again when you're ready.
