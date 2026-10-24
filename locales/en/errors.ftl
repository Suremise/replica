# Errors, shown in the CLI, the window, the log and notifications.

error-target-not-available = { $target } isn't available. Check that the drive is connected or the share can be reached.
error-wrong-password = The repository password for { $target } is wrong.
error-password-missing = There's no saved repository password for { $target } on this PC. If you have the copy you saved, enter it with replica key set and the job's name, or with "Enter a saved password…" in the job's editor.
error-credentials-missing = There's no saved login for { $target }.
error-settings-corrupt = The settings file { $path } can't be read: { $detail }
error-job-not-found = There's no job called { $job }.
error-target-not-found = { $job } has no target called { $target }.
error-restic-missing = restic wasn't found at { $path }.
error-paths-no-exe = Replica couldn't tell which folder it's running from: { $detail }
error-paths-no-appdata = The APPDATA environment variable isn't set, so Replica doesn't know where to keep its settings.
error-restic-failed = restic failed: { $detail }
# The detail of a restic error: a backup that finished without saying what it did.
error-restic-no-summary = the backup ended without restic's summary of what it did
error-before-step-failed = The before step "{ $step }" failed: { $detail }
error-after-step-failed = The after step "{ $step }" failed: { $detail }
error-step-timed-out = The step "{ $step }" didn't finish within { $timeout }.
error-snapshot-not-found = { $job } has no snapshot { $snapshot }.
error-history-corrupt = The run history { $path } can't be read: { $detail }
error-already-running = { $job } is already running.

## The app's folder (portable or installed): shown by the CLI (exit code 4) and as a tray notice

error-portable-not-writable = Replica is running portable, but its folder { $path } can't be written to. Settings are kept in memory until it closes, and nothing is saved.
error-app-folder-not-writable = Replica's folder { $path } can't be written to. Settings are kept in memory until it closes, and nothing is saved.
error-app-folder-unknown = Replica can't work out where to keep its files: { $detail }

## Settings: reading settings.toml, checking jobs, schedules and exclude patterns

# $detail: the operating system's or the TOML reader's own message, in English.
error-settings-io = Can't read or write { $path }: { $detail }
error-settings-corrupt-at = The settings file { $path } can't be read (line { $line }, column { $column }): { $detail }
# Followed by one line per problem.
error-settings-invalid = The settings file { $path } has problems:
warning-settings-unknown-key = Unknown setting "{ $key }" ignored.

## Jobs files: exporting and importing jobs

# $detail: the operating system's or the TOML reader's own message, in English.
error-jobs-file-io = Can't read or write the jobs file { $path }: { $detail }
error-jobs-file-exists = { $path } already exists. Choose another name, or replace it (--force on the command line).
error-jobs-file-corrupt = This jobs file can't be read: { $detail }
error-jobs-file-corrupt-at = This jobs file can't be read (line { $line }, column { $column }): { $detail }
# $format: the format a jobs file declares, "replica-jobs".
error-jobs-file-not-jobs = This isn't a file of Replica jobs: it doesn't say format = "{ $format }" and a version. Choose a file made by Export.
# $version: the file's format version; $supported: the newest this Replica reads.
error-jobs-file-newer = This jobs file was made by a newer Replica (format version { $version }; this one reads up to { $supported }). Update Replica to import it.
error-jobs-file-jobs-not-tables = its jobs aren't [[jobs]] tables.
# $job: the job's name, or its position such as jobs[2].
error-jobs-file-job-unreadable = Job { $job } can't be read: { $detail }
# $job: the job's name, or its position such as jobs[2]; $problem: one of the error-job- texts below.
error-job-problem = Job { $job }: { $problem }
error-job-name-missing = it has no name.
# $max: the longest name allowed, in characters.
error-job-name-invalid = the name "{ $name }" may only use letters, digits, ".", "_" and "-", start with a letter or digit, and be at most { $max } characters.
error-job-name-duplicate = another job is also called "{ $name }" (names ignore case).
error-job-no-sources = it needs at least one source.
error-job-no-targets = it needs at least one target.
error-job-source-not-absolute = the source "{ $path }" must be an absolute path.
error-job-drive-without-volume = a drive target needs a volume_id or a label.
error-job-drive-path-outside = a drive target's path "{ $path }" must be a folder inside the drive, such as Backups\Replica: no drive letter, no leading \ and no "..".
error-job-unc-invalid = "{ $path }" isn't a UNC path like \\server\share\folder.
error-job-credential-empty = a target's credential name is empty.
error-job-credential-reserved = a target's credential "{ $name }" is the name of a repository password, which a share login can't use.
error-job-mirror-not-confirmed = a mirror copies deletions too: set mirror_deletes_confirmed = true to accept that.
error-job-verify-on-mirror = a verify step only works on a versioned job.
error-job-empty-command = a command step has no command.
error-job-zero-timeout = a command step's timeout must be at least a second.
error-job-retention-keeps-nothing = its retention keeps no snapshots.
error-job-delete-limit = its mirror_delete_limit_percent must be between 0 and 100.
# $target: this job's target; $other: the other job's name; $other_target: its target.
error-job-mirror-overlaps-mirror = its mirror target { $target } is the same folder as { $other }'s mirror target { $other_target }, or inside or around it, so each would delete the other's files.
error-job-mirror-overlaps-repository = its mirror target { $target } is the same folder as { $other }'s backup repository { $other_target }, or inside or around it, so the mirror would delete the repository's files.
# $reason: one of the error-exclude- reasons below.
error-exclude-invalid = the exclude pattern "{ $pattern }" can't be used: { $reason }
error-exclude-empty = it's empty.
error-exclude-absolute = it must be relative to its source.
error-exclude-braces = alternatives in braces aren't supported; add a pattern for each.
error-exclude-dots = it can't contain empty, "." or ".." parts.
error-exclude-bracket = it has an unclosed [ ].
error-exclude-class = a [ ] class may only list characters and ranges, like [abc] or [a-z0-9]; [!a] and [^a] aren't supported.
# $detail: the pattern library's message, in English.
error-exclude-glob = it isn't a valid pattern ({ $detail }).
# $reason: one of the error-schedule- reasons below.
error-schedule-invalid = The schedule "{ $text }" isn't valid: { $reason }
error-schedule-time = expected a time like 02:30.
error-schedule-days = expected days like mon,thu.
error-schedule-interval = expected an interval like 6h or 90m.
error-schedule-form = expected "daily HH:MM", "weekly DAYS HH:MM", "every Nh" or "every Nm".
# $volume: the drive's label in quotes, or its volume ID.
error-drive-not-connected = The drive { $volume } isn't connected.

