<#
.SYNOPSIS
    Writes THIRD-PARTY-NOTICES.txt: every crate in Replica's exes, SQLite, and restic.
.DESCRIPTION
    Part 1 is the crates of the Windows build (cargo-about, with about.toml and about.hbs),
    without dev or build dependencies, then the copyright notices of the crates whose MIT text
    there is the licence's template (see Get-CopyrightNotices). SQLite is compiled in by rusqlite's "bundled" feature and
    isn't a crate, so it gets a note of its own. Part 2 is restic's, third_party/restic/NOTICES.txt,
    made by scripts/audit-restic-licences.py when the restic pin moves.

    The file is committed, so the README can link it and every build's About tab carries it.
    -Check writes it to a temp file and fails if the committed one differs (release.yml runs that).
    Build-only: it needs cargo-about 0.9.2 (cargo install cargo-about --version 0.9.2 --locked
    --features cli) and makes no network requests.
#>
[CmdletBinding()]
param([switch]$Check)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Where a crate ships no filled-in MIT licence file, cargo-about gives the licence's template
# ("Copyright (c) <year> <copyright holders>"), and the crate's copyright notices are left out.
# This collects them from the crate's own files, as built (its licence files' "Copyright" lines
# and its SPDX-FileCopyrightText headers), for every crate under such a template, into a
# section of their own: offline, and in a fixed order, so -Check stays reproducible.
function Get-CopyrightNotices([string]$crateText, [string]$root) {
    $rule = '-' * 80
    $crates = [System.Collections.Generic.List[string]]::new()
    foreach ($block in ($crateText -split "(?m)^-{80}$")) {
        if ($block -notmatch 'Copyright \(c\) <year> <copyright holders>') { continue }
        foreach ($m in [regex]::Matches($block, '(?m)^  - (\S+) (\S+) ')) {
            $crates.Add("$($m.Groups[1].Value) $($m.Groups[2].Value)")
        }
    }
    if ($crates.Count -eq 0) { return '' }

    Push-Location $root
    # cargo writes UTF-8; read it as such whatever the console's code page (authors' names
    # aren't all ASCII), so the output is the same however the script is started.
    $encoding = [Console]::OutputEncoding
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    try {
        $metadata = & cargo metadata --format-version 1 --offline --locked | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed ($LASTEXITCODE)" }
    }
    finally {
        [Console]::OutputEncoding = $encoding
        Pop-Location
    }
    $dirs = @{}
    $authors = @{}
    foreach ($p in $metadata.packages) {
        $dirs["$($p.name) $($p.version)"] = Split-Path -Parent $p.manifest_path
        $authors["$($p.name) $($p.version)"] = @($p.authors)
    }

    $lines = @(
        $rule,
        'Copyright notices of the crates above whose licence text is a template',
        '',
        'These crates'' MIT licence above is the licence''s template ("<year> <copyright holders>"):',
        'their copyright notices are in their own files, collected here from the crates as built',
        '(their licence files, and the SPDX-FileCopyrightText headers of their files).',
        ''
    )
    foreach ($crate in ($crates | Sort-Object -Unique -CaseSensitive)) {
        $dir = $dirs[$crate]
        if (-not $dir) { throw "no source for $crate in cargo metadata" }
        $found = [System.Collections.Generic.SortedSet[string]]::new([StringComparer]::Ordinal)
        # Standard licence texts give their "Copyright" lines; any other notice file (such as a
        # LICENSE-LIBM-MIT for code taken from elsewhere) is given whole, below.
        $licenceFiles = @(Get-ChildItem -Path $dir -File | Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE)' } | Sort-Object Name)
        $licenceDir = Join-Path $dir 'LICENSES'
        if (Test-Path $licenceDir) { $licenceFiles += Get-ChildItem -Path $licenceDir -File | Sort-Object Name }
        $whole = [System.Collections.Generic.List[object]]::new()
        foreach ($f in $licenceFiles) {
            $standard = $f.DirectoryName -eq $licenceDir -or $f.Name -match '^LICEN[CS]E(\.(md|txt))?$|^LICEN[CS]E[-_](MIT|APACHE)'
            if (-not $standard) { $whole.Add($f); continue }
            foreach ($line in (Get-Content -Encoding utf8 $f.FullName)) {
                $t = $line.Trim()
                # A notice, with its year or (c); not a template's placeholder.
                if ($t -match '^Copyright\s+(\(c\)|©|\d)' -and $t -notmatch '[<\[{]') { [void]$found.Add($t) }
            }
        }
        $sources = Get-ChildItem -Path $dir -Recurse -File -Include *.rs, *.toml, *.md
        foreach ($m in ($sources | Select-String -Pattern 'SPDX-FileCopyrightText:\s*(.+?)\s*$' -Encoding utf8)) {
            $holder = $m.Matches[0].Groups[1].Value
            if ($holder -notmatch '^(Copyright|©)') { $holder = "Copyright $holder" }
            [void]$found.Add($holder)
        }
        $lines += $crate
        if ($found.Count -gt 0) {
            $lines += @($found | ForEach-Object { "  $_" })
        }
        foreach ($f in $whole) {
            $lines += "  $($f.Name):"
            $lines += @((Get-Content -Encoding utf8 $f.FullName) | ForEach-Object { "    $_".TrimEnd() })
        }
        if ($found.Count -eq 0 -and $whole.Count -eq 0) {
            $names = @($authors[$crate] | Where-Object { $_ })
            if ($names.Count -gt 0) {
                $lines += "  No copyright notice in its files. Its authors, as its Cargo.toml names them: $($names -join ', ')"
            }
            else {
                $lines += '  No copyright notice in its files, and its Cargo.toml names no authors.'
            }
        }
        $lines += ''
    }
    return ($lines -join "`n")
}

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'THIRD-PARTY-NOTICES.txt'
$work = Join-Path ([IO.Path]::GetTempPath()) ("notices-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $crates = Join-Path $work 'crates.txt'
    Push-Location $root
    try {
        & cargo about generate --workspace --offline --fail about.hbs -o $crates
        if ($LASTEXITCODE -ne 0) { throw "cargo about failed ($LASTEXITCODE)" }
    }
    finally { Pop-Location }

    $version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
    $rule = '=' * 80
    $header = @(
        "THIRD-PARTY NOTICES FOR REPLICA $version",
        '',
        'Replica is under the MIT licence (LICENSE, beside this file). It is built from the',
        'third-party Rust crates in part 1 and compiles in SQLite, and it ships the unmodified restic',
        'exe in part 2. Their licences and notices follow.',
        '',
        $rule
    ) -join "`n"
    $sqlite = @(
        $rule,
        'SQLITE',
        '',
        'SQLite is compiled into Replica (through the libsqlite3-sys crate''s "bundled" feature).',
        'Its authors have dedicated it to the public domain: https://www.sqlite.org/copyright.html',
        '',
        $rule
    ) -join "`n"
    $restic = Get-Content -Raw -Encoding utf8 (Join-Path $root 'third_party/restic/NOTICES.txt')
    $crateText = (Get-Content -Raw -Encoding utf8 $crates) -replace "`r`n", "`n"
    $copyrights = Get-CopyrightNotices $crateText $root
    $text = ($header, $crateText.TrimEnd(), '', $copyrights, $sqlite, $restic.TrimEnd()) -join "`n"
    $text = ($text -replace "`r`n", "`n") + "`n"

    if ($Check) {
        $committed = if (Test-Path $out) { (Get-Content -Raw -Encoding utf8 $out) -replace "`r`n", "`n" } else { '' }
        if ($committed -ne $text) {
            Write-Error 'THIRD-PARTY-NOTICES.txt is out of date: run scripts/third-party-notices.ps1 and commit it.'
            exit 1
        }
        Write-Host 'THIRD-PARTY-NOTICES.txt is up to date.'
    }
    else {
        [IO.File]::WriteAllText($out, $text, [Text.UTF8Encoding]::new($false))
        Write-Host "Wrote $out ($((Get-Item $out).Length) bytes)"
    }
}
finally {
    Remove-Item -Recurse -Force $work
}
