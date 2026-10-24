# Replica's window: tabs, labels, buttons, the mirror warning and the repository-password prompt.

## The window itself

window-read-only = Replica's folder can't be written to ({ $path }), so changes can't be saved.
window-settings-error =
    The settings can't be read: { $error }
    Fix { $path } by hand. Nothing is changed here until then.

## Closing or minimising sends Replica to the tray; this says so, the first time

tray-notice-title = Replica is still running
tray-notice-body =
    Closing or minimising the window keeps Replica in the notification area, so your backups
    still run on schedule. To open it again, click its icon there. To stop it, choose Quit from
    the icon's menu.
tray-notice-dont-show = Don't show this again

## Tabs

tab-jobs = Jobs
tab-history = History
tab-restore = Restore
tab-settings = Settings
tab-about = About

## The sidebar and each page's header

# What a screen reader calls the sidebar's list of pages.
nav-pages = Pages
page-jobs-subtitle = What Replica backs up, where to, and when
page-history-subtitle = Every run and test restore, newest first
page-restore-subtitle = Get files back from a backup
page-settings-subtitle = How Replica runs, and where it keeps its files
page-about-subtitle = The version, the licence and the third-party notices

## Words the window's components show themselves

widget-copy-path = Copy the full path
# A table's sortable column header, for a screen reader.
widget-sort-by = Sort by { $column }
widget-time-now = just now
widget-time-in-minutes = in { $n } min
widget-time-ago-minutes = { $n } min ago
widget-time-in-hours = in { $n } h
widget-time-ago-hours = { $n } h ago
# Days only from 2 on: under 48 hours is in hours.
widget-time-in-days = in { $n } days
widget-time-ago-days = { $n } days ago
# A dropdown that carries its own label, closed: "Job: All jobs".
widget-dropdown-labelled = { $label }: { $value }

## Jobs

jobs-new = New job
jobs-edit = Edit
jobs-delete = Delete
jobs-run-now = Run now
jobs-enabled = On
jobs-name = Name
jobs-sources = Sources
jobs-targets = Targets
jobs-mode = Mode
jobs-excludes = Exclusions
jobs-before-steps = Before steps
jobs-after-steps = After steps
jobs-schedule = Schedule
jobs-retention = Retention
jobs-notify-success = Notify me when it succeeds
jobs-run-all = Run all
jobs-table = Jobs
jobs-col-job = Job
jobs-col-on = On
jobs-col-status = Status
jobs-col-last = Last run
jobs-col-next = Next run
jobs-col-targets = Targets
jobs-col-actions = Actions
jobs-empty-title = No backup jobs yet
jobs-empty-body = A job says which folders to back up, where to, and when.
jobs-stop = Stop
jobs-more = More actions
jobs-targets-none = No targets
jobs-never-run-hint = It hasn't run yet.
# Names for screen readers, where a row's control has no visible text of its own.
jobs-enabled-for = { $job } is on
jobs-run-now-for = Run { $job } now
jobs-edit-for = Edit { $job }
jobs-delete-for = Delete { $job }
jobs-more-for = More actions for { $job }
jobs-stop-for = Stop { $job }
jobs-schedule-none = Not scheduled
jobs-schedule-timer = { $schedule } (built-in timer)
jobs-schedule-tasks = { $schedule } (Task Scheduler)
jobs-next-manual = Only when you run it
jobs-next-off = Turned off
jobs-next-paused = Schedules are paused
jobs-next-due = Due now
jobs-running = Running, { $percent }%
jobs-queued = Waiting its turn
# The hover text of a "Versioned · Forever" tag.
jobs-keep-forever-hint = Keeps every backup, forever: nothing is ever deleted, so the backup grows over time.
# A scheduled job whose last good backup is older than its schedule allows.
jobs-overdue = Overdue
# The Overdue chip's hover text. $due and $since: dates and times.
jobs-overdue-hint = Overdue since { $due }. Its last good backup was { $since }.
jobs-overdue-hint-never = Overdue since { $due }. It hasn't been backed up yet.
# $result: Failed, Partly done…; $reason: why, or nothing.
jobs-overdue-last = Last run: { $result }. { $reason }
jobs-last = { $result }, { $when }
# When a job next runs, beyond 12 hours ahead (nearer, it's "in 9 h"); $time is 24-hour.
jobs-next-today = Today { $time }
jobs-next-tomorrow = Tomorrow { $time }
# $day is a short day name (day-mon…).
jobs-next-on = { $day } { $time }
jobs-saved = Saved { $job }.
jobs-deleted = Deleted { $job }.
jobs-delete-title = Delete { $job }?
jobs-delete-body =
    The job is removed from Replica. The backups it already made stay where they are, and so do
    their passwords in Credential Manager.
