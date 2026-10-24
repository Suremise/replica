# Releasing Replica

A release is two files and their checksums, built by `release.yml` on a version tag and
published by hand after the manual checklist below:

| File | What |
|---|---|
| `replica-<version>-x64.msi` | Installs per user, with no admin rights, to `%LOCALAPPDATA%\Programs\Replica`, and adds `replica.exe` to the user's PATH |
| `replica-<version>-portable.zip` | The same files and an empty `portable` marker: unzip anywhere, and everything is kept beside the exes |
| `SHA256SUMS.txt` | The SHA-256 of both, as `sha256sum -c` reads them; users compare their download with it |

Both hold `replica.exe`, `replica-tray.exe`, the pinned, checksum-verified `restic.exe`,
`LICENSE.txt` and `THIRD-PARTY-NOTICES.txt`. Nothing is signed yet, so Windows SmartScreen warns
about both (see the checklist).

## Steps

1. **Update what changed.**
   - If the restic pin moved: set `version`, `asset` and `sha256` (from the release's
     SHA256SUMS) in `third_party/restic/VERSION`, and `exe_sha256` to the SHA-256 of the exe in
     that zip (the fetch fails until it matches). Then `scripts/fetch-restic.ps1 -Force` and
     `python -I scripts/audit-restic-licences.py`. Review the diff of
     `third_party/restic/LICENSES.md` and `NOTICES.txt`: a licence outside the permissive set and
     MPL-2.0 makes the script fail, and needs the maintainer's decision.
   - Regenerate the notices: `scripts/third-party-notices.ps1` (needs cargo-about 0.9.2, below).
     Commit `THIRD-PARTY-NOTICES.txt` if it changed. The release fails if it's out of date.
2. **Set the version** in `Cargo.toml` (`[workspace.package] version`), and run `cargo check` so
   `Cargo.lock` follows.
3. **The changelog:** rename `## [Unreleased]` in `CHANGELOG.md` to `## [<version>] - <date>`,
   and start a new, empty `## [Unreleased]` above it. The release notes are that section.
