#!/usr/bin/env pwsh
<#
.SYNOPSIS
Checks the media loader's extension list against the VLC revision the runtime is built from.

.DESCRIPTION
src/vlc_media_format_loader.rs carries the extensions a file may have to become a
VLCMedia resource. That list is VLC's own — the EXTENSIONS_AUDIO and EXTENSIONS_VIDEO
globs in include/vlc_interface.h, which is the list VLC's open dialogs are built from —
concatenated and de-duplicated, in the order they are declared there. It cannot be read
from VLC at runtime: the shortcuts a module declares live in module_t, which the public
headers keep opaque, and a resource loader has to answer before any file is opened. So the
list is a copy, and this script is what keeps the copy honest.

The revision comes from build/vlc/vlc.lock, the same pin the runtime is built from, and
the header is read from GitHub — with the VideoLAN GitLab as a second host — unless
-VlcSource names a tree that is already on disk. Only that one header is fetched: the
release archive is about 37 MB and this needs ten kilobytes of it.

CI runs -SelfTest alone, because no CI job has a VLC source tree; the check against the
real header is run by hand, and by whoever regenerates the list.

.PARAMETER VlcSource
A VLC source tree at the pinned revision, holding include/vlc_interface.h. Skips the
download.

.PARAMETER VlcCommit
The revision to read the header from. Defaults to VLC_COMMIT in build/vlc/vlc.lock, which
may be an abbreviation — the hosts this reads from resolve one.

.PARAMETER LockPath
The file VLC_COMMIT is read from.

.PARAMETER LoaderSource
The Rust file whose DEFAULT_EXTENSIONS list is checked.

.PARAMETER HeaderPath
The header to read, relative to -VlcSource or to the repository root of the download.

.PARAMETER SelfTest
Exercises the parsing and the comparison against synthetic input and exits. No network.

.EXAMPLE
pwsh scripts/check_media_extensions.ps1

.EXAMPLE
pwsh scripts/check_media_extensions.ps1 -VlcSource D:\src\vlc
#>
[CmdletBinding()]
param(
    [string]$VlcSource,

    [string]$VlcCommit,

    [string]$LockPath = 'build/vlc/vlc.lock',

    [string]$LoaderSource = 'src/vlc_media_format_loader.rs',

    [string]$HeaderPath = 'include/vlc_interface.h',

    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

function Read-LockValue {
    param(
        [Parameter(Mandatory)][string]$Lock,
        [Parameter(Mandatory)][string]$Key
    )

    foreach ($line in Get-Content -LiteralPath $Lock) {
        if ($line -match "^\s*$Key\s*=\s*(.+?)\s*$") { return $Matches[1] }
    }
    throw "$Lock has no $Key"
}

function Get-GlobExtensions {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string]$Text,
        [Parameter(Mandatory)][string]$Name
    )

    # Up to the next #define, which is where each of these macros ends. A name that is a
    # prefix of another one does not match: `#define EXTENSIONS_VIDEO_CSV` has `_CSV`
    # where the whitespace this wants is, and that macro is dead anyway.
    $match = [regex]::Match($Text, "(?s)#define $Name\s+(?<body>.*?)(?=\r?\n#define )")
    if (-not $match.Success) { throw "no '#define $Name' in the header" }

    # Quotes, backslashes and whitespace are all gone before the split, because the audio
    # macro writes one entry per quoted string while the video macro packs many entries
    # into a single one. Reading the quoted strings alone would find one extension per
    # string and silently drop the rest of the video list.
    $body = $match.Groups['body'].Value -replace '["\\\s]', ''

    $extensions = @()
    foreach ($entry in $body.Trim(';').Split(';')) {
        $extension = $entry -replace '^\*\.', ''
        if ($extension -ne '') { $extensions += $extension.ToLowerInvariant() }
    }
    $extensions
}