jobs-delete-task = Its task in Task Scheduler is deleted too.
jobs-delete-confirm = Delete the job
jobs-delete-task-failed = The job is deleted, but its task in Task Scheduler couldn't be: { $error }

## Exporting and importing jobs

jobs-export = Export…
jobs-import = Import…
jobs-export-for = Export { $job }…
# The file type in the Save and Open dialogs.
jobs-file-filter = Replica jobs
export-title = Export jobs
export-intro = Choose the jobs to export. Import the file on another PC, or keep it as a copy of your jobs.
export-filter = Filter the jobs
export-filter-hint = Filter by name or target
export-select-all = Select all
export-select-none = Select none
export-count = { $selected } of { $total } selected
export-none-match = No job matches "{ $filter }".
# A job's check box, for a screen reader: its name, mode and targets.
export-job-for = { $job }, { $mode }, to { $targets }
export-no-passwords = Passwords are never exported.
export-confirm = Export { $count ->
    [one] 1 job
   *[other] { $count } jobs
}
export-done = Exported { $count ->
    [one] 1 job
   *[other] { $count } jobs
} to { $path }.
import-title = Import jobs
import-intro = { $path } has { $count ->
    [one] 1 job
   *[other] { $count } jobs
}. Choose the ones to import.
import-empty = It has no jobs.
# $path: the file; $reason: why, such as "This jobs file was made by a newer Replica".
import-refused = { $path } can't be imported, and nothing was: { $reason }
import-job-for = Import { $job }
import-status-new = New
import-status-exists = Name exists
import-status-needs-password = Needs a password
import-status-drive-not-found = Drive not found
import-status-invalid = Invalid
import-exists-hint = A job here is already called { $existing }.
import-needs-password-hint = No repository password is saved on this PC for { $targets }. Once it's imported, enter the one you saved with "Enter a saved password…" in its editor.
import-drive-hint = The drive for { $targets } isn't connected now. The job shows it as not connected until it is.
import-conflict-for = What to do with { $job }, whose name is taken
import-skip = Skip
import-copy = Import as a copy
import-replace = Replace
import-as-copy = Imports as { $name }.
import-replaces = Replaces { $name } here, which keeps its history.
# $cmd: a command one of the job's steps runs.
import-runs = Runs: { $cmd }
import-note = Imported jobs arrive disabled. Enable them on the Jobs page when you're ready.
import-confirm = Import { $count ->
    [one] 1 job
   *[other] { $count } jobs
}
import-done = Imported { $count ->
    [one] 1 job
   *[other] { $count } jobs
}{ $skipped ->
    [0] {""}
    [one] , skipped 1
   *[other] , skipped { $skipped }
}. Imported jobs are disabled: enable each on the Jobs page when you're ready.
import-failed = Nothing was imported: { $error }

## The job editor