## Windows targets, Credential Manager and repository passwords (replica-win)

# $share: the share's root, such as \\nas\backup; $detail: Windows' own message, in English.
error-share-unreachable = { $share } can't be reached: { $detail }
# $seconds: a whole number.
error-share-timeout = { $share } didn't answer within { $seconds } seconds.
# $user: the user name of the login saved for the target.
error-share-login-refused = { $share } refused the login for { $user }. Check the user name and password saved for this target in Windows Credential Manager.
error-share-access-denied = { $share } refused access. If it needs a login, save one in Windows Credential Manager and name it in the target's credential setting.
error-share-other-login = { $share } is already connected with a different login. Disconnect it (net use { $share } /delete) and try again.
error-share-failed = Couldn't connect { $share }: { $detail }
# The $detail of error-share-unreachable when the quick check finds nothing listening.
error-share-smb-no-answer = { $server } doesn't answer on the SMB port (445).
# The $detail when Windows reports a failure without an error code.
error-share-no-reason = Windows gave no reason.
error-volume-label-ambiguous = More than one connected drive is labelled "{ $label }". Give the target the drive's volume ID instead.
error-volumes-failed = Couldn't list the connected drives: { $detail }
error-credential-read = Couldn't read "{ $name }" from Windows Credential Manager: { $detail }
error-credential-write = Couldn't save "{ $name }" in Windows Credential Manager: { $detail }
error-credential-delete = Couldn't remove "{ $name }" from Windows Credential Manager: { $detail }
error-credential-unreadable = "{ $name }" in Windows Credential Manager isn't text Replica can read.
# The $detail when a change was reported done and wasn't kept, each time it was tried.
error-credential-not-kept = Credential Manager reported the change done, but didn't keep it.
# The $detail when another of Replica's calls held Credential Manager for too long.
error-credential-busy = Another part of Replica is using Credential Manager. Try again in a moment.
# A repository password exists already: replacing it would lock Replica out of that backup.
error-key-exists = A repository password for { $target } is saved already. Replica won't replace it: that would lock it out of the backup there.
error-key-missing = There's no repository password for { $target } in Windows Credential Manager. The backup there can only be read with the copy you saved: replica key set puts it back.
error-key-entered-wrong = That password doesn't open the backup at { $target }, so it wasn't saved.
error-key-saved-works = The password saved for { $target } opens the backup there already, so it wasn't replaced. To replace it anyway with another that opens it too, use --force.
error-key-busy = Another Replica is saving a repository password, so this one wasn't saved. Try again in a moment. ({ $detail })
error-random-failed = Windows couldn't generate a random password: { $detail }

## restic: the engine for versioned targets

