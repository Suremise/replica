# Windows notifications. A failure always notifies; a success only when the job asks for it.

notify-run-ok-title = { $job } is backed up
notify-run-ok-body =
    { $count ->
        [one] 1 target is up to date.
       *[other] { $count } targets are up to date.
    }
notify-run-failed-title = { $job } failed
notify-run-failed-body = { $target }: { $reason }
notify-run-partly-title = { $job } was only partly backed up
notify-run-partly-body = { $ok } of { $total } targets are up to date. Open Replica for the details.
notify-overdue-title = { $job } is overdue
notify-overdue-body = Its last good backup was { $since }. { $reason }
# The $reason of notify-overdue-body. $due: a date and time.
notify-overdue-reason = It was due { $due }.
notify-overdue-never = It hasn't been backed up yet. It was due { $due }.
# A failed run with no single target to blame (a before step, the lock): its reason alone.
notify-run-failed-reason = { $reason }
notify-open = Open Replica