editor-new-title = New job
editor-edit-title = Edit { $job }
editor-name-hint = Letters, digits, ".", "_" and "-", starting with a letter or digit. It's how the command line names the job.
editor-enabled = On: runs on its schedule and with "Run all"
editor-mode-versioned-help = keeps older versions you can go back to
editor-mode-mirror-help = keeps an exact copy, and deletions are copied too
editor-source-path = Folder or file
editor-excludes-help = Leave out (one pattern a line, such as node_modules or *.tmp)
editor-add-folder = Add a folder…
editor-add-file = Add a file…
editor-remove = Remove
editor-remove-for = Remove { $item }
editor-add-drive = Add a drive
editor-add-share = Add a network share
editor-target-name = Name (optional)
editor-drive = Drive
editor-drive-pick = Choose a drive
editor-drive-refresh = Look for drives again
editor-drive-space = { $free } free of { $total }
editor-drive-not-connected = { $label } (not connected)
editor-drive-folder = Folder on the drive
editor-share-path = Share and folder (\\server\share\folder)
editor-share-user = User name
editor-share-password = Password
editor-share-login-saved = A login for this share is saved in Credential Manager.
editor-share-login-remove = Don't use the saved login
editor-schedule-hint = When it runs on its own. Leave it empty to run it only by hand. For example: daily 02:30, weekly mon,thu 21:00, every 6h
editor-retention-daily = Daily
editor-retention-weekly = Weekly
editor-retention-monthly = Monthly
editor-mirror-limit-title = Deletion limit
editor-delete-limit = Stop, deleting nothing, if a run would delete more than this share of the mirror's files
editor-steps-add-sqlite = Add a SQLite snapshot…
editor-steps-add-command = Add a command
editor-steps-add-verify = Add a verify step
editor-step-sqlite = SQLite database
editor-step-command = Command
editor-step-timeout = Time limit (seconds)
editor-step-verify = Check the backup can be read (restic check)
editor-step-verify-schedule = How often (empty: after every backup)
editor-step-verify-subset = How much to read back
editor-not-saved = Not saved.
editor-section-general = General
# What a screen reader calls the "← Jobs" link above the editor.
editor-back = Back to Jobs
editor-discard-title = Discard changes?
editor-discard-body = The changes you made to this job aren't saved.
editor-discard-confirm = Discard changes
editor-discard-keep = Keep editing
editor-section-sources = Sources
editor-section-sources-desc = The folders and files it backs up, each with what to leave out
editor-section-targets = Targets
editor-section-targets-desc = Where the backups go: a drive, or a folder on a network share
editor-section-steps = Before and after steps
editor-section-steps-desc = Run before the backup starts, or after it ends, in this order
editor-section-schedule = Schedule and retention
editor-section-schedule-desc = When it runs on its own, and what it keeps
editor-on = On
editor-on-desc = Runs on its schedule and with "Run all"
editor-notify-desc = A failure always notifies you
editor-mirror-bar-title = Deletions are copied too
editor-mirror-bar = A file you delete from a source is deleted from the copy at the next run, so a mistake isn't kept. The deletion limit below stops a run that would delete a lot.
editor-excludes = Leave out
editor-excludes-none = Nothing is left out.
editor-exclude-new = A pattern to leave out, such as node_modules or *.tmp
editor-exclude-add = Add
editor-sources-none = No sources yet: add a folder or a file.
editor-targets-none = No targets yet: add a drive or a network share.
editor-target-drive = Drive
editor-target-share = Network share
editor-share-login = Login
editor-share-login-none = None: your Windows login is used
editor-share-login-new = A new login, saved in Credential Manager when you save
editor-share-login-replacing = A new login for this share, in place of the one saved
editor-share-login-saved-chip = Saved
# A saved login, with its user name (never its password).
editor-share-login-saved-as = Saved · { $user }
editor-share-login-change = Change…
editor-share-login-remove-saved = Remove
editor-share-login-save = Save login
# What a screen reader calls Cancel while a saved login is being changed.
editor-share-login-keep = Keep the saved login
editor-key-status = Repository password
editor-key-saved = Saved
editor-key-missing = Missing
editor-key-missing-hint = Replica can't open this backup until its password is saved on this PC: enter the copy you saved.
editor-key-new = Made when you save
editor-steps-before = Before the backup
editor-steps-after = After the backup
editor-steps-none = None
editor-steps-add = Add a step
# What a screen reader calls each list's "Add a step".
editor-steps-add-to = Add a step: { $list }
editor-step-sqlite-title = SQLite snapshot
editor-step-command-title = Command
editor-schedule-runs = Runs
editor-schedule-manual = Only when I run it
editor-schedule-daily = Every day
editor-schedule-weekly = On chosen days
editor-schedule-every = At an interval
editor-schedule-custom = As typed
editor-schedule-at = At (24-hour, such as 02:30)
editor-schedule-days = On
editor-schedule-interval = Every
editor-schedule-hours = hours
editor-schedule-minutes = minutes
# What a screen reader calls the hours-or-minutes choice.
editor-schedule-unit = Hours or minutes
editor-schedule-text = Schedule
editor-retention = Keep
# The Keep choice: the newest backups by the counts below, or every backup, forever.
editor-keep-newest = Newest backups
editor-keep-forever = Everything, forever
editor-keep-newest-help = Older backups are deleted as newer ones are made
editor-keep-forever-help = Nothing is ever deleted; the backup grows over time.
editor-retention-counts = How many
editor-retention-desc = The newest backups of each kind to keep
editor-problems =
    { $count ->
        [one] Not saved: there's one thing to fix, marked above.
       *[other] Not saved: there are { $count } things to fix, marked above.
    }