function Get-LoaderExtensions {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Source)

    $match = [regex]::Match($Source, '(?s)const DEFAULT_EXTENSIONS: &\[&str\] = &\[(?<body>.*?)\];')
    if (-not $match.Success) { throw 'the loader has no DEFAULT_EXTENSIONS list' }

    $extensions = @()
    foreach ($quoted in [regex]::Matches($match.Groups['body'].Value, '"([^"]*)"')) {
        $extensions += $quoted.Groups[1].Value
    }
    $extensions
}

function Compare-ExtensionLists {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Vlc,
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Loader
    )

    $problems = @()
    $missing = @($Vlc | Where-Object { $Loader -notcontains $_ })
    $extra = @($Loader | Where-Object { $Vlc -notcontains $_ })
    if ($missing.Count -gt 0) { $problems += "VLC declares but the loader does not offer: $($missing -join ', ')" }
    if ($extra.Count -gt 0) { $problems += "the loader offers but VLC does not declare: $($extra -join ', ')" }
    # The order is VLC's own, so that regenerating the list is a mechanical edit rather
    # than a judgement call; the same entries shuffled is a difference worth reporting.
    if ($problems.Count -eq 0 -and ($Vlc -join ',') -ne ($Loader -join ',')) {
        $problems += 'the same extensions in a different order than the header declares them'
    }
    $problems
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

    # Both shapes the real header uses: one entry per quoted string, and many entries
    # inside one string. `ogg` is in both macros, and the last entry of the video macro
    # has no trailing semicolon, as in the real header.
    $syntheticHeader = @'
#define EXTENSIONS_AUDIO \
    "*.3ga;" \
    "*.aac;" \
    "*.ogg;" \
    "*.wav;"

#define EXTENSIONS_VIDEO "*.3g2;*.3gp;*.asf;" \
                         "*.avi;*.ogg;" \
                         "*.mp4;*.mkv"

#define EXTENSIONS_MEDIA EXTENSIONS_VIDEO ";" EXTENSIONS_AUDIO
'@

    $syntheticLoader = @'
/// A list, with a doc comment that mentions `["quotes"]` and DEFAULT_EXTENSIONS.
const DEFAULT_EXTENSIONS: &[&str] = &[
    "3ga", "aac", "ogg", "wav", "3g2", "3gp", "asf", "avi", "mp4", "mkv",
];
'@

    $audio = @(Get-GlobExtensions -Text $syntheticHeader -Name 'EXTENSIONS_AUDIO')
    $video = @(Get-GlobExtensions -Text $syntheticHeader -Name 'EXTENSIONS_VIDEO')
    $combined = @($audio + $video)
    $deduped = @()
    foreach ($extension in $combined) {
        if ($deduped -notcontains $extension) { $deduped += $extension }
    }

    $expected = '3ga aac ogg wav 3g2 3gp asf avi mp4 mkv'

    Assert-Equal 'reads one entry per quoted string' '3ga aac ogg wav' ($audio -join ' ')
    Assert-Equal 'reads every entry inside a quoted string' '3g2 3gp asf avi ogg mp4 mkv' ($video -join ' ')
    Assert-Equal 'de-duplicates across the two macros' $expected ($deduped -join ' ')

    $loader = @(Get-LoaderExtensions -Source $syntheticLoader)
    Assert-Equal 'reads the list out of the Rust source' $expected ($loader -join ' ')

    $matching = @(Compare-ExtensionLists -Vlc $deduped -Loader $loader)
    Assert-Equal 'a matching list reports nothing' 0 $matching.Count

    $short = @($loader | Where-Object { $_ -ne 'ogg' })
    Assert-Equal 'an extension VLC declares is reported' 1 (@(Compare-ExtensionLists -Vlc $deduped -Loader $short).Count)

    $long = @($loader + 'nope')
    Assert-Equal 'an extension VLC does not declare is reported' 1 (@(Compare-ExtensionLists -Vlc $deduped -Loader $long).Count)

    $rotated = @($loader[-1]) + @($loader[0..($loader.Count - 2)])
    Assert-Equal 'the same entries in another order are reported' 1 (@(Compare-ExtensionLists -Vlc $deduped -Loader $rotated).Count)

    Assert-Equal 'an empty list is reported rather than accepted' 1 (@(Compare-ExtensionLists -Vlc $deduped -Loader @()).Count)

    $threw = $false
    try { [void](Get-GlobExtensions -Text '#define SOMETHING_ELSE "*.x;"' -Name 'EXTENSIONS_VIDEO') }
    catch { $threw = $true }
    Assert-Equal 'a header without the macro is an error' $true $threw

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

