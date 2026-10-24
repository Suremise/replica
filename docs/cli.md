# The command line

`replica.exe` is Replica's command line: a console program with real output and exit codes, for
scripts, schedules and other tools. Everything it does, the tray app does too; nothing needs the
window.

```
replica run <job> | --all [--quiet] [--json]
replica list [--json]
replica status [--json]
replica snapshots <job> [--json]
replica restore <job> <snapshot> <path> --to <folder> [--target <name>] [--overwrite]
replica verify <job>
replica pin <job> <snapshot> [--target <name>]
replica unpin <job> <snapshot> [--target <name>]
replica init <job>
replica key show <job> [--target <name>]
replica key set <job> [--target <name>] [--force]
replica login set <name> [--user <user>]
replica export [<job>…] [--all] -o <file> [--force]
replica import <file> [--only <job>…] [--on-conflict skip|copy|replace]
replica tasks list [--json]
replica tasks enable <task>
replica tasks disable <task>
replica tasks remove <task> | --all
```

`replica --help` and `replica <command> --help` show the same, in your language. `replica
--version` prints the version.

Jobs are set up in `settings.toml` (see [settings.example.toml](settings.example.toml) for every
key). A job is named on the command line by its `name`, ignoring case.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Ok: done. |
| 1 | Failed: not done, because of an error. |
| 2 | Partly done: some files couldn't be backed up or copied, or a later step failed. |
| 3 | Skipped: not done, and not an error. A target isn't available (the drive isn't plugged in, the NAS is off), or the job is already running. |
| 4 | A usage or settings error: a command or job that doesn't exist, a missing argument, a settings file that can't be read, or Replica's folder can't be written to. |

When a command covers several targets or jobs, its code is the worst of theirs: failed, then
partly, then skipped, then ok. `--help` and `--version` exit with 0.

## Output

- Results go to stdout; warnings and errors go to stderr, so `2>` catches every problem.
- On a terminal, `run` shows a progress line that the next one overwrites. Redirected output has
  no progress lines.
- **`--json`** prints one JSON document to stdout instead of the text, for other programs. Its keys
  are never translated, and the exit code is the same as without it. The schema, with examples,
  is in [cli-json.md](cli-json.md).
- Messages are in the Windows display language when Replica has it, else English (v1 has English
  only). Set **`REPLICA_LANG`** (such as `REPLICA_LANG=en`) to choose another.

## `replica run`

```
replica run <job> [--quiet] [--json]
replica run --all [--quiet] [--json]
```

Runs a job now, or with `--all`, every job that's on (`enabled = true`), one after another. A job
named on the command line runs even if it's off.