day-mon = Mon
day-tue = Tue
day-wed = Wed
day-thu = Thu
day-fri = Fri
day-sat = Sat
day-sun = Sun

## Enter a saved password: a versioned target's repository password, from the copy the user kept

editor-key-enter = Enter a saved password…
editor-key-enter-for = Enter a saved password for { $target }
editor-key-enter-hint = Put back the repository password you saved for this target: on a new PC, say, or if Replica says it's missing or wrong.
key-entry-title = Enter a saved password
key-entry-body = Type the repository password you saved for { $target }. Replica checks that it opens the backup there before it saves it in Windows Credential Manager.
key-entry-password = Password
key-entry-save = Check and save
key-entry-replace = Replace it anyway
key-entry-saved-works = The password saved for { $target } opens the backup there already. To save this one in its place, it has to open the backup too: choose "Replace it anyway".
key-entry-checking = Checking it opens the backup at { $target }…
key-entry-saved = Saved the repository password for { $target }: it opens the backup there.
key-entry-replaced = Saved the repository password for { $target } in place of the one saved before: it opens the backup there.
key-entry-unchanged = That's the password saved for { $target } already: nothing changed.

## Setting up a versioned target, when the job is saved

editor-setup = Set up now
editor-setup-checking = Checking { $target }…
editor-setup-initialising = Setting up { $target }…
editor-setup-done = { $target } is set up.
editor-setup-exists = { $target } already has a backup there.
editor-setup-no-password = { $target } already has a backup there, but its password isn't saved on this PC, so Replica can't use it yet. If you have the copy you saved, put it back with "Enter a saved password…" on the target.
editor-setup-not-available = { $target } isn't connected ({ $reason }). Connect it and save again, or save now and set it up later with "Set up now".
editor-setup-save-anyway = Save without setting it up
editor-setup-failed = { $target } couldn't be set up: { $error }
editor-setup-stopped = it stopped before it finished
editor-setup-password-missing = its password was no longer in Credential Manager

## The window failing to open (shown by the tray)

window-no-font = No text font was found in { $dir } (looked for { $files }).
window-start-failed = { $error }
window-stopped = The window stopped because of a fault ({ $error }). Quit Replica from the tray and start it again; the log has the details.
window-gone = The window has closed and can't open again. Quit Replica from the tray and start it again.
window-not-responding = The window isn't responding, so it can't come forward. If it doesn't come back soon, quit Replica from the tray and start it again; the log has the details.
window-not-started = The window hasn't opened. Quit Replica from the tray and start it again; the log has the details.

## Sizes (a drive's free and total space)

size-b = { $value } B
size-kb = { $value } KB
size-mb = { $value } MB
size-gb = { $value } GB
size-tb = { $value } TB

## Settings

