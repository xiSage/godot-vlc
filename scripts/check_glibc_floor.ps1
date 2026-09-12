#!/usr/bin/env pwsh
<#
.SYNOPSIS
Reports, and bounds, the glibc version the built extension requires.

.DESCRIPTION
The extension's shared library is dlopen()ed into the Godot process, so its
glibc requirement must not exceed that of the official Godot Linux binaries.
If it does, Godot starts and the addon silently fails to load — the same silent
failure shape as a missing LibVLC plugin.

The bound lives in build/vlc/glibc-baseline.txt. This script turns it into an
assertion so that building on a newer runner cannot quietly raise the floor.

Measure the official Godot requirement with:
    objdump -T godot | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1

.PARAMETER Path
One or more files or directories to measure. Directories are searched for
shared objects. Pass both the built extension and the assembled runtime: the
LibVLC libraries and plugins are dlopen()ed into the Godot process too, so they
impose the same floor and an unchecked runtime could raise it silently.

.PARAMETER SelfTest
Exercises the parsing against synthetic objdump output and exits.
#>
[CmdletBinding()]
param(
    [string[]]$Path,

    [string]$BaselinePath = 'build/vlc/glibc-baseline.txt',

    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$env:LC_ALL = 'C'

function Get-RequiredGlibcVersions {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$ObjdumpOutput
    )

    $versions = [System.Collections.Generic.HashSet[string]]::new()
    foreach ($line in $ObjdumpOutput) {
        # glibc uses two- and three-component versions (2.34 and the historical
        # 2.2.5), so both shapes have to be captured.
        foreach ($match in [regex]::Matches($line, 'GLIBC_(\d+(?:\.\d+)+)')) {
            [void]$versions.Add($match.Groups[1].Value)
        }
    }

    # Sorted by version, not lexically, so 2.9 does not outrank 2.10.
    @($versions | Sort-Object { [version]$_ })
}

function Get-BaselineVersion {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines)

    foreach ($line in $Lines) {
        if ($line -match '^\s*GLIBC_BASELINE\s*=\s*(\d+\.\d+)\s*$') { return $Matches[1] }
    }
    throw 'GLIBC_BASELINE is not defined in the baseline file'
}

function Invoke-SelfTest {
    $script:failures = 0

    function Assert-Equal {
        param([string]$Description, $Expected, $Actual)
        if ("$Expected" -eq "$Actual") {
            Write-Host "ok   - $Description"
        } else {
            Write-Host "FAIL - $Description (expected '$Expected', got '$Actual')"
            $script:failures++
        }
    }

    $synthetic = @(
        '0000000000000000      DF *UND*	0000000000000000  GLIBC_2.2.5 free'
        '0000000000000000      DF *UND*	0000000000000000  GLIBC_2.34 pthread_create'
        '0000000000000000      DF *UND*	0000000000000000  GLIBC_2.9 weird'
        '0000000000000000      DF *UND*	0000000000000000  GLIBC_2.2.5 memcpy'
        'no glibc symbol here'
    )

    Assert-Equal 'collects distinct versions' '2.2.5 | 2.9 | 2.34' (
        (Get-RequiredGlibcVersions -ObjdumpOutput $synthetic) -join ' | ')
    Assert-Equal 'highest version is version-sorted, not lexical' '2.34' `
        (Get-RequiredGlibcVersions -ObjdumpOutput $synthetic)[-1]
    Assert-Equal 'empty input yields nothing' 0 (Get-RequiredGlibcVersions -ObjdumpOutput @()).Count
    Assert-Equal 'reads the baseline' '2.31' (
        Get-BaselineVersion -Lines @('# comment', 'GLIBC_BASELINE=2.31'))

    if ($script:failures -ne 0) {
        Write-Host "self-test: $($script:failures) case(s) failed" -ForegroundColor Red
        exit 1
    }
    Write-Host 'self-test: all cases passed'
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

$repoRoot = Split-Path -Parent $PSScriptRoot

if (-not $Path -or $Path.Count -eq 0) { throw '-Path is required (files or directories to measure)' }

# Directories are expanded to the shared objects inside them, and a directory
# that yields nothing is an error: a floor check that measures nothing must not
# report success.
$targets = @()
foreach ($item in $Path) {
    $absolute = if ([System.IO.Path]::IsPathRooted($item)) { $item } else { Join-Path $repoRoot $item }
    if (-not (Test-Path -LiteralPath $absolute)) { throw "'$item' not found" }

    if ((Get-Item -LiteralPath $absolute).PSIsContainer) {
        $found = @(
            Get-ChildItem -LiteralPath $absolute -Recurse -Force -File |
                Where-Object { $_.Name -like '*.so' -or $_.Name -like '*.so.*' }
        )
        if ($found.Count -eq 0) { throw "'$item' contains no shared objects" }
        $targets += $found.FullName
    } else {
        $targets += $absolute
    }
}

$baselineAbs = if ([System.IO.Path]::IsPathRooted($BaselinePath)) { $BaselinePath } else { Join-Path $repoRoot $BaselinePath }
if (-not (Test-Path -LiteralPath $baselineAbs)) { throw "'$BaselinePath' not found" }

$highest = $null
$highestSource = $null
foreach ($target in $targets) {
    $required = Get-RequiredGlibcVersions -ObjdumpOutput (& objdump -T $target 2>&1 | ForEach-Object { "$_" })
    if ($required.Count -eq 0) { continue }
    $candidate = $required[-1]
    if ($null -eq $highest -or [version]$candidate -gt [version]$highest) {
        $highest = $candidate
        $highestSource = $target
    }
}

if ($null -eq $highest) { throw "no GLIBC_ version symbols found in any of the $($targets.Count) target(s)" }

$baseline = Get-BaselineVersion -Lines (Get-Content -LiteralPath $baselineAbs)

Write-Host "measured $($targets.Count) shared object(s)"
Write-Host "required glibc: $highest (bound: $baseline)"
Write-Host "  highest requirement comes from $highestSource"

if ([version]$highest -gt [version]$baseline) {
    Write-Host ''
    Write-Host "FAILED: the runtime requires glibc $highest but the supported bound is $baseline." -ForegroundColor Red
    Write-Host 'This raises the minimum distribution users need. Build on an older base, or'
    Write-Host 'raise the bound deliberately and document it in README.md.'
    exit 1
}

Write-Host 'check_glibc_floor: OK' -ForegroundColor Green