4. **Build locally and check** (optional, but it's what the workflow does):
   `scripts/package.ps1` writes `target\dist\`. Look inside the zip, and decompile the MSI
   (`wix msi decompile target\dist\replica-<version>-x64.msi -o x.wxs`) rather than installing it
   on a machine that matters. Once the build is checked, run `scripts/clean-build.ps1` in the
   project folder: it removes the workspace crates' dev and test builds and keeps
   `target\release`, which speeds up the next dist build (docs/TESTING.md, Disk space).
5. **Commit, tag and push:** `git tag v<version>` and push the tag. `release.yml` checks the tag
   matches the version, runs `cargo deny check` and the tests (with restic required), builds
   both files and the checksums, and **drafts** a GitHub release with them and the changelog
   section.
6. **Run the manual checklist** below on the draft's files, on a test machine or account.
7. **Publish the draft** once the checklist passes.

### Build-only tools

| Tool | Install |
|---|---|
| Rust 1.98.1 | `rust-toolchain.toml` (rustup) |
| cargo-deny 0.20 | `cargo install cargo-deny --locked` |
| cargo-about 0.9.2 | `cargo install cargo-about --version 0.9.2 --locked --features cli` |
| cargo-nextest 0.9.149 (Apache-2.0 or MIT) | `cargo install cargo-nextest --version 0.9.149 --locked`: runs the tests (docs/TESTING.md); `cargo test` works without it |
| WiX Toolset 5.0.2 | `dotnet tool install --global wix --version 5.0.2` (needs the .NET 8 SDK) |
| actionlint (optional) | <https://github.com/rhysd/actionlint/releases>, to check the workflows |
| MSVC build tools and Windows SDK | Visual Studio's C++ build tools: the linker, `rc.exe`, and the C runtime's static libraries |

**The C runtime is linked into the exes** (`.cargo/config.toml`, `+crt-static`), so they need no
Visual C++ redistributable, which would need an admin install. The parts of Microsoft's C runtime
linked in are "Distributable Code" under the Visual Studio licence, which allows shipping them in
an app; they need no entry in THIRD-PARTY-NOTICES. `package.ps1` checks both exes import no C
runtime DLL and nothing that isn't part of Windows.

**WiX 5, not cargo-wix or WiX 6+:** `cargo-wix` drives WiX 3, which needs the .NET Framework 3.5
(an admin install); WiX 6 and later come with the Open Source Maintenance Fee terms. WiX 5 is
MS-RL and only builds the MSI: none of it ships in it. The MSI uses **no WiX extension and no
WixUI**, whose dialogs, bitmaps and custom-action DLLs (MS-RL) would be embedded in it;
Windows shows its own progress window instead.

## The MSI

`installer/replica.wxs`:

- **Per user, no admin** (`Scope="perUser"`), to `%LOCALAPPDATA%\Programs\Replica`; components
  keep their key paths under `HKCU\Software\Replica\Installer`.
- **Its own text** (the downgrade message and the shortcut's description) is in
  `locales/en/installer.ftl`; `package.ps1` passes it in and checks the built MSI carries it.
- **The restic it ships** is checked against `exe_sha256` in `third_party/restic/VERSION` when
  it's packaged, whatever the cached copy says its version is.
- **The user's PATH** gets the install folder (removed on uninstall).
- **A Start menu shortcut** to the tray app. **After an interactive install or upgrade the tray
  starts with its window open** (`replica-tray.exe --show`), so the user sees it worked; a silent
  or basic-UI one (`/qn`, `/qb`: `UILevel` 3 or less) starts nothing.
- **Start with Windows** is the public property `STARTWITHWINDOWS`, on by default
  (`msiexec /i replica-<version>-x64.msi STARTWITHWINDOWS=0` turns it off). The MSI records the
  choice; the tray takes it into its own setting on its first start, when there are no settings
  yet. After that it's the tray menu's and the Settings tab's.
- **Upgrades** are major upgrades (same `UpgradeCode`), **the same version included**
  (`AllowSameVersionUpgrades`): every build has a new `ProductCode`, so without it a rebuilt MSI
  of the same version installs beside the old one, and the installer keeps the old exes (equal
  file versions). The old installs are removed first (`Schedule="afterInstallInitialize"`), all
  of them. WiX's ICE61 warning about same-version upgrades is expected. A running tray is asked
  to quit first (`replica-tray.exe --quit`), so its exe isn't in use. `package.ps1` checks the
  built MSI has both settings, and that both exes carry a version resource of the crate's
  version (`crates/replica/build.rs`).
- **Uninstalling** runs `replica-tray.exe --uninstall`: the tray quits, and Replica's scheduled
  tasks and its Start with Windows entry are removed. **Settings, history, logs and backups are
  never removed**: they're in `%APPDATA%\Replica` and on the backup targets.

## Manual checklist

Copy this for each release, fill it in and keep it with the release (in its notes, or
attached to them).

**Release:** v______ **Date:** ________ **Machine and Windows version:** ______________________
**Tester:** ________

| Done | Check | Result and notes |
|:---:|---|---|
| [ ] | **A NAS over UNC with credentials:** a versioned job to `\\nas\share\folder` with a login saved in the editor; it runs, and `replica snapshots` lists the snapshot | |
| [ ] | **An external drive on a different letter:** back up to it, unplug it, give it another letter (Disk Management) and plug it in; the next run finds it by volume ID | |
| [ ] | **Task Scheduler:** turn the option on in Settings; the tasks appear in Task Scheduler's "Replica" folder; a scheduled run happens with **no console window flashing**; turn it off and the tasks go | |
| [ ] | **Another app's run-on-close hook:** `replica run <job> --quiet` from it runs the job and prints nothing on success | |
| [ ] | **No console windows:** run a versioned job with a `command` step from the tray and from the window, do a restore, a test restore and a verify; **no console window flashes** at any point, and the step's output is in the run's log | |
| [ ] | **A documented test restore:** restore a file and a folder from the window and with `replica restore`, and run "Test restore", as the user guide says | |
| [ ] | **The repository password:** a new versioned target shows its password once, and the job isn't saved without "I've saved a copy" | |
| [ ] | **MSI install without admin:** double-click it as a standard user; no UAC prompt; the Start menu shortcut, the tray and `replica --version` in a new terminal (PATH) work | |
| [ ] | **MSI uninstall without admin:** from Settings > Apps; the tray quits; Replica's tasks and Start with Windows entry go; **settings, history and backups stay** | |
| [ ] | **MSI upgrade:** install the previous version, then this one, with the tray running; it quits, upgrades and starts again with its window open; settings are kept; Apps & features shows **one** Replica, and the installed exes' Properties > Details show the new version | |
| [ ] | **MSI over the same version:** install this MSI over itself (or a rebuild of the same version); Apps & features still shows **one** Replica, and the exes' Details show this version | |
| [ ] | **Silent install:** `msiexec /i replica-<version>-x64.msi /qn` installs without a window and **doesn't start the app** | |
| [ ] | **Portable from a USB stick:** unzip to a stick and run it there; settings, history and logs are kept on the stick | |
| [ ] | **Portable from a read-only folder:** it says it can't save, and nothing is written anywhere else | |
| [ ] | **A clean Windows VM** (no Visual C++ redistributable installed): the portable copy starts, and so does the MSI's | |
| [ ] | **SmartScreen's unsigned warning:** on downloading and running both files; note what a user has to click | |
| [ ] | **Checksums:** `SHA256SUMS.txt` matches both downloaded files | |