# The tab list under the page's header, and its tabs.
settings-tabs = Settings sections
settings-tab-schedules = Schedules
settings-tab-general = General
settings-tab-restic = restic
settings-tab-storage = Storage
settings-mode = What runs the schedules
settings-mode-timer = Built-in timer
settings-mode-timer-help = Jobs run on their schedules while Replica's tray app runs.
settings-mode-tasks = Task Scheduler
settings-mode-tasks-help = Windows Task Scheduler runs the jobs on their schedules, even when the tray app isn't running.
settings-mode-help =
    Task Scheduler runs the jobs only while you're logged on to Windows. When it's off, the
    built-in timer takes over. The two never both run a job.
settings-pause-help = No job runs on its schedule until you turn this off. Run now still works.
settings-start-with-windows-help = Starts Replica's tray app when you sign in to Windows.
# Its accessible name: the unit is shown beside the number (settings-days-unit).
settings-log-days = Keep run logs for (days)
settings-log-days-label = Keep run logs for
settings-log-days-help = Older run logs are deleted. The run history keeps every run.
settings-days-unit = days
settings-restic-in-use-bundled = In use: the restic that comes with Replica
settings-restic-in-use-own = In use: your own restic
settings-restic-finding = Finding the restic in use…
settings-restic-version = restic { $version }
settings-restic-file = File
settings-restic-path = restic.exe
settings-restic-path-hint = Leave empty to use the restic that comes with Replica.
settings-restic-bundled = Use the one that comes with Replica
settings-open-folder = Open Replica's folder
settings-open-logs = Open the logs folder
settings-reset-title = Reset
settings-reset-help = Puts Replica's own settings back to their defaults. Your jobs, their backups and their passwords are never deleted.
settings-reset = Reset settings
settings-reset-confirm-title = Reset Replica's settings?
settings-reset-body =
    Schedules, Start with Windows, the restic setting and log retention go back to their
    defaults, and Task Scheduler is turned off with its tasks deleted. Your jobs stay as they
    are, and so do their backups.
settings-reset-done = The settings are back to their defaults. Your jobs are unchanged.

## The Scheduled-tasks view (in Settings)

tasks-title = Scheduled tasks
tasks-help =
    These are Replica's tasks in Windows Task Scheduler, in its "Replica" folder. Replica makes
    one for each scheduled job while the Task Scheduler option is on. You can turn each on or
    off, or delete it.
tasks-none-title = No scheduled tasks
tasks-none = Replica has no tasks in Task Scheduler.
tasks-col-task = Task
tasks-col-job = Job
tasks-col-trigger = Schedule
tasks-col-next = Next run
tasks-col-last = Last run
tasks-col-on = On
tasks-col-actions = Actions
# A row of the tasks table, for a screen reader.
tasks-row = The task { $task }
# A task whose job is gone.
tasks-orphan = No such job any more
tasks-orphan-help = A task marked "No such job any more" has lost its job (deleted or renamed), so it has nothing to run. Delete it here, or with Remove all.
tasks-enable-for = Turn on the task { $task }
tasks-disable-for = Turn off the task { $task }
tasks-delete-for = Delete the task { $task }
tasks-delete-title = Delete the task { $task }?
tasks-delete-body = It's removed from Task Scheduler. The job stays; while the Task Scheduler option is on, Replica makes its task again.
tasks-remove-all = Remove all Replica tasks
tasks-remove-all-title = Remove all of Replica's tasks?
tasks-remove-all-body = Every task in Task Scheduler's "Replica" folder is deleted. Your jobs stay; while the Task Scheduler option is on, Replica makes their tasks again.
tasks-removed = { $count ->
    [one] Removed 1 task.
   *[other] Removed { $count } tasks.
}
tasks-open-scheduler = Open Task Scheduler
tasks-result-code = Task Scheduler code { $code }
tasks-turn-off-title = Turn Task Scheduler off?
tasks-turn-off-body =
    The built-in timer runs the schedules from now on, while the tray app runs. Replica's tasks
    in Task Scheduler aren't needed any more: delete them, or keep them turned off.
tasks-turn-off-delete = Delete the tasks
tasks-turn-off-keep = Keep them, turned off

## About

