<#
.SYNOPSIS
    Builds Replica's release files: the MSI, the portable zip and their SHA-256 checksums.
.DESCRIPTION
    Into target\dist (or -OutDir):
      replica-<version>-x64.msi           per user, no admin (installer\replica.wxs)
      replica-<version>-portable.zip      the exes, restic, the "portable" marker, the licences
      SHA256SUMS.txt                      the checksums of both, as `sha256sum -c` reads them

    Both hold replica.exe, replica-tray.exe, the pinned, checksum-verified restic.exe,
    LICENSE.txt and THIRD-PARTY-NOTICES.txt. It fetches restic unless the copy there is the
    pinned exe (scripts\fetch-restic.ps1, by its SHA-256), checks the copy it packages against
    that pin again, fails if THIRD-PARTY-NOTICES.txt is out of date
    (scripts\third-party-notices.ps1 -Check), and builds the exes with `cargo build --release`.

    The MSI's own text comes from locales\en\installer.ftl, and the built MSI is checked to carry
    it (decompiled with `wix msi decompile`), and to upgrade any earlier install, the same version
    included. Both exes are checked to carry a version resource of the crate's version, as the
    MSI's File table does. The two exes are checked to import no C runtime
    DLL and nothing that isn't part of Windows (the C runtime is linked in: .cargo\config.toml),
    so they start on a clean PC.

    Build-only tools: the Rust toolchain, cargo-about 0.9.2, and WiX 5.0.2
    (dotnet tool install --global wix --version 5.0.2). Nothing is signed yet.
#>
[CmdletBinding()]
param(
    [string]$OutDir,
    # Skips `cargo build --release` (release.yml builds first).
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $root 'target\dist' }
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value

# The MSI's own text, from Fluent: one-line messages without placeables.
function Read-Ftl([string]$path) {
    $messages = @{}
    foreach ($line in Get-Content -Encoding utf8 $path) {
        if ($line -match '^([a-z][a-z0-9-]*)\s*=\s*(.*)$') {
            if ($Matches[2] -match '[{}]' -or -not $Matches[2].Trim()) {
                throw "$path`: $($Matches[1]) must be one line of plain text."
            }
            $messages[$Matches[1]] = $Matches[2].Trim()
        }
        elseif ($line -match '^\s+\S') {
            throw "$path`: messages must be one line each."
        }
    }
    $messages
}
$installerText = Read-Ftl (Join-Path $root 'locales\en\installer.ftl')
foreach ($key in 'installer-downgrade', 'installer-shortcut-description') {
    if (-not $installerText.ContainsKey($key)) { throw "locales\en\installer.ftl has no $key." }
}
# The downgrade message is MSI "formatted" text, where [name] is a property: brackets are escaped.
$downgrade = [regex]::Replace($installerText['installer-downgrade'], '[\[\]]', { "[\$($args[0].Value)]" })
$shortcut = $installerText['installer-shortcut-description']

& (Join-Path $PSScriptRoot 'fetch-restic.ps1')
& (Join-Path $PSScriptRoot 'third-party-notices.ps1') -Check
if (-not $NoBuild) {
    Push-Location $root
    try {
        & cargo build --release --locked --bin replica --bin replica-tray
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }
    finally { Pop-Location }
}

# The DLLs a 64-bit PE file imports, from its import directory.
function Get-ImportedDlls([string]$path) {
    $b = [IO.File]::ReadAllBytes($path)
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    $sections = [BitConverter]::ToUInt16($b, $pe + 6)
    $optional = $pe + 24
    $table = $optional + [BitConverter]::ToUInt16($b, $pe + 20)
    function ToOffset([int]$rva) {
        for ($i = 0; $i -lt $sections; $i++) {
            $s = $table + $i * 40
            $va = [BitConverter]::ToInt32($b, $s + 12)
            $size = [Math]::Max([BitConverter]::ToInt32($b, $s + 8), [BitConverter]::ToInt32($b, $s + 16))
            if ($rva -ge $va -and $rva -lt $va + $size) { return $rva - $va + [BitConverter]::ToInt32($b, $s + 20) }
        }
        throw "RVA $rva isn't in a section of $path"
    }
    $entry = ToOffset ([BitConverter]::ToInt32($b, $optional + 120))
    $dlls = @()
    while (($name = [BitConverter]::ToInt32($b, $entry + 12)) -ne 0) {
        $at = ToOffset $name
        $end = [Array]::IndexOf($b, [byte]0, $at)
        $dlls += [Text.Encoding]::ASCII.GetString($b, $at, $end - $at).ToLowerInvariant()
        $entry += 20
    }
    $dlls
}

