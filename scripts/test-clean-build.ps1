<#
.SYNOPSIS
    Checks scripts\clean-build.ps1 in a throwaway workspace.
.DESCRIPTION
    Copies clean-build.ps1 into a temp folder laid out like a checkout: a two-crate workspace,
    built once, with dist\, reference\, target\ui-shots, target\dist, target\release, a
    dependency's rlib, a folder beside the checkout and a CARGO_TARGET_DIR elsewhere. Runs the
    script, then the script with -All, and checks that each removed what it should and nothing
    else. Throws on the first failure; removes the temp folder either way. Needs only cargo.

        pwsh scripts/test-clean-build.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$temp = Join-Path ([IO.Path]::GetTempPath()) "replica-clean-build-test-$PID-$(Get-Random)"
$worktree = Join-Path $temp 'worktree'
$target = Join-Path $worktree 'target'
$failures = 0

function New-File($path, $text = 'keep') {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    Set-Content -LiteralPath $path -Value $text
}
function Check($ok, $what) {
    if ($ok) { Write-Host "ok    $what" } else { Write-Host "FAIL  $what"; $script:failures++ }
}
function Find($dir, $filter) {
    if (-not (Test-Path -LiteralPath $dir)) { return @() }
    @(Get-ChildItem -LiteralPath $dir -Filter $filter -Force)
}

