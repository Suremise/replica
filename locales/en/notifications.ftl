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
# A mirror left items that are a file in one place and a folder in the other. $count: how many.
# The first one follows on a line of its own when there's room: a notification holds 255
# characters, so keep this short.
notify-run-clashes = { $count ->
    [one] 1 item is a file in one place and a folder in the other, so it was left as it is. History says what to do.
   *[other] { $count } items are a file in one place and a folder in the other, so they were left as they are. History lists them all, with what to do.
}
notify-overdue-title = { $job } is overdue
notify-overdue-body = Its last good backup was { $since }. { $reason }
# The $reason of notify-overdue-body. $due: a date and time.
notify-overdue-reason = It was due { $due }.
notify-overdue-never = It hasn't been backed up yet. It was due { $due }.
# A failed run with no single target to blame (a before step, the lock): its reason alone.
notify-run-failed-reason = { $reason }
notify-open = Open Replica