$lockAbs = if ([System.IO.Path]::IsPathRooted($LockPath)) { $LockPath } else { Join-Path $repoRoot $LockPath }
if (-not (Test-Path -LiteralPath $lockAbs)) { throw "'$LockPath' not found" }
if (-not $VlcCommit) { $VlcCommit = Read-LockValue -Lock $lockAbs -Key 'VLC_COMMIT' }

$loaderAbs = if ([System.IO.Path]::IsPathRooted($LoaderSource)) { $LoaderSource } else { Join-Path $repoRoot $LoaderSource }
if (-not (Test-Path -LiteralPath $loaderAbs)) { throw "'$LoaderSource' not found" }

$headerText = $null
$headerFrom = $null
if ($VlcSource) {
    $headerAbs = Join-Path $VlcSource $HeaderPath
    if (-not (Test-Path -LiteralPath $headerAbs)) {
        throw "'$headerAbs' not found; -VlcSource has to be a VLC source tree at $VlcCommit"
    }
    $headerText = Get-Content -LiteralPath $headerAbs -Raw
    $headerFrom = $headerAbs
} else {
    # GitHub first: it serves a single file at an abbreviated commit, which is what the
    # lock records. GitLab is the repository the pin names, and its API serves the same
    # file, but its plain archive and raw paths answer with a bot-check page.
    $urls = @(
        "https://raw.githubusercontent.com/videolan/vlc/$VlcCommit/$HeaderPath",
        "https://code.videolan.org/api/v4/projects/videolan%2Fvlc/repository/files/include%2Fvlc_interface.h/raw?ref=$VlcCommit"
    )
    $lastError = $null
    foreach ($url in $urls) {
        try {
            $headerText = (Invoke-WebRequest -Uri $url -TimeoutSec 120).Content
            $headerFrom = $url
            break
        } catch {
            $lastError = $_.Exception.Message
        }
    }
    if (-not $headerText) {
        throw "could not read $HeaderPath at $VlcCommit ($lastError); pass -VlcSource <path> to read a tree that is already on disk"
    }
}

$declared = @(Get-GlobExtensions -Text $headerText -Name 'EXTENSIONS_AUDIO') +
            @(Get-GlobExtensions -Text $headerText -Name 'EXTENSIONS_VIDEO')
$deduped = @()
foreach ($extension in $declared) {
    if ($deduped -notcontains $extension) { $deduped += $extension }
}

$offered = @(Get-LoaderExtensions -Source (Get-Content -LiteralPath $loaderAbs -Raw))

Write-Host "VLC $VlcCommit declares $($deduped.Count) extensions ($headerFrom)"
Write-Host "the loader offers $($offered.Count)"

$problems = @(Compare-ExtensionLists -Vlc $deduped -Loader $offered)
if ($problems.Count -gt 0) {
    Write-Host ''
    Write-Host "FAILED: the loader's extension list does not match VLC $VlcCommit." -ForegroundColor Red
    foreach ($problem in $problems) { Write-Host "  $problem" }
    Write-Host 'The list is DEFAULT_EXTENSIONS in the loader source: regenerate it from the'
    Write-Host 'header, in the order the header declares it.'
    exit 1
}

Write-Host 'check_media_extensions: OK' -ForegroundColor Green