# What both packages hold.
$stage = Join-Path $OutDir 'stage'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
$release = Join-Path $root 'target\release'
Copy-Item (Join-Path $release 'replica.exe') $stage
Copy-Item (Join-Path $release 'replica-tray.exe') $stage
# They start on a clean PC: no C runtime DLL (it's linked in), nothing that isn't Windows' own.
$system32 = Join-Path $env:SystemRoot 'System32'
foreach ($exe in 'replica.exe', 'replica-tray.exe') {
    $foreign = Get-ImportedDlls (Join-Path $stage $exe) | Where-Object {
        $_ -match '^(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)' -or
        -not ($_ -match '^(api-ms-win-|ext-ms-)' -or (Test-Path (Join-Path $system32 $_)))
    }
    if ($foreign) { throw "$exe imports $($foreign -join ', '): link the C runtime statically (.cargo\config.toml)." }
}
# They carry a version resource of the crate's version (crates\replica\build.rs): Details shows
# it, and the installer compares it with the installed exe's.
$fileVersion = (@($version -split '[.+-]' | Select-Object -First 3) + '0') -join '.'
foreach ($exe in 'replica.exe', 'replica-tray.exe') {
    $info = (Get-Item (Join-Path $stage $exe)).VersionInfo
    if ($info.FileVersionRaw -ne [version]$fileVersion -or $info.ProductVersion -ne $version -or $info.ProductName -ne 'Replica') {
        throw "$exe's version resource is '$($info.ProductName)' $($info.ProductVersion) ($($info.FileVersionRaw)), not Replica $version ($fileVersion)."
    }
}
Copy-Item (Join-Path $root 'third_party\restic\bin\restic.exe') $stage
# What ships is the official exe, whatever happened to the cached copy since it was fetched.
$pinnedExe = (Select-String -Path (Join-Path $root 'third_party\restic\VERSION') -Pattern '^exe_sha256=([0-9a-f]{64})$').Matches[0].Groups[1].Value
$stagedExe = (Get-FileHash -Algorithm SHA256 (Join-Path $stage 'restic.exe')).Hash.ToLowerInvariant()
if ($stagedExe -ne $pinnedExe) { throw "The restic.exe to package has SHA-256 $stagedExe, not the pinned $pinnedExe." }
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
Copy-Item (Join-Path $root 'THIRD-PARTY-NOTICES.txt') $stage

