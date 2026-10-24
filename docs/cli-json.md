# `replica --json`: the schema

`replica run`, `list`, `status` and `snapshots` take `--json` and then print one JSON document to
stdout, for scripts and other programs. This page is the contract.

- **Keys are never translated.** Values that are messages (`reason`, `error`) are in the user's
  language; values from a fixed set (`outcome`, `mode`) are the English words listed here.
- **`"schema": 1`** is in every document. A change that could break a reader (a key removed,
  renamed or retyped, a value's meaning changed) raises it; new keys can appear in any version, so
  ignore keys you don't know.
- **Times** are RFC 3339 with a UTC offset (`2026-10-21T19:07:30.596+01:00`). **Sizes** are bytes.
- **The exit code** is the same with and without `--json`. Messages that aren't the document (such
  as a settings warning) go to stderr.

## Outcomes and exit codes

| `outcome` | `exit_code` | Meaning |
|---|---|---|
| `"ok"` | 0 | Done. |
| `"failed"` | 1 | Not done, because of an error. |
| `"partly"` | 2 | Done, but some files couldn't be backed up or copied, or a later step failed. |
| `"skipped"` | 3 | Not done, and not an error: a target isn't available, or the job is already running. |
| | 4 | A usage or settings error: no JSON is printed, and the message goes to stderr. |

A run's outcome is the worst of its targets' and steps' (failed, then partly, then skipped, then
ok), and `run --all`'s is the worst of its jobs'.

## `replica run <job>|--all --json`

```json
{
  "schema": 1,
  "outcome": "ok",
  "exit_code": 0,
  "jobs": [
    {
      "job": "projects",
      "run_id": 42,
      "outcome": "ok",
      "exit_code": 0,
      "started_at": "2026-10-21T19:07:30.596+00:00",
      "duration_ms": 8123,
      "files": 12,
      "bytes": 104857,
      "error": null,
      "log": "C:\\Users\\me\\AppData\\Roaming\\Replica\\logs\\runs\\projects-20261021-190730-596-1234.log",
      "waits": [
        {
          "kind": "turn",
          "target": "External HDD",
          "job": "docs",
          "duration_ms": 41250,
          "ended": "ok"
        }
      ],
      "targets": [
        {
          "target": "External HDD",
          "mode": "versioned",
          "outcome": "ok",
          "reason": null,
          "files": 12,
          "bytes": 104857,
          "snapshot_id": "fa1b05d7c441ba5cfd46f69b4852cf305d41904998ef042e3c7f985e46909cb7"
        }
      ]
    }
  ]
}
```

- `jobs`: one per job run, in the jobs' order (whatever order they ran in: `--all` starts jobs
  whose repositories are free first); empty when `--all` finds no job that's on.
- `run_id`: the run's ID in Replica's history, or `null` if it couldn't be recorded (or the job was
  already running and so didn't run).
- `error`: what failed for the whole run (a before step, the lock), or `null`.
- `log`: the run's own log file, or `null` if it has none.
- `waits`: what the run waited for before it could go on, in order; empty if it didn't wait.
  `kind` is `"turn"` (another job was using the same repository: `job` names it, or is `null` if
  its name couldn't be read) or `"restic-lock"` (a restic outside Replica held the repository's
  lock; `job` is `null`). `duration_ms` is how long it waited. `ended` is `"ok"` (it went on),
  `"timed-out"`, `"cancelled"`, or `"gave-way"` (with `--all`: it let a job behind it whose
  repository was free go first, and was tried again later). While it waits, the run prints
  "Waiting for docs to finish with External HDD" (to stderr with `--json`). A run that waits
  longer than `repository_wait_minutes` is `"failed"`, with the reason in `error`.
- `targets[].mode`: `"versioned"` or `"mirror"`.
- `targets[].reason`: why the target was skipped or failed, or what went partly wrong; `null` when
  it's ok.
- `targets[].files`, `bytes`: for a versioned target, the new and changed files and the bytes
  added to the repository; for a mirror, the files copied and their bytes.
- `targets[].snapshot_id`: the new snapshot (versioned targets), else `null`.

## `replica list --json` and `replica status --json`

```json
{
  "schema": 1,
  "jobs": [
    {
      "name": "projects",
      "enabled": true,
      "mode": "versioned",
      "schedule": "daily 02:30",
      "targets": ["External HDD", "NAS"],
      "last_run": {
        "run_id": 42,
        "started_at": "2026-10-21T19:07:30.596+00:00",
        "outcome": "ok",
        "exit_code": 0
      },
      "last_success": "2026-10-21T19:07:30.596+00:00",
      "next_run": "2026-10-22T02:30:00+01:00",
      "overdue": false,
      "running": false
    }
  ]
}
```

- `schedule`: the job's schedule as written in settings (`"daily 02:30"`, `"weekly mon,thu 21:00"`,
  `"every 6h"`), or `null` if it runs only by hand.
- `last_run`: the latest run in the history, or `null` if it has never run.
- `last_success`: the start of the latest run in which a target backed up or mirrored (its own
  result ok or partly), or `null`.
- `next_run`: when the schedule next runs it; `null` if it has no schedule, is off, or schedules are
  paused.
- `overdue`: its last success is older than its schedule allows (plus an hour's grace).
- `keep_forever`: whether the job keeps every snapshot, forever (its runs never remove one).
- `running`: **`status` only** (absent from `list`): whether a run of the job is going on now, in
  any process.

## `replica snapshots <job> --json`

```json
{
  "schema": 1,
  "job": "projects",
  "snapshots": [
    {
      "target": "External HDD",
      "id": "fa1b05d7c441ba5cfd46f69b4852cf305d41904998ef042e3c7f985e46909cb7",
      "short_id": "fa1b05d7",
      "time": "2026-10-21T19:07:30.596+01:00",
      "paths": ["D:\\Projects"],
      "hostname": "DESKTOP",
      "pinned": false
    }
  ],
  "unavailable": [
    { "target": "NAS", "outcome": "skipped", "reason": "the target isn't available: …" }
  ]
}
```

- `snapshots`: every snapshot of the job in each of its versioned targets that could be read,
  oldest first within a target.
- `unavailable`: targets that couldn't be read, with `outcome` `"skipped"` (not available) or
  `"failed"`, and why. The exit code is the worst of the targets'.
- A mirror job has no snapshots: `snapshots` on one is a usage error (exit code 4).

## `replica tasks list --json`

Replica's Windows Task Scheduler tasks. They exist only when schedules run
through Task Scheduler, an option that's off by default.

```json
{
  "schema": 1,
  "folder": "\\Replica",
  "tasks": [
    {
      "name": "projects",
      "job": "projects",
      "orphan": false,
      "schedule": "daily 02:30",
      "enabled": true,
      "running": false,
      "next_run": "2026-10-22T02:30:00",
      "last_run": "2026-10-21T02:30:01",
      "last_result": 0
    }
  ]
}
```

- `name`: the task's name in the folder, which is its job's name. `job`: the job it runs.
- `orphan`: the job no longer exists in the settings.
- `schedule`: the schedule as `settings.toml` writes it, as recorded in the task; `null` if the
  task doesn't say.
- `next_run` and `last_run`: local time, `YYYY-MM-DDTHH:MM:SS`, or `null` (none scheduled, or
  never run).
- `last_result`: the last run's exit code (0 ok, 1 failed, 2 partly, 3 skipped, 4 a settings
  problem), or Task Scheduler's own code if it couldn't start it; `null` if it never ran.
- No tasks: `"tasks": []`. The other `replica tasks` commands have no `--json`.
