<#
.SYNOPSIS
    Fetches the pinned restic release into third_party/restic/bin/restic.exe.
.DESCRIPTION
    Reads third_party/restic/VERSION, downloads that release's Windows amd64 zip
    from restic's GitHub releases, checks its SHA-256 against the pinned one and
    fails on a mismatch, then checks the exe in it against the pinned exe_sha256.
    Build-only: Replica itself never downloads anything.

    A restic.exe that's already there is kept only when its SHA-256 is the pinned
    exe_sha256 (its version alone proves nothing); otherwise it's fetched again.
    -Force always fetches. -VerifyOnly never downloads: it fails unless the exe that's
    there is the pinned one.
#>
[CmdletBinding()]
param([switch]$Force, [switch]$VerifyOnly)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$resticDir = Join-Path $root 'third_party/restic'
$binDir = Join-Path $resticDir 'bin'
$exe = Join-Path $binDir 'restic.exe'

$pin = @{}
foreach ($line in Get-Content (Join-Path $resticDir 'VERSION')) {
    if ($line -match '^\s*([a-z0-9_]+)\s*=\s*(\S+)\s*$') { $pin[$Matches[1]] = $Matches[2] }
}
foreach ($key in 'version', 'asset', 'sha256', 'exe_sha256') {
    if (-not $pin.ContainsKey($key)) { throw "third_party/restic/VERSION has no '$key'." }
}

function Get-Sha256($path) { (Get-FileHash -Algorithm SHA256 -Path $path).Hash.ToLowerInvariant() }

$pinnedExe = $pin.exe_sha256.ToLowerInvariant()
if ((Test-Path $exe) -and -not $Force) {
    $have = Get-Sha256 $exe
    if ($have -eq $pinnedExe) {
        Write-Host "restic $($pin.version) is already at $exe (SHA-256 verified)"
        exit 0
    }
    if ($VerifyOnly) {
        Write-Error "$exe isn't the pinned restic $($pin.version): its SHA-256 is $have, not $pinnedExe."
        exit 1
    }
    Write-Warning "$exe isn't the pinned restic (SHA-256 $have): fetching it again."
}
elseif ($VerifyOnly) {
    Write-Error "There's no restic at ${exe}: run scripts/fetch-restic.ps1."
    exit 1
}

$url = "https://github.com/restic/restic/releases/download/v$($pin.version)/$($pin.asset)"
$work = Join-Path ([IO.Path]::GetTempPath()) ("fetch-restic-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work $pin.asset
    Write-Host "Downloading $url"
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

    $actual = Get-Sha256 $zip
    if ($actual -ne $pin.sha256.ToLowerInvariant()) {
        throw "SHA-256 mismatch for $($pin.asset): expected $($pin.sha256), got $actual."
    }
    Write-Host "SHA-256 verified: $actual"

    $unzipped = Join-Path $work 'unzipped'
    Expand-Archive -Path $zip -DestinationPath $unzipped
    $found = @(Get-ChildItem -Path $unzipped -Filter 'restic*.exe' -Recurse)
    if ($found.Count -ne 1) { throw "Expected one restic exe in $($pin.asset), found $($found.Count)." }
    $inZip = Get-Sha256 $found[0].FullName
    if ($inZip -ne $pinnedExe) {
        throw "The exe in $($pin.asset) has SHA-256 $inZip, not the pinned exe_sha256 $pinnedExe."
    }

    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item -Path $found[0].FullName -Destination $exe -Force
}
finally {
    Remove-Item -Recurse -Force -Path $work -ErrorAction SilentlyContinue
}

$have = & $exe version
if ($have -notmatch "^restic $([regex]::Escape($pin.version)) ") {
    throw "restic.exe reports '$have', not $($pin.version)."
}
Write-Host "Fetched: $have"
