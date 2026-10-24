# Replica user guide

Replica backs up your folders to an external drive or a NAS share, on a schedule or when you ask,
and tells you how each run went. This guide covers setting it up, keeping the backups safe,
getting files back, and what to do when something goes wrong.

- [Versioned or mirror](#versioned-or-mirror)
- [Setting up a job](#setting-up-a-job): [in the window](#in-the-window), [in settings.toml](#in-settingstoml)
- [Where backups go](#where-backups-go): [drives](#a-drive), [NAS shares](#a-nas-share)
- [The repository password](#the-repository-password)
- [Before and after steps](#before-and-after-steps)
- [Schedules](#schedules)
- [The tray icon](#the-tray-icon)
- [Restoring](#restoring)
- [A test restore routine](#a-test-restore-routine)
- [Retention and verify](#retention-and-verify)
- [Keeping something forever](#keeping-something-forever)
- [Recover without Replica](#recover-without-replica)
- [Running Replica from other apps](#running-replica-from-other-apps)
- [Installed or portable](#installed-or-portable)
- [Troubleshooting](#troubleshooting)

The command line is described in full in [cli.md](cli.md), and every settings key in
[settings.example.toml](settings.example.toml).

## Versioned or mirror

Each job is one of two kinds.

**Versioned (the default, and what you almost always want).** Each run adds a *snapshot* to a
[restic](https://restic.net) repository on the target. A snapshot is the whole set of files as
they were at that moment, so you can go back to yesterday's or last month's version of any file,
including ones you've since deleted. The repository is encrypted, and deduplicated: a file that
didn't change takes no extra space, so many snapshots cost little more than one. Old snapshots are
thinned out by the job's [retention](#retention-and-verify).

**Mirror.** The target is kept as an exact, plain copy of the sources, which you can open in
Explorer. That's handy for photos on a second drive, but a mirror copies your mistakes too: delete
or damage a file, or let ransomware encrypt it, and the next run does the same to the copy, with
no older version to go back to. So:

- choosing a mirror shows a warning, and only "Use a mirror anyway" (or
  `mirror_deletes_confirmed = true` in the settings file) makes it one;
- a mirror run stops, deleting nothing, if it would delete more than a set share of the target's
  files (30% unless you change it), as when a source folder was emptied by mistake;
- a file that turned into a folder (or the other way round) is reported as a type conflict and
  left alone, for you to sort out by hand.

**Use versioned for anything you care about.** Use a mirror only as a second, convenient copy of
something that's also backed up with versions.

## Setting up a job

A job is: a **name**, the **sources** (folders or files to back up, each with things to leave
out), one or more **targets** (where to), the **mode** (versioned or mirror), an optional
**schedule**, **retention** (versioned only), and **before and after steps**. A job with several
targets backs up to each in turn, and each target gets its own result.

### In the window

Open Replica from its icon in the notification area (or the Start menu), then **Jobs > Add job**:

1. **Name:** letters, digits, `.`, `_` and `-`. The command line uses it (`replica run documents`).
2. **Sources:** **Add a folder…** or **Add a file…**. A new source starts with common things to leave
   out (`node_modules`, `**/.godot`, `dist`, `out`, `*.tmp`); edit them, one pattern a line. A name
   (`*.tmp`, `node_modules`) matches at any depth; a path (`build/cache`, `**/.godot`) is matched
   from the source's own folder.
3. **Targets:** **Add a drive** or **Add a network share** (see [Where backups go](#where-backups-go)).
4. **Mode**, **Schedule**, **Retention**, **Notify me when it succeeds** and the **before and after
   steps**.
5. **Save.** Every field is checked first, and anything wrong is listed. For a new versioned
   target, Replica now sets up its repository and shows its [password](#the-repository-password)
   once: save a copy, then click **I've saved a copy**.

The job takes effect at once. **Run now** runs it; its result, progress and next run are in the
list. **Edit**, **Delete** (which asks first, and removes its Task Scheduler task too) and the
**On** box are beside it.

### In settings.toml

Everything the window sets is in one file you can edit by hand: `%APPDATA%\Replica\settings.toml`
(or `settings.toml` beside the exe for a portable copy). The tray app picks up changes within 15
seconds, and the command line reads the file each time.
[settings.example.toml](settings.example.toml) has every key, with comments; a short job:

```toml
[[jobs]]
name = "documents"
schedule = "daily 21:00"

[[jobs.sources]]
path = 'C:\Users\me\Documents'
exclude = ["*.tmp", "node_modules"]

[[jobs.targets]]
kind = "drive"
name = "Backup drive"
label = "BACKUP"
path = 'Replica\documents'
```

Then set its targets up once, from a terminal:

```
replica init documents
```

For a versioned target this makes the repository and shows its password: save a copy, then type
`saved`. Use single quotes around Windows paths in the file, so backslashes stay as they are.

The window rewrites this file when you change settings there, keeping every key but not comments
you've added.

## Where backups go

### A drive

A drive target is a folder on a local or external drive, found by the drive's **volume ID** (and
its label, as a fallback), not its letter. An external drive that comes back as `F:` instead of
`E:` is still found. In the window, pick it from the connected drives (**Look for drives again**
after plugging one in) and give the folder on it. In the settings file, give `volume_id` or
`label` (or both) and `path`, the folder relative to the drive's root.

A drive that isn't plugged in makes the run **skipped**, not failed: nothing is wrong, and the
next run catches up. If it stays away long enough for the job to be overdue, you're told.

### A NAS share

A share target is a UNC path, `\\nas\share\folder`. If the share needs a login, enter its user name
and password with the target in the window: they're saved in **Windows Credential Manager**, never
in the settings file, as one login per share (`Replica/share:\\nas\share`). Replica connects the
share for a run, without a drive letter, and disconnects it afterwards.

Setting it up in `settings.toml` instead: name the login (`credential = "nas-login"`), and save it
with Replica, which asks for the password (twice, not shown as you type):

```
replica login set nas-login --user NAS\me
```

Or with Windows' own `cmdkey`, under that name with `Replica/` in front: `cmdkey
/generic:Replica/nas-login /user:NAS\me /pass` (`/pass` on its own asks for the password, so it
isn't left in your terminal's history). A NAS
that's switched off or unreachable makes the run **skipped**, like an unplugged drive.

## The repository password

**Every versioned target's repository is encrypted with its own password. Lose it, and the backup
can't be read by anyone, Replica included.**

Replica makes a strong password when the target is set up and keeps it in Windows Credential
Manager. **That copy goes with the PC**: if the PC dies, is stolen or is reinstalled, so is the
password, and that's exactly when you need the backup. So:

- **When the password is shown, save a copy somewhere other than this PC**: in your password
  manager, or written down and kept somewhere safe. The window won't save the job until you click
  **I've saved a copy**; `replica init` wants `saved` typed in.
- To see it again later: `replica key show <job>`.
- To put it back (on a new PC, say): `replica key set <job>`, or **Enter a saved password…** on the
  target in the job's editor. See [Troubleshooting](#troubleshooting).
- The password is the **only** key. There's no reset and no recovery service. Without it, the
  repository is just encrypted noise.
- Never put it in the settings file, a script or a note in the backup itself.

Share logins are kept the same way, in Credential Manager.

## Before and after steps

**Before steps** run before the backup; if one fails, the job fails and nothing is backed up.

- **SQLite snapshot:** a consistent copy of a live SQLite database, made safely while the app
  using it keeps running, into Replica's own staging folder. The copy is backed up; the live file
  and its `-wal`, `-shm` and `-journal` files are left out. Use it for any app that keeps its data
  in SQLite, rather than backing up the file mid-write.
- **Command:** any command, with a time limit (300 seconds unless you change it). A non-zero exit
  code or running out of time fails the job, and the command is stopped with every process it
  started. It runs without a window, and its output goes to the run's log. **A step can't ask for
  input:** there's nowhere to type it, so give it everything on its command line (a script that
  waits for a key press waits until its time limit).

**After steps** run after the backup:

- **Command:** as above. A failure makes the run "partly done".
- **Verify** (versioned only): `restic check`, reading back part of the stored data, on its own
  schedule (after every backup if you don't give one). See [Retention and verify](#retention-and-verify).

## Schedules

A job's schedule is one of:

| Schedule | Means |
|---|---|
| `daily 02:30` | every day at that time |
| `weekly mon,thu 21:00` | on those days at that time |
| `every 6h`, `every 90m` | that long after its last run |

Without a schedule, a job runs only when you ask. A run missed while the PC was off runs once,
when Replica is back; it isn't repeated for each missed time.

**What runs the schedules** is set in **Settings > Schedules**:

- **The built-in timer (the default):** the tray app runs schedules while it's running. Keep
  **Start with Windows** on (in the tray menu or Settings) so it always is.
- **Windows Task Scheduler (off by default):** Replica makes a task for each scheduled job, in Task
  Scheduler's "Replica" folder, for your account only and with no admin rights. Backups then run
  even when the tray app isn't running, **while you're logged on**. **Settings > Scheduled tasks**
  shows every task, with its next and last run, and lets you turn each on or off, delete one or
  all, or open Task Scheduler itself. A task whose job is gone is flagged. Turning the option off
  asks whether to delete the tasks (the default) or keep them turned off; the built-in timer then
  takes over.

The two never both run a job, and a job never runs twice at once: a second run while one is going
is skipped. **Pause schedules** (tray menu or Settings) stops scheduled runs, by the timer and by
Task Scheduler alike; you can still run jobs by hand. Quitting the tray app stops only the
built-in timer.

A job is **overdue** when its last good backup is older than its schedule allows, plus an hour.
You're told once a day while it stays overdue (not while schedules are paused), and the Jobs page
shows **Overdue** in its Status column, even if the job keeps running and failing: hover over it
for when it was due, its last good backup and how its last run ended.

## The tray icon

Replica lives in the notification area (on Windows 11, it may start in the overflow; drag it onto
the taskbar to keep it in view):

| Icon | Means |
|---|---|
| Plain | All well |
| Turning arrow | A job is running; the tooltip shows which, its progress and what's waiting |
| Red dot | A job's last run failed; the tooltip names it |
| Amber dot | A job is overdue |

Click it to open the window; right-click for **Run all now**, **Run** a single job, **Stop** the
running one, **Open Replica…**, **Pause schedules**, **Start with Windows** and **Quit**.

**Notifications:** a failed or partly done run always notifies you. A successful one notifies only
if its job asks (**Notify me when it succeeds**). A skipped run doesn't; an overdue notice covers a
target that stays away.

Starting Replica yourself (the Start menu, Explorer, a pinned icon) opens its window; starting it
again while it runs brings the window forward. **Start with Windows** starts it in the
notification area only, so logging on doesn't open the window.

Closing or minimising the window keeps Replica running in the notification area (the first time,
it says so). **Quit** in the tray menu stops the tray app and its built-in timer. **Task
Scheduler's tasks keep running without it**, by design: to stop scheduled backups whatever runs
them, use **Pause schedules**, or turn Task Scheduler off (or turn its tasks off) in **Settings**.

## Restoring

Restoring never touches your files unless you ask it to: by default it goes to a **new folder**.

### From the window

**Restore** tab:

1. Pick the **job** and, if it has several, the **backup** (target). Its snapshots are listed,
   newest first.
2. Pick a **snapshot**, then open the folders to find what you want (each folder is read when you
   open it). Tick files, or **All of** a folder.
3. **To a new folder** is the default (under `Restored by Replica` in your user folder, named
   after the job and the snapshot's time); change it or **Browse…** if you like. **Where they were,
   replacing what's there now** puts them back over the originals, after asking you to confirm.
4. **Restore**, then **Open the folder**.

### From the command line

```
replica snapshots documents
replica restore documents latest "C:\Users\me\Documents\report.docx" --to C:\Restored
replica restore documents 4f2a9c1e "C:\Users\me\Documents\Projects" --to C:\Restored
```

`latest` is the job's newest snapshot; otherwise give a snapshot's ID (or its start) from
`replica snapshots`. The item is restored inside the `--to` folder by its own name. The folder must
be new or empty and can't be one of the job's sources; `--overwrite` allows restoring over the
originals. See [cli.md](cli.md#replica-restore).

A **mirror** target has no snapshots: it's a plain copy, so copy files back from it in Explorer.

## A test restore routine

A backup you've never restored from is a backup you hope works. Once a month, and after any
change to a job:

1. **Restore tab > Test restore**, for each versioned job and each of its targets. Replica restores
   a few random files from the latest snapshot to a temporary folder and compares them, byte for
   byte, with the originals. It says how many were compared and that they're the same, or names
   any that differ. History keeps every test restore. (Files that changed since the backup aren't
   compared: they're expected to differ.)
2. **Restore one file and one folder by hand**, to a new folder, from an older snapshot, and open
   them.
3. **Check you can find the repository password** you saved, away from this PC.
4. Every few months, try [Recover without Replica](#recover-without-replica) on another PC.

A test restore that says files differ, though their sources haven't changed since the backup,
means the backup doesn't hold what it should: run **verify** (below), check the drive, and back up
again.

## Retention and verify

**Retention** (versioned jobs) decides which snapshots are kept: by default the last snapshot of
each of the last 7 days, 4 weeks and 6 months. After each backup, Replica removes the snapshots
retention doesn't keep (`restic forget`) and frees their space (`restic prune`). Only this job's
own snapshots are touched, even in a repository two jobs share.

**Verify** (an after step, versioned only) runs `restic check` and reads back part of the stored
data, `5%` unless you set `1/10` or a size like `500M`, to catch damage on the drive. Give it a
schedule (`weekly sun 03:00`) so it doesn't run after every backup. `replica verify <job>` runs it
now.

## Keeping something forever

Retention eventually removes the last snapshot that holds a file once you've deleted the original.
Two ways keep something for good:

- **Keep forever** (a job): in the editor's Schedule and retention, choose **Keep: Everything,
  forever** (`keep_forever = true` under `[jobs.retention]`). The job's runs never remove a
  snapshot (no `restic forget` or `prune`), so its backup grows over time; verify still runs. The
  Jobs page tags it **Versioned · Forever**.
- **Pin this backup** (one snapshot): on the Restore page, choose a snapshot and click **Pin this
  backup**, or run `replica pin <job> <snapshot>`. A pinned snapshot is kept whatever any job's
  retention says, and shows **Pinned**. **Unpin…** (or `replica unpin`) asks first, then lets the
  retention remove it again. restic gives a snapshot a new ID when it's pinned or unpinned: the
  window chooses it again for you, and `replica pin` prints the new ID.

For files you can't replace, don't rely on one backup alone: keep **at least two copies, one of
them offline** (a drive you unplug, or a NAS elsewhere), and **test-restore** them before you
delete an original.

## Recover without Replica

Replica's versioned backups are ordinary **restic repositories**. If Replica is gone, or the PC
is, **any copy of restic** can read them with just two things: **the repository folder** and
**its password**. Download restic for your system from <https://restic.net> (a newer restic reads
older repositories).

Each repository has a `HOW-TO-RESTORE.txt` beside it saying the same, with its own path filled in.
It holds no secrets.

List the snapshots:

```
restic -r "E:\Replica\documents" snapshots
```

Restore the latest snapshot into a new, empty folder:

```
restic -r "E:\Replica\documents" restore latest --target "C:\Restored"
```

Restore just one file or folder from it, by its path as the snapshot shows it (`C:\Users\me` is
`/C/Users/me`):

```
restic -r "E:\Replica\documents" restore latest --target "C:\Restored" --include "/C/Users/me/Documents/report.docx"
```

restic asks for the password each time. To restore an older snapshot, give its ID from the list
instead of `latest`. If two of your jobs share a repository, add `--tag <job>` to pick that job's
snapshots (Replica tags each snapshot with its job's name).

- **A NAS repository** works the same by its UNC path: `restic -r "\\nas\backup\replica\documents" snapshots`
  (connect the share in Explorer first if it needs a login).
- **A mirror target** needs nothing at all: it's plain files and folders. Copy them back.
- **The password is the only key.** That's why you saved a copy.

## Moving jobs to another PC

Jobs can be exported to a file and imported from one: to set the same jobs up on a new PC, or to
keep a copy of them. The file **holds no passwords**: only the names of share logins, as
`settings.toml` does.

**Export…** on the Jobs page lists every job with a check box. The job selected in the table is
ticked (every job, if none is), or use a job's **⋯ > Export…** for just that one. Type in the
filter to find jobs by name or target; **Select all** and **Select none** act on the jobs it shows.
**Export** asks where to save the file (`.replica-jobs.toml`).

**Import…** asks for a file, then shows each job in it before anything changes:

| Badge | |
|---|---|
| **New** | No job here has its name. |
| **Name exists** | Choose: **Skip** it (the default), **Import as a copy** (`Documents-2`), or **Replace** yours, which keeps its name and so its history. |
| **Needs a password** | Its backup's repository password isn't saved on this PC yet. It's imported; enter the copy you saved with **Enter a saved password…** in its editor before it runs. |
| **Drive not found** | Its drive isn't connected now. It's imported, and shows the drive as not connected until it is. |
| **Invalid** | It has problems (shown under it), such as a mirror over one of your backups. It can't be chosen, and is never imported. |

The commands a job runs are listed under it. **Imported jobs arrive disabled**: check each one,
then turn it on on the Jobs page. A file that isn't a jobs file, or comes from a newer Replica, is
refused, and nothing is imported.

On the command line:

```
replica export --all -o jobs.replica-jobs.toml
replica import jobs.replica-jobs.toml
```

(or name the jobs to export, `--only` the jobs to import, and `--on-conflict copy` or `replace`
for names you have already). See [cli.md](cli.md) for the options.

## Running Replica from other apps

`replica.exe` is made for scripts and other apps. Many apps have a hook to run a command, such as
on close or after a build:

```
replica run documents --quiet
```

`--quiet` prints nothing unless something goes wrong, and the exit code says how it went: **0** ok,
**1** failed, **2** partly done, **3** skipped (a target isn't available, or the job is already
running), **4** a usage or settings error. `--json` prints results for programs to read
([cli-json.md](cli-json.md)). The MSI puts `replica.exe` on your PATH; for a portable copy, give
its full path.

## Installed or portable

| | Installed (MSI) | Portable (zip) |
|---|---|---|
| The program | `%LOCALAPPDATA%\Programs\Replica` | wherever you unzipped it |
| Settings, history, logs | `%APPDATA%\Replica` | beside the exes |
| Admin rights | none | none |
| `replica.exe` on the PATH | yes | no |

A copy is portable when a file named `portable` (empty, no extension), or a `settings.toml`,
sits beside its exes; the zip has the marker already. Run a portable copy from a USB stick and its
settings and history travel with it. If its folder can't be written to (a read-only stick or
folder), Replica says so and saves nothing anywhere else.

Share logins and repository passwords are always in Windows Credential Manager, in both cases.
**Settings > Where Replica keeps its files** shows the exact folders, with buttons to open them.

Uninstalling the MSI (Settings > Apps) removes the program, Replica's scheduled tasks and its Start
with Windows entry. It **never** removes your settings, history, logs or backups.

## Troubleshooting

**Where to look first.** **History** in the window shows every run, each target's result and why,
and **Open the log** for that run. The logs are in `logs\runs\` (one per run) and `logs\` (one per
start of the tray or the command line) in Replica's folder (see above); **Settings > Open the logs
folder** opens them. `replica status` shows every job's last result and what's running.

**"The target isn't available" (skipped).** The drive isn't plugged in, or the NAS is off or out
of reach. Plug it in or switch it on, and run the job again, or wait for the next scheduled run.
If a drive is plugged in and still not found, check it's the same drive: Replica finds it by its
volume ID, then its label (**Look for drives again** in the job's editor shows what's connected).
For a NAS, check you can open the share in Explorer.

**"There's no saved login for …".** The settings name a share login that isn't in Credential
Manager. Enter it in the job's editor, or save it with `replica login set <name>` (see
[A NAS share](#a-nas-share)).

**"The repository password for … is wrong" / "There's no saved repository password for …".**
Credential Manager doesn't have the right password for that repository: on a new PC, after
reinstalling Windows, or if it was removed. Put back the copy you saved:

- in the window, edit the job and click **Enter a saved password…** on the target, type the
  password and click **Check and save**; or
- on the command line, `replica key set <job>` (add `--target <name>` for one target), and type
  the password when asked. It isn't shown as you type.

Replica checks that the password opens the backup before it saves it, so a typo is never saved.
A saved password that already opens the backup is never replaced by accident (the window asks
before **Replace it anyway**; `replica key set` needs `--force`). Once it's back, the job runs,
lists and restores as before, older snapshots from the other PC included. A new PC needs only the
repository's folder, the job (set up again, pointing at that folder) and this password.

If you have no copy of the password, the old backup can't be read: point the target at a new,
empty folder, set it up again (a new password, to save this time) and start a new backup. You can
also [restore with restic](#recover-without-replica) directly with the saved password.

**"Waiting for … to finish with …".** Several jobs can back up to the same folder (one
repository): they take turns, so a job that starts while another is using the repository waits
for it, from the tray, a schedule or the command line alike, and the window shows **Waiting its
turn**. A restore, a test restore, a verify and setting a repository up wait their turn the same
way, saying what they're waiting for. It waits up to two hours (`repository_wait_minutes` in `settings.toml`), then the run
fails and you're notified. Jobs on different folders don't wait for each other. Two mirror jobs
can't share a folder, or a folder inside the other's, and a mirror can't be in or around a
backup repository: each would delete the other's files, so the settings refuse it.

**"The repository … stayed locked by another restic".** Replica waits up to 10 minutes for a
restic that it didn't start (another backup, restore or check) and removes a lock restic itself
says is stale. If the repository stays locked, the lock is held by a restic that's still running,
or one restic can't yet tell is stale: wait for it to finish. If none is running (a PC that was
switched off mid-backup, say), remove the lock with restic itself, using the password from
`replica key show`:

```
restic -r "E:\Replica\documents" unlock
```

**"There's no restic repository at … yet".** The target hasn't been set up: `replica init <job>`,
or edit the job in the window and click **Set up now** on the target.

**"restic wasn't found at …".** The bundled `restic.exe` is missing from beside Replica's exes
(reinstall, or unzip the portable copy again), or **Settings > restic** points somewhere it isn't:
click **Use the one that comes with Replica**.

**A mirror run stopped without deleting anything.** It would have deleted more of the target's
files than the job allows, which usually means a source was emptied or moved by mistake. Check the
source. If the deletions are what you want, raise the job's deletion limit (100% allows any), run
it, and set the limit back.

**"The settings file … can't be read".** It has a mistake in it (often after editing by hand): the
message says where. Replica keeps running with the settings it had and changes nothing until the
file is fixed.

**A job didn't run on schedule.** Check **Pause schedules** is off and the job is **On**. With the
built-in timer, the tray app must be running (turn on **Start with Windows**). With Task Scheduler,
check **Settings > Scheduled tasks**: its task should be on, and runs happen only while you're
logged on.