about-version = Version { $version }
about-tagline = Versioned backups and mirrors for Windows.
about-project-hint = Opens Replica's page on GitHub in your browser.
about-licence-title = Licence
about-licence = Replica is free software under the MIT licence.
about-licence-text = The MIT licence
about-licence-online = View it on GitHub
about-notices = Third-party notices
about-notices-help = The licences of everything Replica is built from, and of restic, which it bundles.
about-copy = Copy
about-go-to-restic = Go to restic's section
about-links = Links
about-project = Replica's page
about-notices-missing = This build doesn't carry the crates' notices: they're made with each release (THIRD-PARTY-NOTICES). restic's are below.

## History

history-open-log = Open the log
history-open-log-failed = The log couldn't be opened: { $error }
history-empty-body = Runs show here once a job has run.
history-no-match-title = No runs match these filters
history-no-match-body = Try another job, result or time, or clear the filters to see every run.
history-clear-filters = Clear filters
history-refresh = Look again
history-show-older = Show older
history-filter-job = Job
history-all-jobs = All jobs
history-filter-result = Result
history-all-results = All results
# One run, as it's listed.
history-run-line = { $when }  ·  { $job }  ·  { $result }  ·  { $duration }  ·  { $files ->
    [one] 1 file
   *[other] { $files } files
}, { $size }
# One test restore, as it's listed.
history-test-line = { $when }  ·  { $job }  ·  test restore: { $result }  ·  { $checked } compared, { $differing } different
history-target = { $target } ({ $mode }): { $result }, { $files ->
    [one] 1 file
   *[other] { $files } files
}, { $size }
history-target-why = { $line }: { $reason }
# A partly-done run or target with how many files it couldn't back up or copy: "Partly done · 3 skipped".
history-partly-skipped = { $result } · { $count } skipped
history-test-target = From { $target }, snapshot { $snapshot }
history-empty-title = No runs yet
history-filter-period = When
history-period-any = Any time
history-period-day = The last 24 hours
history-period-week = The last 7 days
history-period-month = The last 30 days
history-search = Search the history
history-search-hint = Search…
history-col-started = Started
history-col-job = Job
history-col-target = Target
history-col-result = Result
history-col-duration = Took
history-col-files = Files
history-col-size = Size
# Marks a test restore among the runs.
history-test-restore = Test restore
# The chosen run's details: when first, so a long job's name is what's cut.
history-details-title = { $when }  ·  { $job }
history-copy-details = Copy details
history-target-name = { $target } ({ $mode })
history-target-amount = { $files ->
    [one] 1 file
   *[other] { $files } files
}, { $size }
history-summary-runs = { $count ->
    [one] 1 run
   *[other] { $count } runs
}
history-summary-tests = { $count ->
    [one] 1 test restore
   *[other] { $count } test restores
}
history-summary-ok = { $count } OK
history-summary-partly = { $count } partly done
history-summary-failed = { $count } failed
history-summary-skipped = { $count } skipped

## Test restores (the Restore tab and History)

test-restore-same = { $checked ->
    [one] The file compared is the same as its source.
   *[other] All { $checked } files compared are the same as their sources.
}
test-restore-nothing-compared = Nothing could be compared: the files picked have changed or gone since the backup. Try again later.
test-restore-differs = { $differing ->
    [one] A restored file differs from its source, though the source hasn't changed since the backup:
   *[other] { $differing } restored files differ from their sources, though the sources haven't changed since the backup:
}

## Restore

