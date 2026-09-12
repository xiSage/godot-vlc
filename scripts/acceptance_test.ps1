<#
.SYNOPSIS
Proves that the shipped LibVLC decodes H.264.

.DESCRIPTION
The acceptance test for the runtime build, and the question the whole pipeline
exists to answer: does the LibVLC in the addon decode H.264 on a machine that has
nothing else installed?

The decoding is done by src/acceptance.rs, a Rust test that drives LibVLC through
the same software video callbacks the extension uses. It runs against the
ASSEMBLED ADDON -- the environment below points the loader at the addon rather
than at the staged tree -- so the bytes under test are the ones users receive.

It is deliberately stricter than "a frame arrived". LibVLC's failures are quiet:
a plugin that cannot be loaded produces one error line and LibVLC carries on, and
the visible symptom is a codec that "is not supported", which reads like a codec
gap rather than a file that cannot load. So the run fails on plugin-load failures
as well, because a shipped file that cannot load is a defect in the package
regardless of whether playback survives it.

Driving the `vlc` command-line tool was tried first and abandoned. It is not what
ships: the addon is a library the extension loads. On Windows the CLI is also a
GUI-subsystem binary that needs a console it may not be able to attach, and it
responded by writing its output to vlc-help.txt and blocking in a modal "Press the
RETURN key to continue" dialog -- a hang with no output that said nothing about
the runtime. It additionally reads and writes the invoking user's configuration
and looks for plugins beside itself rather than beside the runtime. None of that
concerns a library, and all of it disappeared when the test moved to this one.

Requires:
    scripts/stage_libvlc.ps1
    scripts/assemble_addon.ps1

.PARAMETER Platform
Which assembled runtime to test. Defaults to the host platform, and may only be
the host platform: a Windows runtime cannot be executed from Linux or the other
way round.

.PARAMETER RuntimeDir
Defaults to the assembled addon. Override it to check a staged runtime before it
has been assembled.

.PARAMETER Sample
The media file to decode, relative to the repository root.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64')]
    [string]$Platform,

    [string]$RuntimeDir,

    [string]$Sample = 'test/media/h264_64x64_1s.mp4'
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot

. "$PSScriptRoot/lib/vlc_runtime.ps1"

if (-not $Platform) { $Platform = Get-HostVlcPlatform }
$hostPlatform = Get-HostVlcPlatform
if ($Platform -ne $hostPlatform) {
    throw "the $Platform runtime cannot be executed on $hostPlatform; run this on a $Platform host (CI runs each platform's test on that platform's runner)"
}

$runtimeDir = if ($RuntimeDir) {
    if ([System.IO.Path]::IsPathRooted($RuntimeDir)) { $RuntimeDir } else { Join-Path $repoRoot $RuntimeDir }
} else {
    Join-Path $repoRoot "gdextension_template/bin/$Platform"
}
if (-not (Test-Path -LiteralPath $runtimeDir)) {
    throw "'$runtimeDir' is missing. Run scripts/assemble_addon.ps1 -Platforms $Platform first; this test deliberately exercises the assembled addon rather than the staged tree."
}

$samplePath = Join-Path $repoRoot $Sample
if (-not (Test-Path -LiteralPath $samplePath)) {
    throw "the sample '$Sample' is missing"
}

# Point the loader and LibVLC at the assembled addon. This is the same mechanism
# the extension uses at runtime (src/vlc_runtime.rs), so a pass here also shows
# that the pinning is enough.
Add-VlcRuntimeToSearchPath -LibDir $runtimeDir -Platform $Platform

Write-Host "acceptance: decoding $Sample with the runtime in $runtimeDir"

# --nocapture so that LibVLC's own log is visible; it is the only place a
# plugin-load failure shows up.
$output = & cargo test --lib acceptance::decodes_the_h264_sample -- --nocapture 2>&1
$exitCode = $LASTEXITCODE
$text = ($output | ForEach-Object { "$_" }) -join "`n"

if ($text) { $output | ForEach-Object { "  $_" } }

if ($exitCode -ne 0) {
    Write-Host "acceptance: FAILED (cargo test exited with $exitCode)"
    exit 1
}

$loadFailures = @(
    # Matched up to the file extension rather than to the next colon: a Windows
    # path starts with a drive letter, so a non-greedy match to ':' yields "D".
    $output |
        Select-String -SimpleMatch 'cannot load plug-in' |
        ForEach-Object { [regex]::Match($_.Line, 'cannot load plug-in (.+?\.(?:dll|so[0-9.]*))').Groups[1].Value } |
        Where-Object { $_ } |
        Sort-Object -Unique
)
if ($loadFailures.Count -gt 0) {
    Write-Host "acceptance: FAILED with $($loadFailures.Count) plugin(s) that could not be loaded:"
    foreach ($failure in $loadFailures) { Write-Host "  $failure" }
    Write-Host 'A plugin that cannot load is a defect in the package even when playback survives it.'
    exit 1
}

Write-Host "acceptance: OK ($Platform decoded $Sample; no plugin failed to load)"
