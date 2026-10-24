<#
.SYNOPSIS
    Frees the disk space the workspace's own builds pile up in target\.
.DESCRIPTION
    Cargo never removes old test exes: each round of edits builds about 20 new ones, and the
    incremental caches grow beside them, so a checkout's target\debug reaches tens of GB.

    By default, removes the workspace's own crates' dev and test output (their exes, rlibs,
    stale test exes and incremental caches) with `cargo clean -p <each> --profile dev`. The
    compiled dependencies and target\release are kept, so the next build takes about as long
    as a one-line change.

    -All removes everything cargo built in target\: what `cargo clean` removes, except
    target\ui-shots (screenshots awaiting approval) and target\dist (scripts\package.ps1's
    output), which are kept. The next build is from scratch.

    Prints the size of target\ before and after. Never touches anything outside the checkout's
    target\ (dist\, reference\ and the rest of the checkout are left alone), whatever
    CARGO_TARGET_DIR says. If target is, or holds, a junction or symbolic link, it stops
    before cleaning anything. scripts\test-clean-build.ps1 checks this.
#>
[CmdletBinding()]
param([switch]$All)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
$target = Join-Path $root 'target'
# Not cargo's: kept even by -All. And CACHEDIR.TAG, without which cargo refuses to clean target.
$keep = 'ui-shots', 'dist', 'CACHEDIR.TAG'
# What cargo writes there when it makes target (https://bford.info/cachedir/).
$cacheDirTag = "Signature: 8a477f597d28d172789f06886806bc55`n# This file is a cache directory tag created by cargo.`n# For information about cache directory tags see https://bford.info/cachedir/`n"

function Get-Size($path) {
    $sum = 0
    foreach ($file in Get-ChildItem -LiteralPath $path -Recurse -File -Force -ErrorAction SilentlyContinue) {
        $sum += $file.Length
    }
    $sum
}
function Format-Size($bytes) { '{0:N1} GB' -f ($bytes / 1GB) }

if (-not (Test-Path -LiteralPath $target)) {
    Write-Host "Nothing to clean: $target doesn't exist."
    return
}
# A junction or symbolic link at target or anywhere in it would point the deletes at another
# folder. Cargo never makes one there, so one found means target isn't only cargo's: stop
# before anything is written or removed. (Get-ChildItem -Recurse doesn't follow links, so the
# search stays in target.)
$targetItem = Get-Item -LiteralPath $target -Force
if (-not $targetItem.PSIsContainer) { throw "$target isn't a folder: nothing cleaned." }
$links = @($targetItem | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint })
if (-not $links) { $links = @(Get-ChildItem -LiteralPath $target -Recurse -Force -Attributes ReparsePoint) }
if ($links) {
    $list = ($links | Select-Object -First 5 | ForEach-Object FullName) -join "`n  "
    throw "target holds a junction or link, so cleaning it could delete files outside it: nothing cleaned. Remove it first:`n  $list"
}
$before = Get-Size $target
Write-Host "target: $(Format-Size $before)"

if ($All) {
    foreach ($item in Get-ChildItem -LiteralPath $target -Force) {
        if ($item.Name -in $keep) {
            if ($item.Name -ne 'CACHEDIR.TAG') { Write-Host "Keeping target\$($item.Name)" }
            continue
        }
        Remove-Item -LiteralPath $item.FullName -Recurse -Force
    }
} else {
    $metadata = & cargo metadata --no-deps --format-version 1 --manifest-path $manifest | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed ($LASTEXITCODE)" }
    $packages = @($metadata.packages | ForEach-Object { '-p'; $_.name })
    # Cargo writes the tag only when it makes target, so one an earlier -All or a hand clean
    # left without it gets it back: this target is always the checkout's own.
    $tag = Join-Path $target 'CACHEDIR.TAG'
    if (-not (Test-Path -LiteralPath $tag)) { [IO.File]::WriteAllText($tag, $cacheDirTag) }
    # --target-dir, so a CARGO_TARGET_DIR set elsewhere never points this at another folder.
    & cargo clean --manifest-path $manifest --target-dir $target --profile dev @packages
    if ($LASTEXITCODE -ne 0) { throw "cargo clean failed ($LASTEXITCODE)" }
}

$after = Get-Size $target
Write-Host "target: $(Format-Size $before) -> $(Format-Size $after) ($(Format-Size ($before - $after)) freed)"
