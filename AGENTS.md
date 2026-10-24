# Replica: notes for agents

Replica is a Windows backup tool: restic-based versioned backups and plain mirrors, run from a
tray app, a window and a command line. These notes are for every agent working on this repository,
whichever CLI runs it: Codex reads this file, and Claude Code reads it through `CLAUDE.md`.

- Building, the code's layout, strings and the licence rule: [CONTRIBUTING.md](CONTRIBUTING.md)
- How the code fits together, and the decisions behind it: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- **Testing: what to run when, flaky tests, and the rules every test keeps:
  [docs/TESTING.md](docs/TESTING.md).** In short: each round, fmt, clippy and the tests of what you
  changed; the full suite (`cargo nextest run --workspace`) once before Review; a flaky test's
  proof is its file 50 times under load plus one full run, never 50 full runs; tests wait for
  conditions, never for time.
- Releases: [docs/RELEASING.md](docs/RELEASING.md)

**Disk space** ([docs/TESTING.md](docs/TESTING.md#disk-space)): cargo never removes old test
exes, so a checkout's `target` grows by gigabytes over a few changes.
- **Builders:** once your work is merged, run `pwsh scripts/clean-build.ps1` in your checkout.
  It keeps the compiled dependencies, so the next change's first build stays quick.
- **Reviewers:** once your verdict is posted, delete your own probe folders under `%TEMP%`
  (`replica-*-review-*`, `replica-*-probe-*`).

**No global keyboard or mouse input, ever** ([docs/TESTING.md](docs/TESTING.md#rules-every-test-keeps)),
in tests, probes or scripts: no SendKeys, SendInput, keybd_event or mouse_event, wscript or
AutoHotkey, and no clicking at screen coordinates. Synthetic input goes to whatever is in the
foreground, which is the user's work (a probe's typed path once arrived as a message the user
sent).
- **Drive only your own windows:** UI Automation patterns (ValuePattern, InvokePattern and the
  like) on elements found by your own process ID or window handle, or messages posted to your
  own window's handle. In window tests, egui_kittest's `click_accesskit`.
- **If you can't do it that way,** don't drive the real UI: prove it with tests, and say so in
  your summary of the change.

Probes, logs and scratch files go in your scratchpad or a temp folder, never in the repository or
beside a checkout. Personal, machine-specific notes belong in `CLAUDE.local.md` (Claude Code reads
it; keep it out of git), not here.