# $detail: the operating system's message, in English.
error-restic-spawn = restic at { $path } couldn't be started: { $detail }
# $detail: the operating system's message, in English.
error-restic-not-stopped = restic or the programs it started had to be stopped, but couldn't all be: they may still be running. ({ $detail })
# $detail: restic's own message, in English.
error-restic-failed-code = restic failed (exit code { $code }): { $detail }
error-repo-not-found = There's no restic repository at { $location } yet. Set the target up first (replica init).
# $detail: restic's message about who holds the lock, in English.
error-repo-locked = The repository at { $location } is in use by another restic: { $detail }
error-cancelled = Cancelled.
error-restore-target-not-empty = { $path } already exists and isn't empty. Restore to a new or empty folder.
error-restore-over-source = { $path } is a source of the job, or inside or around one, so restoring there could overwrite live files.
# $detail: the operating system's message, in English.
error-restore-target-invalid = Where { $path } leads couldn't be worked out, so nothing was restored: { $detail }
# $item: the file or folder's path in the snapshot.
error-restore-nothing = { $item } wasn't restored: restic found nothing by that name in the snapshot.
error-restore-same-name = Two of the items have the same name ({ $name }), so one would replace the other in the restore folder. Restore them one at a time, to different folders.
# $detail: what couldn't be read, in English.
error-restic-bad-output = restic's output couldn't be read: { $detail }
warning-restic-version = restic { $found } is in use, but Replica is tested with { $pinned }.

## The mirror engine

error-mirror-source-missing = The source { $path } doesn't exist, so nothing was mirrored.
error-mirror-source-empty = The source { $path } has no files. If its drive or share isn't connected, mirroring it would empty the mirror, so nothing was changed.
error-mirror-source-root = { $path } is the root of a drive or share; mirror a folder on it instead.
error-mirror-target-root = { $path } is the root of a drive or share; a mirror needs a folder on it.
error-mirror-target-overlaps = The mirror's target { $target } is the source { $source }, or inside or around it.
error-mirror-duplicate-name = Two sources are called { $name }; a mirror keeps each source in a folder of its name, so they must differ.
# $count of $total files in the target would be deleted; $limit is the job's percentage.
error-mirror-too-many-deletions = Nothing was changed: this run would delete { $count } of the { $total } files in the mirror, more than the job's limit of { $limit }%. If that's intended, raise the job's mirror_delete_limit_percent (100 allows any).
# $detail: the operating system's message, in English.
error-mirror-io = { $path } couldn't be read or made: { $detail }
error-mirror-destination-link = { $path } is a link or junction. A mirror never writes through one, so nothing was changed; remove it, or point the job at another folder.
error-mirror-parent-not-made = its folder in the mirror couldn't be made.
error-mirror-type-conflict = type conflict: resolve by hand. It's a file in one place and a folder in the other, so the mirror left it as it is.

## Running a job: steps, SQLite snapshots, the run lock and the history

# $detail: the operating system's message, in English.
error-step-spawn = The step "{ $step }" couldn't be started: { $detail }
# $code: the command's exit code; $output: its last lines of output.
error-step-exit-code = The step "{ $step }" failed with exit code { $code }. { $output }
# $detail: SQLite's message, in English.
error-sqlite-snapshot = The database { $db } couldn't be copied: { $detail }
error-sqlite-missing = it doesn't exist.
error-sqlite-no-file-name = it isn't a path to a file.
error-sqlite-busy = it stayed locked by another program for two minutes.
error-history = The run history couldn't be read or written: { $detail }
error-run-lock = The job's run lock couldn't be taken: { $detail }
# $detail: the operating system's message, in English.
error-repo-lock = The repository's lock couldn't be taken: { $detail }
# $minutes: how long it waited.
error-wait-timed-out = Waited { $minutes } min for { $job } to finish with { $target }, so this run didn't start.
error-wait-timed-out-unknown = Waited { $minutes } min for another job to finish with { $target }, so this run didn't start.
# $minutes: how long it retried; $detail: restic's message about who holds the lock, in English.
error-restic-locked-too-long = The repository stayed locked by another restic for { $minutes } min, so this was given up: { $detail }
error-run-no-restic = restic wasn't found, so versioned targets can't run.
# $count: how many errors restic check found.
error-verify-errors = Verifying the repository found { $count } errors.
# $count: how many files couldn't be backed up or copied.
error-run-unreadable = { $count ->
    [one] 1 file couldn't be backed up.
   *[other] { $count } files couldn't be backed up.
}
warning-step-output-open = (Stopped reading the step's output at its timeout: a program it started still holds it open.)
error-snapshot-copy-over-db = The copy { $copy } would be written over the live database { $db } or one of its files, which is inside Replica's staging folder. Move the database out of it.
# $detail: the operating system's message, in English.
error-snapshot-unresolvable = Where { $path } leads couldn't be worked out, so the database copies can't be checked: { $detail }
error-snapshot-copy-excluded = The copy { $copy } is inside the source { $source }, whose exclusions leave it out, so it would never be backed up. Change the exclusions so they don't match Replica's staging folder.
# $detail: why the other processes couldn't be stopped, in English.
error-step-not-stopped = The step "{ $step }" was stopped, but the programs it started couldn't all be: they may still be running. ({ $detail })
# $detail: the operating system's message, in English.
error-snapshot-tidy = Replica's staging folder couldn't be tidied, so nothing ran: { $detail }
error-snapshot-db-in-staging = The database { $db } is inside Replica's staging folder, which Replica tidies, so it could be removed. Move it out of there.