A run goes through the job's `before` steps, then each target in turn (a versioned target: a
restic backup, then `forget` with the job's retention, then `prune`; a mirror: the copy), then
its `after` steps. Each run has its own log in `logs\runs\` and is recorded in the history.

| Option | |
|---|---|
| `<job>` | The job's name. |
| `--all` | Every job that's on. Give a job or `--all`, not both. No job that's on is exit code 0. |
| `--quiet` | Print nothing unless something goes wrong. Problems still go to stderr. For other apps' hooks, such as a "run on close" setting: `replica run <job> --quiet`. |
| `--json` | Print the results as JSON ([cli-json.md](cli-json.md#replica-run-job--all---json)). |

A job that's already running (in the tray, a schedule or another `replica run`) isn't run twice:
that run is skipped (3).

## `replica list` and `replica status`

```
replica list [--json]
replica status [--json]
```

`list` shows each job with its last result, when it last ran and when it runs next. The next run
says "off" for a job that's off, notes when the job is overdue (its last success is older than its
schedule allows, plus an hour), and says "not scheduled" for a job with no schedule or while
schedules are paused.

`status` shows the same, and which jobs are running now, in any process.

| Option | |
|---|---|
| `--json` | Print the jobs as JSON ([cli-json.md](cli-json.md#replica-list---json-and-replica-status---json)). |

Exit code 0, or 4 if the settings or the history can't be read.

## `replica snapshots`

```
replica snapshots <job> [--json]
```

Lists a versioned job's snapshots in each of its targets, oldest first: the snapshot's short ID,
its time and its target, and `pinned` for a pinned one ([`replica pin`](#replica-pin-and-replica-unpin)).
Only this job's own snapshots are listed, even in a repository that two jobs share.

| Option | |
|---|---|
| `<job>` | The job's name. It must be a versioned job: a mirror has no snapshots (exit code 4). |
| `--json` | Print the snapshots as JSON ([cli-json.md](cli-json.md#replica-snapshots-job---json)). |

A target that isn't available, or can't be read, is reported on stderr, and the exit code is the
worst of the targets'.

## `replica restore`

```
replica restore <job> <snapshot> <path> --to <folder> [--target <name>] [--overwrite]
```

Restores a file or folder from one of the job's snapshots into a folder.

| Option | |
|---|---|
| `<job>` | The job's name. It must be a versioned job. |
| `<snapshot>` | The snapshot's ID, or the start of it (as `replica snapshots` shows it), or `latest` for the job's newest. It must be one of this job's own snapshots. |
| `<path>` | The file or folder as it was backed up: a Windows path (`D:\Work\report.docx`; a relative path is taken from the current folder), or as restic shows it (`/D/Work/report.docx`). |
| `--to <folder>` | Where to restore it. The folder must be new or empty, and it can't be one of the job's sources, or inside or around one. The item is restored inside it by its own name. |
| `--target <name>` | Only this target, by its name (as `list` and `snapshots` show it). Without it, each target is tried in turn, and the first one that has the snapshot restores it. |
| `--overwrite` | Allow restoring into the original place, or into a folder that has files in it, replacing what's there. |

Before restoring, Replica checks that the item is in that snapshot, so a file already in the
folder can't pass for a restored one.

Exit codes: 0 restored; 1 failed (the snapshot or the item isn't there, the folder isn't allowed,
restic failed); 3 no target was available; 4 a usage error.

## `replica verify`

```
replica verify <job>
```

Checks each of a versioned job's repositories with `restic check`, reading back part of the
stored data: the `read_data_subset` of the job's `verify` step, or 5% if it has none. A check
that passes is recorded in the history, for the job's verify schedule.

Exit codes: 0 every repository is sound; 1 a check found errors or couldn't run; 3 a target
isn't available; 4 a usage error (such as a mirror job).

## `replica pin` and `replica unpin`

```
replica pin <job> <snapshot> [--target <name>]
replica unpin <job> <snapshot> [--target <name>]
```

Pins one of a versioned job's snapshots, so it's kept whatever any job's retention says (restic's
tag `replica:pinned`, which every `forget` Replica runs keeps), or unpins it, so the job's
retention can remove it again. `<snapshot>` and `--target` are as for `replica restore`; the first
target that has the snapshot is used. restic gives the snapshot a new ID when its tags change: the
new short ID is printed. Like a run, it waits its turn while another job is using the repository.

Exit codes: 0 done; 1 failed (no such snapshot, restic failed); 3 no target was available; 4 a
usage error (such as a mirror job).

## `replica init`

```
replica init <job>
```

Sets up a job's targets before its first run:

- **A mirror target:** makes its folder.
- **A versioned target:** makes its folder, a restic repository in it, and a password for it.
  The password is shown once, with a request to save a copy. Type `saved` and press Enter once you
  have, and Replica stores it in Windows Credential Manager and makes the repository. Anything
  else stores nothing and makes nothing (exit code 1): run `replica init` again when you're ready.
  A `HOW-TO-RESTORE.txt` is written beside the repository: how to restore with restic alone,
  without Replica. It holds no secrets.

A target that's already set up is left as it is (exit code 0).

**Keep a copy of the password somewhere other than this PC**, such as a password manager or on
paper. Without it, nobody can read the backup, Replica included. `replica key show` shows it
again.

## `replica key show`

```
replica key show <job> [--target <name>]
```

Prints a versioned target's repository password, from Windows Credential Manager, so you can save
a copy or restore with restic directly. Without `--target`, each of the job's targets is shown.

| Option | |
|---|---|
| `<job>` | The job's name. It must be a versioned job. |
| `--target <name>` | Only this target, by its name. |

Exit codes: 0 shown; 1 a password isn't stored (run `replica init`, or `replica key set` if you
have the copy you saved); 4 a usage error.

## `replica key set`

```
replica key set <job> [--target <name>] [--force]
```

Puts a versioned target's repository password back from the copy you saved: on a new PC, after
reinstalling Windows, or if its entry was removed from Credential Manager. Every run says "There's
no saved repository password" until it's back. With the repository's folder and this password,
the backup works from any PC: runs, snapshots and restores, the older PC's snapshots included.

For each of the job's targets (or the one `--target` names), Replica asks for the password. It
isn't shown as you type, and it's never put on a command line or in a log, so it isn't in your
shell's history either. Piped input (a script) is read as it is, one line per target.

Replica **checks the password opens the repository** (`restic cat config`) before it saves it, under
the same name `replica init` uses. A password that doesn't open it is never saved. A password saved
already is treated like this:

- **It opens the repository:** it's left as it is, and you aren't asked for one (exit code 1). With
  `--force`, the one you type replaces it, but only if it opens the repository too (restic allows
  more than one password for a repository, added with `restic key add`).
- **It doesn't open the repository** (a stale entry): the one you type replaces it.
- **It's the one you type:** nothing changes.

| Option | |
|---|---|
| `<job>` | The job's name. It must be a versioned job. |
| `--target <name>` | Only this target, by its name. |
| `--force` | Replace a saved password even though it opens the repository. |

Exit codes: 0 saved (or saved already); 1 not saved (the password doesn't open the repository,
there's no repository at the target yet, or a working one is saved and `--force` wasn't given); 3
the target isn't available; 4 a usage error. In the window, a versioned target's **Enter a saved
password…** in the job editor does the same.

## `replica login set`

```
replica login set <name> [--user <user>]
```

Saves a share's login in Windows Credential Manager, for a share target that names it in
`settings.toml` (`credential = "<name>"`), instead of using `cmdkey`. Replica asks for the user
name (unless `--user` gives it), then the password twice. Neither password is shown as you type.
A login saved under that name already is replaced. If no share target in the settings names it,
Replica says so, as that's usually a typo. The login is first used by the next run that connects
to the share.

| Option | |
|---|---|
| `<name>` | The login's name, as a share target's `credential` gives it. It can't start with `replica:repo:`, which is where repository passwords are kept. |
| `--user <user>` | The user name, such as `NAS\backup` or `backup`. |

Exit codes: 0 saved; 1 not saved (the two passwords differ, or are empty); 4 a usage error.

Logins the window saves for a share are named after the share (`share:\\nas\backup`), and can be
replaced here under that name too.

## `replica export` and `replica import`

```
replica export [<job>…] [--all] -o <file> [--force]
replica import <file> [--only <job>…] [--on-conflict skip|copy|replace]
```

Jobs to and from a **jobs file**, to set the same jobs up on another PC or keep a copy of them.
It's TOML, the `[[jobs]]` tables of `settings.toml` under a short header (`format =
"replica-jobs"`, `version = 1` and the Replica that wrote it); `.replica-jobs.toml` is the
suggested extension. It holds only jobs, never the app's settings, history, tasks or pins, and
**never a password**: a share target keeps its login's *name* (`credential`), as `settings.toml`
does, and repository passwords aren't in jobs at all.

`export` writes the jobs named (ignoring case), or every job with `--all`, and won't replace a file
that's there unless `--force`.

`import` prints what it does with each job in the file, then does it:

| A job that… | |
|---|---|
| is new here | is imported. |
| has a name a job here has already | is skipped (the default), imported as a copy under the first free name (`--on-conflict copy`: `projects-2`, `projects-3`…), or put in place of the job here (`--on-conflict replace`), which keeps its name and so its history. |
| has no saved repository password here | is imported; enter the password you saved with `replica key set`, or in its editor, before it can run. |
| uses a drive that isn't connected | is imported, and shows the target as not connected until it is. |
| doesn't validate, alone or beside the jobs here (a mirror over a repository, say) | is **not** imported, with the reasons. |

**Every job imported is turned off** (`enabled = false`), so nothing runs and no task is made
until you've checked it and turned it on. The commands a job's steps run are listed as it's
imported. `--only` imports just the jobs named. A file that isn't a jobs file, or comes from a
newer Replica, is refused.

Exit codes: 0 done; 1 nothing imported because every job chosen has problems, or the settings
couldn't be saved; 2 some imported and some not; 4 a usage error (no such job, the file exists, a
file that isn't a jobs file).

## `replica tasks`

```
replica tasks list [--json]
replica tasks enable <task>
replica tasks disable <task>
replica tasks remove <task> | --all
```

Replica's tasks in Windows Task Scheduler, in the `\Replica` folder. They exist only when
`schedule_mode = "task-scheduler"` is set (off by default: the tray app's built-in timer runs
schedules while it runs). Each task is named after its job and runs `replica-tray.exe --run <job>`
while you're logged on.

| Command | |
|---|---|
| `list` | Every task: its job (or that the job is gone, an orphan), schedule, next and last run, last result, and whether it's on. `--json` prints them as JSON ([cli-json.md](cli-json.md#replica-tasks-list---json)). |
| `enable <task>` | Turns a task on. |
| `disable <task>` | Turns a task off, without removing it. |
| `remove <task>` | Removes a task. |
| `remove --all` | Removes every one of Replica's tasks, and the folder. |

`<task>` is the task's name, which is its job's name. Exit code 0, or 4 if the task doesn't exist
or Task Scheduler refused.

## `replica-tray.exe`

The tray app is a separate, windowed exe, so it never opens a console:

```
replica-tray                 the tray icon, its menu and the built-in timer (one at a time),
                             with its window open
replica-tray --show          the same (what the installer runs after an interactive install)
replica-tray --background    the tray alone, its window closed (what Start with Windows runs)
replica-tray --run <job>     runs one job, notifies if it failed, and exits with its code
```

Started while a tray runs, `replica-tray` (with or without `--show` or `--background`) brings
that tray's window forward and ends. `--run` never opens the window.

`--run` is what Task Scheduler's tasks run. It does nothing while schedules are paused or the
Task Scheduler option is off. Its exit codes are those above.

## Where things are

| | Installed | Portable |
|---|---|---|
| Settings | `%APPDATA%\Replica\settings.toml` | `settings.toml` beside the exe |
| Run history | `%APPDATA%\Replica\history.db` | `history.db` beside the exe |
| Run logs | `%APPDATA%\Replica\logs\runs\` | `logs\runs\` beside the exe |

Replica is portable when a file named `portable` (empty, no extension) or a `settings.toml` sits
beside the exe. Share logins and repository passwords are always in Windows Credential Manager,
never in the settings file.
