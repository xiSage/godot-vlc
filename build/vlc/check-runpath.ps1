#!/usr/bin/env pwsh
<#
.SYNOPSIS
Validates that every RPATH/RUNPATH entry in `readelf -d` output is relocatable.

.DESCRIPTION
A runtime tree that keeps an absolute runpath is not relocatable: the path baked
in at build time does not exist on a user's machine, so the addon fails to load,
and the failure is easy to miss. Patching those paths by hand is what made the
previous runtime unreproducible, so the rule is checked rather than trusted.

The rule itself lives in lib/runpath.ps1, which postprocess.ps1 also uses, so the
gate and the patcher cannot disagree about what "relocatable" means.

.PARAMETER InputPath
Files containing `readelf -d` output to validate. Omit when running -SelfTest.

.PARAMETER SelfTest
Runs the built-in cases. Needs no runtime and no ELF files, so it runs anywhere.
#>
[CmdletBinding()]
param(
    [string[]]$InputPath,

    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

# binutils translates its own output; pin the locale so the parser sees the
# untranslated form regardless of the caller's environment.
$env:LC_ALL = 'C'

. "$PSScriptRoot/lib/runpath.ps1"

if ($SelfTest) {
    $script:failures = 0
    function Assert-Equal {
        param([string]$Description, $Expected, $Actual)
        if ("$Expected" -eq "$Actual") { Write-Host "ok   - $Description" }
        else { Write-Host "FAIL - $Description (expected '$Expected', got '$Actual')"; $script:failures++ }
    }

    $runpathLine = ' 0x000000000000001d (RUNPATH)            Library runpath: [{0}]'

    Assert-Equal 'reads a $ORIGIN runpath' '$ORIGIN' `
        (Get-RunpathValue -Output @($runpathLine -f '$ORIGIN'))
    Assert-Equal 'reads a two-component runpath' '$ORIGIN/../..:$ORIGIN/../../..' `
        (Get-RunpathValue -Output @($runpathLine -f '$ORIGIN/../..:$ORIGIN/../../..'))
    Assert-Equal 'ignores lines without a path tag' $null `
        (Get-RunpathValue -Output @(' 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]'))
    Assert-Equal 'reads an empty runpath' '' `
        (Get-RunpathValue -Output @($runpathLine -f ''))
    # A file that arrived with DT_RPATH rather than DT_RUNPATH must still be
    # parsed, otherwise an absolute RPATH would slip past the gate entirely.
    $rpathTags = Get-RelativePathTag -Output @(' 0x000000000000000f (RPATH)              Library rpath: [/opt/vlc/lib]')
    Assert-Equal 'parses DT_RPATH as well as RUNPATH' 'RPATH:/opt/vlc/lib' `
        "$($rpathTags[0].Tag):$($rpathTags[0].Value)"
    Assert-Equal 'rejects an absolute DT_RPATH' '/opt/vlc/lib' `
        ((Get-NonRelocatableComponent -Values $rpathTags.Value) -join ',')

    Assert-Equal 'accepts $ORIGIN and its parents' 0 `
        (Get-NonRelocatableComponent -Values @('$ORIGIN', '$ORIGIN/../..:$ORIGIN/../../..')).Count
    Assert-Equal 'rejects an absolute path' '/opt/vlc/lib' `
        ((Get-NonRelocatableComponent -Values @('/opt/vlc/lib')) -join ',')
    Assert-Equal 'rejects an absolute component after $ORIGIN' '/opt/vlc/lib' `
        ((Get-NonRelocatableComponent -Values @('$ORIGIN:/opt/vlc/lib')) -join ',')
    Assert-Equal 'rejects an absolute component before $ORIGIN' '/opt/vlc/lib' `
        ((Get-NonRelocatableComponent -Values @('/opt/vlc/lib:$ORIGIN')) -join ',')
    Assert-Equal 'rejects a bare relative component' '../lib' `
        ((Get-NonRelocatableComponent -Values @('../lib')) -join ',')
    Assert-Equal 'ignores an empty value' 0 (Get-NonRelocatableComponent -Values @('')).Count

    if ($script:failures -ne 0) {
        Write-Host "self-test: $($script:failures) case(s) failed" -ForegroundColor Red
        exit 1
    }
    Write-Host 'self-test: all cases passed'
    exit 0
}

if (-not $InputPath -or $InputPath.Count -eq 0) {
    throw 'give -InputPath with files of readelf output, or -SelfTest'
}

$violations = @()
foreach ($file in $InputPath) {
    if (-not (Test-Path -LiteralPath $file)) { throw "'$file' not found" }
    $tags = Get-RelativePathTag -Output (Get-Content -LiteralPath $file)
    foreach ($bad in Get-NonRelocatableComponent -Values $tags.Value) {
        $violations += "$file : $bad"
    }
}

if ($violations.Count -gt 0) {
    $violations | ForEach-Object { Write-Host "non-relocatable runpath: $_" }
    exit 1
}

Write-Host 'check-runpath: OK'
