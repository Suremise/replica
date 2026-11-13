# The tray icon: its tooltip for each state and its right-click menu.

## Tooltip

tray-tooltip-idle = Replica
tray-tooltip-running = Replica: running { $job } ({ $percent }%)
tray-tooltip-failed = Replica: the last run of { $job } failed
tray-tooltip-overdue =
    { $count ->
        [one] Replica: 1 job is overdue
       *[other] Replica: { $count } jobs are overdue
    }
tray-tooltip-paused = Replica: schedules are paused
# While a job waits its turn for a backup repository another job is using. $wait: the
# wait-turn text from common.ftl ("Waiting for hive to finish with External HDD").
tray-tooltip-waiting = Replica: { $job }, { $wait }
# While a job runs and others wait their turn.
tray-tooltip-queued =
    { $count ->
        [one] , 1 more waiting
       *[other] , { $count } more waiting
    }

## Menu

tray-run-all = Run all now
tray-run-job = Run
tray-run-no-jobs = No jobs yet
tray-stop = Stop { $job }
tray-open = Open Replica…
tray-pause-schedules = Pause schedules
tray-start-with-windows = Start with Windows
tray-check-for-updates = Check for updates
tray-quit = Quit

## Notices from the tray itself

tray-window-error-title = Replica's window can't open
tray-settings-error-title = Replica's settings can't be read
tray-settings-warning-title = Replica's settings have a problem
tray-folder-title = Replica can't save anything
tray-schedule-error-title = Replica couldn't update its scheduled tasks
tray-autostart-error-title = Replica couldn't change Start with Windows

## The update check: it asks GitHub for Replica's latest release (when the tray starts and
## once a day, only while "Check for updates" is on, or when the user checks by hand) and
## never downloads anything. $version: a version ("0.4.0").

update-available-title = Replica { $version } is available
update-available-body = Click here to see what's new on GitHub and download it.
# Why a check didn't work, after about-update-failed's "couldn't check". $status: an HTTP
# status code ("404").
update-failed-status = GitHub answered { $status } instead of the latest release.
# $version: this version ("0.3.0").
update-failed-version = This version ({ $version }) can't be compared with GitHub's.
update-failed-stopped = The check stopped before it finished.
# GitHub redirected somewhere that isn't a release of Replica (a sign-in page, say).
update-failed-answer = GitHub's answer didn't name a release of Replica.
