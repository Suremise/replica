# Replica's window frame: the menu bar, the status bar and the messages they give.

## The menu bar

menu-bar = Menu bar

menu-file = File
menu-new-job = New job…
menu-import-jobs = Import jobs…
menu-export-jobs = Export jobs…
menu-open-app-folder = Open settings folder
menu-open-log-folder = Open log folder
menu-close-window = Close window
menu-exit = Exit Replica

menu-jobs = Jobs
menu-run-all = Run all
menu-stop = Stop running jobs

menu-view = View
menu-view-jobs = Jobs
menu-view-history = History
menu-view-restore = Restore
menu-status-bar = Status bar

menu-tools = Tools
menu-settings = Settings…
menu-scheduled-tasks = Scheduled tasks…
menu-task-scheduler = Windows Task Scheduler

menu-help = Help
menu-project-page = Replica on GitHub
menu-about = About Replica

# A menu command that couldn't be done: opening a folder, a page or Task Scheduler.
menu-failed-title = That didn't work
menu-failed = { $error }

## The status bar

status-bar = Status bar
status-ready = Ready
status-running = Running { $job }: { $percent }%
status-queued = { $count ->
    [one] 1 more queued
   *[other] { $count } more queued
}
status-next = Next: { $job }, { $when }
status-next-due = Next: { $job }, due now
status-paused = Schedules paused
status-failed = { $count } failed
status-partly = { $count } partly done
status-skipped = { $count } skipped
status-overdue = { $count } overdue
status-problems-hint = Show them in History
status-scheduler-timer = Built-in timer
status-scheduler-tasks = Task Scheduler
status-portable = Portable