$savedTargetDir = $env:CARGO_TARGET_DIR
try {
    # A workspace of two crates, one with an integration test, as replica's are.
    New-File (Join-Path $worktree 'Cargo.toml') "[workspace]`nresolver = `"3`"`nmembers = [`"crates/fake-core`", `"crates/fake-app`"]"
    foreach ($crate in 'fake-core', 'fake-app') {
        New-File (Join-Path $worktree "crates/$crate/Cargo.toml") "[package]`nname = `"$crate`"`nversion = `"0.1.0`"`nedition = `"2024`""
        New-File (Join-Path $worktree "crates/$crate/src/lib.rs") "pub fn one() -> u32 { 1 }"
    }
    New-File (Join-Path $worktree 'crates/fake-app/tests/smoke.rs') "#[test] fn smoke() { assert_eq!(fake_app::one(), 1); }"
    New-Item -ItemType Directory -Force -Path (Join-Path $worktree 'scripts') | Out-Null
    Copy-Item (Join-Path $PSScriptRoot 'clean-build.ps1') (Join-Path $worktree 'scripts')

    $env:CARGO_TARGET_DIR = $null
    & cargo build --quiet --tests --manifest-path (Join-Path $worktree 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    $deps = Join-Path $target 'debug/deps'
    if (-not (Find $deps 'smoke-*.exe')) { throw "the throwaway workspace's build made no test exe" }
    # A stale test exe from an earlier round, beside the current one.
    New-File (Join-Path $deps 'smoke-0123456789abcdef.exe') 'stale'

    $kept = @(
        (Join-Path $worktree 'dist/replica.exe'),
        (Join-Path $worktree 'reference/example.toml'),
        (Join-Path $target 'ui-shots/217/window.png'),
        (Join-Path $target 'dist/replica-0.1.0-x64.zip'),
        (Join-Path $temp 'beside/keep.txt')
    )
    $keptByDefault = @(
        (Join-Path $deps 'libserde-0123456789abcdef.rlib'),
        (Join-Path $target 'release/replica.exe')
    )
    foreach ($file in $kept + $keptByDefault) { New-File $file }
    # Another target folder, which the script must not follow.
    $elsewhere = Join-Path $temp 'elsewhere'
    New-File (Join-Path $elsewhere 'debug/deps/libfake_core-0123456789abcdef.rlib')
    $env:CARGO_TARGET_DIR = $elsewhere

    & pwsh -NoProfile -File (Join-Path $worktree 'scripts/clean-build.ps1')
    if ($LASTEXITCODE -ne 0) { throw "clean-build.ps1 failed ($LASTEXITCODE)" }
    Check (-not (Find $deps 'smoke-*')) 'the default removes the test exes, the stale one too'
    Check (-not (Find $deps 'fake_core-*') -and -not (Find $deps 'libfake_app-*')) 'the default removes the crates'' own builds'
    Check (-not (Find (Join-Path $target 'debug/incremental') 'fake*')) 'the default removes their incremental caches'
    foreach ($file in $kept + $keptByDefault) { Check (Test-Path -LiteralPath $file) "the default keeps $($file.Substring($temp.Length + 1))" }
    Check (Find (Join-Path $elsewhere 'debug/deps') 'libfake_core-*') 'the default ignores CARGO_TARGET_DIR'

    & pwsh -NoProfile -File (Join-Path $worktree 'scripts/clean-build.ps1') -All
    if ($LASTEXITCODE -ne 0) { throw "clean-build.ps1 -All failed ($LASTEXITCODE)" }
    $left = @(Get-ChildItem -LiteralPath $target -Force | ForEach-Object Name | Sort-Object)
    Check (($left -join ',') -eq 'CACHEDIR.TAG,dist,ui-shots') "-All leaves only target\dist, target\ui-shots and cargo's CACHEDIR.TAG (left: $($left -join ', '))"
    foreach ($file in $kept) { Check (Test-Path -LiteralPath $file) "-All keeps $($file.Substring($temp.Length + 1))" }
    Check (Find (Join-Path $elsewhere 'debug/deps') 'libfake_core-*') '-All ignores CARGO_TARGET_DIR'

    & pwsh -NoProfile -File (Join-Path $worktree 'scripts/clean-build.ps1')
    Check ($LASTEXITCODE -eq 0) 'the default runs after -All (cargo still finds its CACHEDIR.TAG)'

    # A target with no files left in it, its tag gone too, as a hand clean can leave it.
    Get-ChildItem -LiteralPath $target -Force | Remove-Item -Recurse -Force
    & pwsh -NoProfile -File (Join-Path $worktree 'scripts/clean-build.ps1') -All
    Check ($LASTEXITCODE -eq 0) '-All cleans an empty target without an error'
    & pwsh -NoProfile -File (Join-Path $worktree 'scripts/clean-build.ps1')
    Check ($LASTEXITCODE -eq 0) 'the default cleans a target without CACHEDIR.TAG'

    # A junction at target, or anywhere in it, to a folder outside: neither mode follows it.
    $outside = Join-Path $temp 'outside'
    $precious = @(
        (Join-Path $outside 'precious.txt'),
        (Join-Path $outside 'debug/deps/libfake_core-0123456789abcdef.rlib'),
        (Join-Path $outside 'debug/deps/smoke-0123456789abcdef.exe')
    )
    foreach ($file in $precious) { New-File $file }
    Remove-Item -LiteralPath $target -Recurse -Force
    $cases = [ordered]@{
        'target is a junction' = $target
        'target\debug is a junction' = Join-Path $target 'debug'
        'a junction deep in target' = Join-Path $target 'debug/deps/linked'
    }
    foreach ($case in $cases.GetEnumerator()) {
        $link = $case.Value
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $link) | Out-Null
        New-Item -ItemType Junction -Path $link -Target $outside | Out-Null
        foreach ($mode in @(), @('-All')) {
            $name = if ($mode) { '-All' } else { 'the default' }
            & pwsh -NoProfile -File (Join-Path $worktree 'scripts/clean-build.ps1') @mode *> $null
            Check ($LASTEXITCODE -ne 0) "$($case.Key): $name stops with an error"
            Check (-not ($precious | Where-Object { -not (Test-Path -LiteralPath $_) })) "$($case.Key): $name deletes nothing outside target"
            Check (-not (Test-Path -LiteralPath (Join-Path $outside 'CACHEDIR.TAG'))) "$($case.Key): $name writes nothing outside target"
        }
        # Removes the junction itself, never what it points at.
        [IO.Directory]::Delete($link)
        if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
    }
} finally {
    $env:CARGO_TARGET_DIR = $savedTargetDir
    Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures) { throw "$failures check(s) failed" }
Write-Host 'All checks passed.'
