# Testing Replica

How the tests are run, what to run when, and the rules every test keeps. For contributors
and coding agents alike.

## Running the tests

The suite runs under [cargo-nextest](https://nexte.st), a build-only tool (never shipped; see
RELEASING's build-only tools):

```powershell
cargo install cargo-nextest --version 0.9.149 --locked   # once
pwsh scripts/fetch-restic.ps1                            # once: the pinned restic the tests use

cargo nextest run --workspace                            # the default tier: every test but the ignored
cargo test --workspace --doc                             # the doc tests (nextest doesn't run them)
```

nextest runs every test as its own process, all the test binaries side by side, so the full
default tier takes **about 30 s** on a 16-thread PC (measured below), where `cargo test
--workspace` takes about 3½ minutes because it runs the binaries one after another. Its settings
are in `.config/nextest.toml`.

**`cargo test --workspace` still works**, for anyone without nextest: the same tests, slower.

**Just the tests you changed:** name the package and the binary (a file under `tests/`) or the
unit tests' module:

```powershell
cargo nextest run -p replica -E 'binary(window_settings)'
cargo nextest run -p replica-core -E 'binary(restic_driver) | test(/mirror::/)'
cargo nextest run -p replica -E 'binary(window) & test(/history/)'
```

**On a busy PC** (several agents building at once), cap the threads: `-j 4`, and
`CARGO_BUILD_JOBS=4` for the build. The default tier then takes about 80 s.

**restic:** tests that need the pinned restic skip without it, unless `REPLICA_REQUIRE_RESTIC=1`
(CI sets it), when they fail instead.

## Tiers

| Tier | What | Run by |
|---|---|---|
| **Default** | Every test that isn't ignored | `cargo nextest run --workspace`; CI on every push and pull request, and nightly |
| **Stress** | Ignored tests named `stress_…`, with `#[ignore = "stress: …"]`: sweeps and loops too long for every run, whose quick version is in the default tier | `cargo nextest run --workspace --run-ignored only -E 'not binary(ui_shots)'`; CI nightly |
| **Screenshots** | `crates/replica/tests/ui_shots.rs`, ignored: renders PNGs for review (ARCHITECTURE, The window) | By hand, on changes to what's on screen |

A test that repeats something many times to catch a rare race, or sweeps many sizes, goes in the
stress tier, with a quick version (a few representative cases) in the default tier, so nothing it
covers is lost from every run. Today: `stress_a_click_on_a_row_s_node_chooses_it_at_every_width_scale_and_size`
(`crates/replica/tests/window.rs`), the full sweep behind a click on a row.

## Tests that take turns

Test binaries no longer run one after another, so tests in different binaries can run at the
same moment. A test that can't share something with any other test goes in a **test group**
with `max-threads = 1` in `.config/nextest.toml`, never in a lock of its own (nextest runs each
test as its own process, so an in-process `Mutex` doesn't make tests take turns):

- `credential-manager`: `replica-win`'s `credentials.rs`, whose test stores take turns (its
  notes).

Everything else shares what it uses safely, each test under names of its own: Credential Manager
under a per-test prefix (every call takes a per-user named mutex), Task Scheduler in a
per-test `\ReplicaTest-<pid>-…` folder, temp folders, `APPDATA` per test.

## What to run when

- **Each round:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and the tests of the crates and files you changed (as above). Not the full suite.
- **Before Review, once:** the full default tier (`cargo nextest run --workspace`, and the doc
  tests if you changed a doc comment's example).
- **Before merging:** the full default tier again only if main moved since that run.
- **Screenshots:** only on changes to what's on screen.
- **Reviewers** rely on the builder's run record (the commit, each command, its result and where
  the log is), and rerun only what they doubt, plus the tests of anything they probe.

## A flaky test

Fix it at its cause: a test that waits for what it expects, or a race in the product. **The proof
is the affected test file 50 times under parallel load plus one full run**, never 50 full runs:

```powershell
# 50 runs of one binary, each beside another binary's run as load; then the full tier once.
cargo nextest run -p replica -E 'binary(window_settings)' --stress-count 50
cargo nextest run --workspace
```

(`--stress-count` runs each chosen test that many times; run another binary beside it, or the
default tier in a second terminal, for the load.) Log both runs where the reviewer can read them.

## Rules every test keeps

- **Wait for conditions, never for time.** Poll what you expect to see, with a generous bound for
  a hung run (`step_until`, `wait_for`), rather than sleep, then check, or step a fixed number of
  frames. A timeout bounds a hung run and is never the fix. When a test needs a timeout to pass
  (a step's own timeout under test), start it short and lengthen it only when the test shows it
  was too short (`with_a_started_program` in `command_tree.rs`).
- **Clean up, even on failure.** Temp folders, test-only Credential Manager prefixes and Task
  Scheduler folders are removed in `Drop`, so a failed assertion still cleans up.
- **Pass under load,** not only alone: every test runs beside about 400 others.
- **Press what layout can move through AccessKit** (`click_accesskit`), as a screen reader does;
  a pointer click (`click`) is for what the click itself tests.
- **No global keyboard or mouse input, ever,** in tests, probes or scripts: no SendKeys,
  SendInput, keybd_event or mouse_event, wscript or AutoHotkey, and no clicking at screen
  coordinates. It goes to whatever is in the foreground, which is the user's work. Drive
  only your own windows: UI Automation patterns (ValuePattern, InvokePattern and the like) on
  elements found by your own process ID or window handle, messages posted to your own window's
  handle, or `click_accesskit` in window tests. If you can't, don't drive the real UI: prove it
  with tests, and say so in the pull request.
- **Check each frame's accessibility tree** where something new opens (a dialog, a page), with
  `assert_names_resolve` in `window.rs`: a node named after another (`labelled_by`) that isn't in
  that frame's tree panics AccessKit's Windows adapter, and with it the window. `run()`
  only shows the settled frame, and a new dialog's first one is laid out unseen.
- **Nothing outside temp folders:** no test writes to the real `%APPDATA%\Replica`, the real
  Credential Manager names or Task Scheduler's `\Replica` folder.
- **The real tray in a test** runs from a portable copy with `REPLICA_TEST_TAG=ReplicaTest-…`.
  That gives it its own Task Scheduler folder, Start with Windows key (never the Run
  key), single-instance objects and Credential Manager prefix, so it never touches, wakes or
  quits an installed Replica. Drive it by its own process ID and window handles, never with
  global input. The end-to-end tray test opens a real window, so it's ignored in the suite; run
  it on purpose with `cargo nextest run -p replica --test tray --run-ignored only` when the
  tray, the window's start or Open changes.

## Disk space

Cargo never removes an old test exe: each round of edits builds about 20 new ones, and the
incremental caches grow beside them, so a checkout's `target\debug` reaches tens of GB within a
few days' work. Two things keep it down:

- **Less debug info in dev and test builds** (`Cargo.toml`): `debug = "line-tables-only"` for
  the workspace's crates, so panics, test failures and backtraces still name the file and line,
  and none for the dependencies. Release builds are unchanged. A debugger shows no local
  variables in a dev build any more; to get them back for a session, set
  `CARGO_PROFILE_DEV_DEBUG=full` (and rebuild).
- **`pwsh scripts/clean-build.ps1`** removes the workspace crates' dev and test builds (their
  exes, rlibs, every stale test exe and the incremental caches) and keeps the compiled
  dependencies and `target\release`, so the next build takes about half a minute. `-All` removes
  everything cargo built (a full rebuild follows). Neither ever touches `target\ui-shots`,
  `target\dist`, or anything outside the checkout's `target`, and both stop before cleaning
  anything if `target` is, or holds, a junction or symbolic link (cargo never makes one):
  `pwsh scripts/test-clean-build.ps1` checks all of it in a throwaway workspace, and CI's
  Windows job runs that check on every push to main and every PR. Contributors run the
  script once a change is merged (AGENTS.md), and the dist build runs it once
  checked (RELEASING.md).

Measured on a 16-thread PC, 24 Oct 2026, at a6196b9, with `CARGO_BUILD_JOBS=4` and `--test-threads=4`
(each from `cargo clean`; a "one-line change" is a comment added to `replica-core`'s `lib.rs`,
then `cargo nextest run --workspace --no-run`):

| | Before the trim (full debug info) | After |
|---|---|---|
| `target\debug` after the full default tier | 7.0 GB (deps 5.1, incremental 0.8) | **3.2 GB** (deps 2.1, incremental 0.5) |
| The full default tier, build included | 273 s | 230 s |
| A one-line change in `replica-core`, every test exe rebuilt | 19 s | **13–14 s** |
| `target\debug` after two such changes | 8.1 GB | 3.7 GB |
| `clean-build.ps1` frees, then the test build after it | 5.9 GB, 41 s | 2.4 GB, 29 s |

## Timings

The default tier on a 16-thread PC, 24 Oct 2026, on the move to nextest (each build already done):

| Run | Wall time |
|---|---|
| `cargo test --workspace -- --test-threads=4`, before nextest | about 3½ min |
| `cargo test --workspace -- --test-threads=4`, after | 2 min 15 s |
| `cargo nextest run --workspace`, before the waits were trimmed | 34 s |
| **`cargo nextest run --workspace`** | **32 s** (392 tests) |
| `cargo nextest run --workspace -j 4` | 77 s |

The longest tests set the floor: the command line's end-to-end runs (`cli.rs`, about 20 s each,
a chain of `replica.exe` and restic calls) and the window's restore and test restore
(`window_restore.rs`). `restic init` alone takes about 3 s (restic tunes its key derivation to
the PC), so tests make one repository each and no more.
