# Replica's Windows Task Scheduler tasks: the optional way to run schedules (off by default).
# Shown by `replica tasks`, the window's Scheduled tasks view, and in Task Scheduler itself.

# A task's description in Task Scheduler. $schedule is its schedule as settings.toml writes it.
tasks-description = Runs the Replica job "{ $job }" ({ $schedule }). Manage it in Replica's Settings, or with replica tasks.

## replica tasks list

cli-tasks-none = Replica has no scheduled tasks.
cli-tasks-task = Task
cli-tasks-job = Job
cli-tasks-schedule = Schedule
cli-tasks-next-run = Next run
cli-tasks-last-run = Last run
cli-tasks-last-result = Last result
cli-tasks-enabled = On
cli-tasks-yes = yes
cli-tasks-no = no
cli-tasks-never = never
cli-tasks-orphan = { $job } (gone: this task's job no longer exists)
cli-tasks-running = running now

## replica tasks enable, disable and remove

cli-tasks-enabled-one = Turned on the task { $task }.
cli-tasks-disabled-one = Turned off the task { $task }.
cli-tasks-removed-one = Removed the task { $task }.
cli-tasks-removed-all =
    { $count ->
        [0] Replica had no scheduled tasks to remove.
        [one] Removed Replica's 1 scheduled task.
       *[other] Removed Replica's { $count } scheduled tasks.
    }
cli-tasks-name-or-all = Give a task's name, or --all.
cli-help-tasks = Lists and manages Replica's Windows Task Scheduler tasks (used when schedules run through Task Scheduler).
cli-help-tasks-list = Lists Replica's tasks: their job, schedule, next and last run, and whether each is on.
cli-help-tasks-enable = Turns a task on.
cli-help-tasks-disable = Turns a task off, without removing it.
cli-help-tasks-remove = Removes a task, or all of them with --all.
cli-help-arg-task = The task's name (its job's name).
cli-help-arg-all-tasks = Every one of Replica's tasks.

## Errors

error-tasks-failed = Windows Task Scheduler couldn't do that: { $detail }
error-task-not-found = Replica has no scheduled task called { $task }.