restore-job = Job
restore-pick-job = Choose a job
restore-target = Backup
restore-test = Test restore
restore-test-hint = Restores a few random files from the latest snapshot to a temporary folder and checks they're the same as the originals.
restore-test-running = Test restore running…
restore-mirror = A mirror is a plain copy of the files: open its folder to copy files back.
restore-loading-snapshots = Looking for snapshots…
restore-no-snapshots = No snapshots of this job here yet.
restore-target-not-available = { $target } isn't available: { $reason }
restore-stopped = It stopped before it finished.
restore-snapshot = Snapshot
restore-snapshot-line = { $when }  ·  { $host }  ·  { $tags }  ·  { $size }  ·  { $id }
restore-loading-folder = Reading the folder…
restore-empty-folder = This folder is empty.
restore-whole-folder = All of { $name }
restore-file-line = { $name } ({ $size })
restore-to-folder = To a new folder
restore-folder = Folder
restore-to-original = Where they were, replacing what's there now
restore-to-original-help = The files there now are replaced. You're asked first.
# The three steps, each a card.
restore-step-choose = 1. Choose a backup
restore-step-choose-help = A job, its backup if it has more than one, and a snapshot: the backup as it was at one time.
restore-no-snapshots-title = No snapshots yet
restore-no-jobs-title = Nothing to restore yet
restore-no-jobs-body = Restore gets files back from a job's backups. Make a job on the Jobs page first.
restore-col-date = Date
restore-col-host = PC
restore-col-tags = Tags
restore-col-size = Size
restore-col-id = ID
# Pinning a snapshot: kept whatever the job's retention.
restore-col-pinned = Pinned
restore-pinned = Pinned
restore-pinned-hint = Kept whatever the job's retention: only unpinning it lets the retention delete it.
restore-pin = Pin this backup
restore-pin-hint = Keep this backup whatever the job's retention, until you unpin it.
restore-unpin = Unpin…
restore-pinning = Pinning…
restore-unpinning = Unpinning…
restore-pinned-done = Pinned: it's kept whatever the job's retention.
restore-unpinned-done = Unpinned.
restore-unpin-title = Unpin this backup?
restore-unpin-body = This backup can then be deleted by the job's retention, along with the files only it holds.
restore-unpin-confirm = Unpin
restore-step-files = 2. Choose files and folders
# Where the files are from: job › snapshot.
restore-breadcrumb = { $job }  ›  { $when }  ›  { $host }
restore-chosen = { $count ->
    [0] Nothing chosen yet. Open a folder to see what's in it.
    [one] 1 item chosen
   *[other] { $count } items chosen
}
restore-step-to = 3. Restore to
restore-test-title = Check this backup
restore-test-last = Last test restore
restore-test-never = Never run
restore-default-folder = Restored by Replica
restore-start = { $count ->
    [0] Restore
    [one] Restore 1 item
   *[other] Restore { $count } items
}
restore-running = Restoring…
# restic counts the folders it restores as well as the files.
restore-done = { $files ->
    [one] Restored 1 file or folder ({ $size }).
   *[other] Restored { $files } files and folders ({ $size }).
}
restore-open-folder = Open the folder
restore-original-title = Replace the files where they were?
restore-original-body =
    The chosen files and folders are put back where they were backed up from. Files there now
    with the same names are replaced by the snapshot's copies, and can't be got back unless
    another backup has them.
restore-original-confirm = Replace them

## Buttons used across the window

button-save = Save
button-cancel = Cancel
button-close = Close
button-browse = Browse…
button-ok = OK

## The mirror warning, shown when a target's mode is set to mirror

mirror-warning-title = A mirror copies deletions too
mirror-warning-body =
    A mirror keeps an exact copy of the sources. When a file is deleted or damaged in a source,
    the next run deletes or damages it in the mirror too, and there's no older version to go back to.
    A versioned target keeps those older versions.
mirror-warning-confirm = Use a mirror anyway
mirror-warning-keep-versioned = Keep it versioned

## The repository password: shown once by `replica init` and the window, again by `replica key show`

password-save-title = Save a copy of this password
password-save-body =
    The backup on { $target } is encrypted with this password. Replica keeps it in Windows
    Credential Manager, but that copy is lost with this PC, and without the password the backup
    can't be read by anyone. Save a copy now, in a password manager or on paper.
password-save-confirm = I've saved a copy
password-save-copy = Copy password
password-save-step-save = Save your password
password-save-step-create = Create the repository
password-save-cli-confirm = Press Enter once you've saved a copy.

## Connected drives: picking a drive target (the window and replica init)

volume-label = Label
volume-letter = Drive
volume-size = Size
volume-free = Free
volume-removable = Removable
volume-id = Volume ID
volume-no-label = (no label)
volume-no-letter = (no drive letter)