# The MSI.
$msi = Join-Path $OutDir "replica-$version-x64.msi"
& wix build (Join-Path $root 'installer\replica.wxs') -arch x64 -d "Version=$version" -d "Stage=$stage" `
    -d "DowngradeMessage=$downgrade" -d "ShortcutDescription=$shortcut" -o $msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($msi, '.wixpdb'))

# The built MSI carries the Fluent text, as it is.
$decompiled = Join-Path $OutDir 'msi-check.wxs'
& wix msi decompile $msi -o $decompiled
if ($LASTEXITCODE -ne 0) { throw "wix msi decompile failed ($LASTEXITCODE)" }
$xml = [xml](Get-Content -Raw -Encoding utf8 $decompiled)
Remove-Item -Force $decompiled
$ns = @{ w = 'http://wixtoolset.org/schemas/v4/wxs' }
$inMsi = @{
    'installer-downgrade'            = (Select-Xml -Xml $xml -XPath '//w:MajorUpgrade' -Namespace $ns).Node.DowngradeErrorMessage
    'installer-shortcut-description' = (Select-Xml -Xml $xml -XPath '//w:Shortcut' -Namespace $ns).Node.Description
}
$expected = @{ 'installer-downgrade' = $downgrade; 'installer-shortcut-description' = $shortcut }
foreach ($key in $expected.Keys) {
    if ($inMsi[$key] -ne $expected[$key]) {
        throw "The MSI's $key is '$($inMsi[$key])', not installer.ftl's '$($expected[$key])'."
    }
}
# Its tables, read with Windows Installer's own API (opened read-only: nothing is installed).
# `wix msi decompile` 5.0.2 gives MajorUpgrade's Schedule as afterInstallFinalize whatever it is,
# so the upgrade is checked here rather than in the decompiled source.
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @([string]$msi, [int]0))
# Each row of a query, as an array of its fields' text.
function Get-MsiRows([string]$query) {
    $view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database, @($query))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    while ($record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)) {
        $count = $record.GetType().InvokeMember('FieldCount', 'GetProperty', $null, $record, $null)
        , @(1..$count | ForEach-Object { $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, $_) })
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
}
try {
    # A same-version MSI upgrades too (AllowSameVersionUpgrades: the upgrade row's VersionMax is
    # this version, inclusive), else a rebuild installs beside the old one and keeps its exes.
    $upgrade = @(Get-MsiRows "SELECT ``VersionMax``, ``Attributes`` FROM ``Upgrade`` WHERE ``ActionProperty`` = 'WIX_UPGRADE_DETECTED'")
    $inclusive = 0x200  # msidbUpgradeAttributesVersionMaxInclusive
    if ($upgrade.Count -ne 1 -or $upgrade[0][0] -ne $version -or -not ([int]$upgrade[0][1] -band $inclusive)) {
        throw "The MSI doesn't upgrade an install of its own version (AllowSameVersionUpgrades): its Upgrade row is '$($upgrade | ForEach-Object { $_ -join ', ' })'."
    }
    # Every earlier install is removed before this one installs (Schedule="afterInstallInitialize").
    $sequence = @{}
    Get-MsiRows "SELECT ``Action``, ``Sequence`` FROM ``InstallExecuteSequence``" | ForEach-Object { $sequence[$_[0]] = [int]$_[1] }
    $removeAt = $sequence['RemoveExistingProducts']
    $between = $sequence.Values | Where-Object { $_ -gt $sequence['InstallInitialize'] -and $_ -lt $removeAt }
    if (-not $removeAt -or $removeAt -le $sequence['InstallInitialize'] -or $between) {
        throw "The MSI's RemoveExistingProducts is at $removeAt, not just after InstallInitialize ($($sequence['InstallInitialize']))."
    }
    # Its File table has both exes' versions, which the installer compares on an upgrade.
    foreach ($file in 'ReplicaExe', 'ReplicaTrayExe') {
        $inTable = @(Get-MsiRows "SELECT ``Version`` FROM ``File`` WHERE ``File`` = '$file'")
        if ($inTable.Count -ne 1 -or $inTable[0][0] -ne $fileVersion) {
            throw "The MSI's $file has version '$($inTable | ForEach-Object { $_ })', not $fileVersion."
        }
    }
}
finally {
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($database)
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($installer)
}

# The portable zip: the same files and the empty marker that keeps everything beside the exes.
$portable = Join-Path $OutDir 'portable'
if (Test-Path $portable) { Remove-Item -Recurse -Force $portable }
Copy-Item -Recurse $stage $portable
New-Item -ItemType File -Path (Join-Path $portable 'portable') | Out-Null
$zip = Join-Path $OutDir "replica-$version-portable.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $portable '*') -DestinationPath $zip
Remove-Item -Recurse -Force $portable

# The checksums, as `sha256sum -c SHA256SUMS.txt` reads them.
$sums = foreach ($file in $msi, $zip) {
    '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant(), (Split-Path -Leaf $file)
}
[IO.File]::WriteAllText((Join-Path $OutDir 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"))

Write-Host "Built in ${OutDir}:"
Get-ChildItem $OutDir -File | ForEach-Object { Write-Host ("  {0} ({1:N0} bytes)" -f $_.Name, $_.Length) }
