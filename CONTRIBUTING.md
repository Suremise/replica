# Contributing to Replica

Thanks for helping. Bug reports, fixes, translations and ideas are all welcome. Please read the
[Code of Conduct](CODE_OF_CONDUCT.md) first, and report security problems privately, as
[SECURITY.md](SECURITY.md) says, not in a public issue.

For anything bigger than a small fix, open an issue first so we can agree on the shape before you
spend time on it.

## Building and checking

You need Windows 10 or 11, [Rust](https://rustup.rs) and PowerShell 7. The toolchain is pinned in
`rust-toolchain.toml`, and rustup installs it on first use.

```
pwsh scripts/fetch-restic.ps1        # once: the pinned restic, into third_party\restic\bin\
cargo build
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace        # the tests, in parallel (cargo install cargo-nextest)
cargo test --workspace --doc         # the doc tests, which nextest doesn't run
cargo deny check                     # licences, advisories and sources (cargo install cargo-deny)
```

`cargo test --workspace` runs the same tests without nextest, one binary after another (about
3½ minutes, against about half a minute). [docs/TESTING.md](docs/TESTING.md) says what to run
when, how to run only the tests you changed, and the rules every test keeps.

**Disk space.** Dev builds keep only line tables for Replica's crates and no debug info for the
dependencies (`Cargo.toml`'s `[profile.dev]`), and cargo never removes old test exes, so
`pwsh scripts/clean-build.ps1` now and then frees what piles up in `target\` while keeping the
compiled dependencies (`-All` removes everything cargo built). docs/TESTING.md, Disk space, has
the numbers.

A pull request must pass all of these: the CI runs them on Windows, and runs `replica-core`'s
tests on Linux too.

**restic.** Replica bundles a pinned restic release. `scripts/fetch-restic.ps1` reads
`third_party/restic/VERSION`, downloads that release and checks it against its pinned SHA-256; it
fails on a mismatch. The exe is never committed. Tests that need restic skip without it, unless
`REPLICA_REQUIRE_RESTIC=1` is set (the CI sets it), when they fail instead. Moving the pin means
updating `VERSION` and rerunning `scripts/audit-restic-licences.py`, which regenerates
`third_party/restic/LICENSES.md`; review its diff.

## How the code is laid out

```
crates/
  replica-core/   platform-free: jobs and settings, exclusions, the restic driver, the mirror
                  engine, running a job (steps, SQLite snapshots, results, exit codes), the run
                  history, the strings
  replica-win/    Windows only: settings paths and portable mode, drives and shares, Credential
                  Manager, Task Scheduler, notifications, job objects, logging
  replica/        the two exes: replica.exe (the command line) and replica-tray.exe (the tray)
locales/en/       the strings, in Fluent
docs/             ARCHITECTURE.md, cli.md, cli-json.md, settings.example.toml
third_party/      restic's pin and licences
assets/           the icons, drawn by assets/export.py
scripts/          build-only scripts
```

**`replica-core` must build and test on any OS.** It never calls the OS for secrets, drives,
shares, notifications or process trees: it goes through small traits in
`replica_core::platform`, which `replica-win` implements for Windows. In-memory versions for
tests are in `replica_core::platform::fake`. Windows code goes in `replica-win`.
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains each part and the decisions behind it.

## Strings and languages

Every word the user sees, command-line messages included, comes from the
[Fluent](https://projectfluent.org) files in `locales/<language>/`. `--json` keys are never
translated.

- **Adding a string:** add `key = Text` to the right file in `locales/en/` (`cli.ftl`,
  `tray.ftl`, `errors.ftl`, ...). Keys are lowercase and hyphenated, prefixed by where they show
  (`cli-`, `tray-`, `error-`, ...). Add a comment for any argument whose meaning isn't obvious.
  Then use `tr("key")`, or `tr!("key", name = value)` with arguments. Pass the key as a literal:
  a test checks that every key the code uses exists in English.
- **Adding a language:** create `locales/<tag>/` (such as `locales/fr/`) with any of English's
  files, translating the keys you can. Missing keys fall back to English one by one. Then add it to
  `LOCALES` in `crates/replica-core/src/i18n/embedded.rs`: the tests fail until every file on disk
  is listed there.

## Dependencies: the licence rule

Everything Replica ships must be **MIT or equally permissive**: MIT, Apache-2.0, BSD-2-Clause,
BSD-3-Clause, ISC, Zlib, 0BSD, BSL-1.0, CC0-1.0 or Unicode-3.0 / Unicode-DFS-2016, or a dual
licence that includes one of them. **No copyleft and no extra conditions**: no GPL, LGPL, MPL,
OFL and the like. That's why egui's `default_fonts` feature is off. The one exception is the
bundled, unmodified restic exe, whose licences are recorded in
[third_party/restic/LICENSES.md](third_party/restic/LICENSES.md).

`cargo deny check` enforces this for every crate in the tree (the allowed list is in
`deny.toml`). Before adding a dependency:

- check its licence, and its dependencies' (`cargo deny check` after adding it);
- prefer a well-used, maintained crate, with only the features you need;
- say in the pull request what it's for and why it's worth it.

Build-only tools (the toolchain, CI actions, scripts) don't ship, so the rule doesn't apply to
them, but list a new one in ARCHITECTURE.md's "Build-only tools".

## Tests

- **Tests use temporary folders only** (`tempfile`). A test never reads or writes your real
  settings, drives, shares or backups, and cleans up after itself even when it fails.
- Tests that need Windows' own services use their own names: Credential Manager entries under a
  test prefix (`REPLICA_CREDENTIAL_PREFIX`), and Task Scheduler tasks in a test folder that each
  test deletes.
- A test that starts processes stops them, whatever its outcome.
- A fix comes with a test that fails without it.

## Commits and pull requests

- **A commit message's first line says what changed, in plain English** ("Restores check that the
  item is in the snapshot"), under about 72 characters, with the issue or pull request number at
  the end when there is one ("... (#12)"). Add a body when the why isn't obvious.
- Keep a pull request to one change. Update the docs it affects: [docs/cli.md](docs/cli.md) for
  the command line, [docs/cli-json.md](docs/cli-json.md) for `--json`, the settings example for
  settings, and an `Unreleased` entry in [CHANGELOG.md](CHANGELOG.md) for anything a user would
  notice.
- By contributing, you agree that your contribution is under Replica's [MIT licence](LICENSE).
