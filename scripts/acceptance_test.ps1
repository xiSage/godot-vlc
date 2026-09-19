<#
.SYNOPSIS
Proves that the shipped LibVLC decodes H.264, reports a media it cannot open,
loops between two points, and fills in the fields of a track.

.DESCRIPTION
The acceptance test for the runtime build, and the question the whole pipeline
exists to answer: does the LibVLC in the addon decode H.264 on a machine that has
nothing else installed?

The decoding is done by src/acceptance.rs, a Rust test that drives LibVLC through
the same software video callbacks the extension uses. It runs against the
ASSEMBLED ADDON -- the environment below points the loader at the addon rather
than at the staged tree -- so the bytes under test are the ones users receive.

The second test in that file covers the other half of what the runtime has to get
right: a media that does not exist has to be reported through libvlc's error
event. Nothing else reports it -- play() returns 0 for a media it cannot open,
and the error state is not one a caller can observe -- so a runtime that stopped
raising that event would leave the extension's `error` signal silently useless.

Three more tests cover the A to B loop, which is the only way this runtime offers
to repeat a piece of a media: one that a loop set in milliseconds wraps while
playback stays running and describes itself through libvlc's getter, one that does
the same for a loop set as positions, and one that the loop does not outlive the
input it was set on -- a stop or a new media takes it with it, which is what the
extension's documentation has to tell its users.

The rest pins what the extension exposes on top of libvlc, one test per promise:
the per-media options, which are read when the input is created and not after; the
subtitle entry points, where the delay belongs to the input while the text scale
belongs to the player; and the fields of the track struct, where the test is which
member of libvlc's union gets read -- the members a track's type does not name are
memory libvlc never wrote, so a reader that tested the pointer instead of the type
would pass heap memory on. Those track tests use two media on purpose: the small
sample's geometry is either the file's own 64x64 at 1:1 and 10 fps, or six zeroes --
which of the two depends on the platform, so the test accepts both and rejects
anything else -- while the demo's media reports 854x480, `1280:1281` and stereo audio
everywhere, and that is asserted outright.

Track selection is here too, and it is the half of it that needs the runtime rather
than an engine: that a selection replaces the type's whole set, that libvlc's cap of
two text tracks drops the third one in silence, that an id selects its track and is
applied again by the next input, that an id matching nothing clears the selection,
that the four track events carry the payloads the binding's five signals are built
from, and that none of it does anything without an input. The engine-side half --
whether an `Array[VLCTrack]` crosses into Rust, whether the signals arrive in a
script, and whether a track from a media descriptor is refused instead of crashing --
is `demo/tests/track_selection.gd`, which is run by hand like the other demo tests.

Media identity is here as well: what `libvlc_media_get_mrl` answers for a path (a
percent-encoded `file://` URI), for a location (verbatim) and for a duplicate (the
same as its source), what `libvlc_media_get_type` guesses and how a parse rewrites
that guess -- a local `.m3u` is a file until it is parsed and a playlist after -- and
that an option added to a duplicate shortens only the duplicate. The one answer this
cannot reach is the in-memory media the binding's own `load_from_file` builds, which
reports the constant `imem://`; that one is measured in `demo/tests/media_loader.gd`.

The runtime's self-report is here too: that `libvlc_get_version` and
`libvlc_get_changeset` name the build this addon pins (the changeset is a `git
describe` string, so the pinned commit is inside it rather than at its front), that
the error status belongs to the thread that failed and is not cleared by a successful
call, and that the log context libvlc hands a callback names the module, the source
file and the line. The binding's own half of the log -- the level mapping, the signal
and the runtime setter for the level -- is `demo/tests/log_message.gd` plus a unit
test in `src/vlc_instance.rs`.

The audio delay is here as well, and it is the one `libvlc_audio_*` setting that needs
no audio output: the value is kept on the input and handed to the decoders, so this
harness -- whose instances are built with `--no-audio` -- can still set it and read it
back, which is what one of the three tests does. They also pin the unit trap that comes
with it: the `audio-desync` option seeds the same field in milliseconds while the API is
in microseconds, so an instance built with `--audio-desync=250` has to read back
`250000`. The relative jump is measured at three of its four edges, including the one
that has no answer in the source -- past the end, where playback simply ends.

The playback-time watcher is here as well, and so is the measurement that made it
testable: this harness runs with `--vout=dummy` and no audio, and the watcher fires
anyway -- about twenty points a second for a ten-frame-a-second media, because the
input clock reports as well as the output. What the four tests pin is the runtime's
half of it: one watcher per player with a second registration refused, a larger
`min_period_us` meaning fewer reports, the two calls libvlc makes for one seek (the
point asked for, then none at all), and the arithmetic of `time_point_interpolate` --
including the path where it reports failure and hands back a parameter it filled from
its own uninitialised stack. The binding's half -- the four signals, the interpolation
method, and the two guards that exist because libvlc would abort or crash rather than
answer -- is `demo/tests/time_point.gd`.

Media lists are here too, in the half that needs no player: a media's own subitems, which
are its list of what a playlist file, a disc or a directory holds. The list is live and
read-only, and reading it means holding libvlc's own lock -- which the binding does --
because the parsing thread appends to it from behind that same lock. The events are here
in the shape libvlc sends them: two per change, one before it and one after, each
carrying the media and its index. The end of a parse has its own event, and it is measured
to come from `parse_request` rather than from playing the media, because libvlc sends it
only where it reports a parsed status changing. The wrapper a script builds, and the
signals arriving with a `VLCMedia` in hand, are `demo/tests/media_list.gd`.

It is deliberately stricter than "a frame arrived". LibVLC's failures are quiet:
a plugin that cannot be loaded produces one error line and LibVLC carries on, and
the visible symptom is a codec that "is not supported", which reads like a codec
gap rather than a file that cannot load. So the run fails on plugin-load failures
as well, because a shipped file that cannot load is a defect in the package
regardless of whether playback survives it.

That check covers every test here. The media the second one asks for does not
exist, so its run logs failures of its own -- but not this one: it was measured to
produce no "cannot load plug-in" line, which is the only thing the check looks
for.

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

Write-Host "acceptance: decoding $Sample, reporting a missing media and looping between two times, with the runtime in $runtimeDir"

# --nocapture so that LibVLC's own log is visible; it is the only place a
# plugin-load failure shows up.
$output = & cargo test --lib acceptance:: -- --nocapture 2>&1
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

Write-Host "acceptance: OK ($Platform decoded $Sample, reported a missing media, looped between two points, and read the fields of a track; no plugin failed to load)"
