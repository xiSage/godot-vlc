<#
.SYNOPSIS
Proves that the addon shows video on a real Android device.

.DESCRIPTION
The device-side counterpart of acceptance_test.ps1. That one stops where a device
begins: it decodes H.264 through the runtime on the host. This one takes the
assembled addon, exports the demo, installs it, and asserts from the device's own
log which video output LibVLC opened:

    using vout display module "vmem"

vmem is the memory output. Frames are decoded to RAM and handed to the callbacks
the extension turns into a Godot texture, so that line is the difference between
a picture and no picture -- and the failure without it is quiet. The runtime
VideoLAN publishes cannot create a video output on Android at all: it logs
"no vout window modules matched with name dummy" once and carries on playing
audio, which is why "sound but no picture" was the symptom that started this.

Two more assertions exist for the same reason: the APK must carry the extension
and the demo's media file. A device-side load cannot be diagnosed from a build
that silently shipped neither, and both have been missing at least once.

It runs where the device is, which is never CI, so it is a developer's check
rather than a pipeline step.

Known rough edge: adb's server is a daemon that can hold on to the handles of
whatever started it, so a caller that *captures* this script's output can sit there
after the verdict has been printed. The work is done at that point. The server is
started detached below, which removes the usual reason for it; a caller that wants
to be certain should redirect to a file rather than capture, which leaves nothing of
its own for a daemon to hold:

    Start-Process pwsh -ArgumentList '-File','scripts/acceptance_test_android.ps1' `
        -RedirectStandardOutput run.log -RedirectStandardError run.err

Three things about this flow are easy to get wrong, so the script does them
rather than documenting them for the caller:

  * a sleeping device recycles the activity it was just asked to show within a
    fraction of a second, before the engine has printed anything, so the device
    is woken and kept awake first;
  * project scripts are parsed before a GDExtension registers its classes, so the
    first export after a clean checkout cannot load the demo; the project is
    imported once with the engine first;
  * assembling one platform replaces the whole payload, so the host platform is
    assembled alongside Android -- otherwise the next host run, or the next
    export, finds no extension at all.

.PARAMETER Apk
Where to write the exported APK. Defaults to artifacts/godot-vlc-android.apk.

.PARAMETER Package
Application id to install and launch. It has to match demo/export_presets.cfg.

.PARAMETER TimeoutSeconds
How long to wait for LibVLC to report its video output.

.PARAMETER SkipBuild
Use the extension already under target/, instead of rebuilding it.

.PARAMETER Screenshot
Also save a screenshot beside the APK, for a human to look at.
#>
[CmdletBinding()]
param(
    [string]$Apk = 'artifacts/godot-vlc-android.apk',

    [string]$Package = 'com.example.godotvlc',

    [int]$TimeoutSeconds = 150,

    [switch]$SkipBuild,

    [switch]$Screenshot
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
. "$PSScriptRoot/lib/vlc_runtime.ps1"

function Write-Step {
    param([string]$Message)
    Write-Host ''
    Write-Host "== $Message" -ForegroundColor Cyan
}

function Invoke-Adb {
    param([string[]]$Arguments, [switch]$AllowFailure)
    $output = & $script:adb @Arguments 2>&1
    if ($LASTEXITCODE -ne 0 -and -not $AllowFailure) {
        throw "adb $($Arguments -join ' ') failed with $LASTEXITCODE`n$output"
    }
    $output
}

# ---------------------------------------------------------------------------
Write-Step 'checking the tools this needs'
# ---------------------------------------------------------------------------
$adb = (Get-Command 'adb' -ErrorAction SilentlyContinue)?.Source
if (-not $adb -and $env:ANDROID_HOME) {
    $candidate = Join-Path $env:ANDROID_HOME 'platform-tools/adb.exe'
    if (Test-Path -LiteralPath $candidate) { $adb = $candidate }
}
if (-not $adb) { throw 'adb is not on PATH and ANDROID_HOME/platform-tools does not have it' }
$script:adb = $adb

# Started detached, and deliberately: adb's server is a daemon that inherits the
# handles of whatever started it, and one holding this script's stdout keeps a
# wrapper that captures the output -- a CI step, an editor task, another shell --
# waiting for a pipe that will not close, long after the verdict has been printed.
# Started here, it inherits the console of the process created for it instead.
#
# Without -Wait, which matters more than it looks: on Windows -Wait waits for the
# process tree, and start-server forks the daemon, so -Wait never returns at all.
# The sleep is what keeps the next command from racing it and starting a second
# server of its own, which would inherit our handles and undo this.
Start-Process -FilePath $adb -ArgumentList 'start-server' -WindowStyle Hidden | Out-Null
Start-Sleep -Seconds 3

$devices = Invoke-Adb -Arguments @('devices')
$ready = @($devices | Select-String -Pattern "`tdevice$" | ForEach-Object { ($_.Line -split "`t")[0] })
$unauthorized = @($devices | Select-String -Pattern "`tunauthorized$" | Measure-Object).Count
if ($ready.Count -eq 0) {
    if ($unauthorized -gt 0) {
        throw 'a device is attached but not authorised: accept the USB debugging prompt on it'
    }
    throw 'no device is attached'
}
Write-Host "  device(s): $($ready -join ', ')"

$ndkRoot = if ($env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME }
elseif ($env:ANDROID_NDK_ROOT) { $env:ANDROID_NDK_ROOT }
else { $null }
# The NDK ships one prebuilt toolchain per host, and the one needed here is this
# host's: the extension is cross-compiled from the machine the device is plugged
# into. The *runtime* is built in a Linux container instead, and that container
# fetches its own Linux NDK, so nothing on this machine has to be one.
$ndkHostTag = if ($IsWindows) { 'windows-x86_64' } else { 'linux-x86_64' }

if (-not $SkipBuild) {
    if (-not $ndkRoot) { throw 'set ANDROID_NDK_HOME to the Android NDK; the build cannot link without it' }
    if (-not (Test-Path -LiteralPath (Join-Path $ndkRoot "toolchains/llvm/prebuilt/$ndkHostTag"))) {
        throw "$ndkRoot has no $ndkHostTag toolchain"
    }
    $rustTargets = @(& rustup target list --installed 2>$null)
    if ($rustTargets -notcontains 'aarch64-linux-android') {
        throw 'run: rustup target add aarch64-linux-android (from the repository, so the pinned toolchain gets it)'
    }
}

$godot = $null
foreach ($candidate in @('godot_console', 'godot', 'godot4')) {
    $command = Get-Command $candidate -ErrorAction SilentlyContinue
    if ($command) { $godot = $command.Source; break }
}
if (-not $godot) { throw 'Godot is not on PATH; the demo has to be exported by the engine' }

# ---------------------------------------------------------------------------
Write-Step 'checking the staged runtime'
# ---------------------------------------------------------------------------
$runtimeDir = Join-Path $repoRoot 'thirdparty/vlc/android-arm64/lib'
if (-not (Test-Path -LiteralPath (Join-Path $runtimeDir 'libvlc.so'))) {
    throw 'thirdparty/vlc/android-arm64/lib/libvlc.so is missing. Stage it: pwsh scripts/stage_libvlc.ps1 -Platforms android-arm64'
}
$soSize = (Get-Item -LiteralPath (Join-Path $runtimeDir 'libvlc.so')).Length
Write-Host ("  libvlc.so: {0:N0} bytes" -f $soSize)
if (Test-Path -LiteralPath (Join-Path $repoRoot 'artifacts/vlc-android-arm64.tar.gz')) {
    $info = tar -xzf (Join-Path $repoRoot 'artifacts/vlc-android-arm64.tar.gz') -O build-info.txt
    foreach ($key in @('vlc_commit', 'vlc_api_version_string', 'license')) {
        $line = $info | Where-Object { $_ -match "^$key=" }
        if ($line) { Write-Host "  $($line -replace '=', ': ')" }
    }
}

# ---------------------------------------------------------------------------
if (-not $SkipBuild) {
    Write-Step 'building the extension for arm64'
    $toolchain = Join-Path $ndkRoot "toolchains/llvm/prebuilt/$ndkHostTag/bin"
    $clang = Join-Path $toolchain "aarch64-linux-android24-clang$($IsWindows ? '.cmd' : '')"
    $env:CC_aarch64_linux_android = $clang
    $env:AR_aarch64_linux_android = Join-Path $toolchain 'llvm-ar.exe'
    $env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $clang
    Push-Location $repoRoot
    try {
        & cargo build --target aarch64-linux-android
        if ($LASTEXITCODE -ne 0) { throw 'the debug build failed' }
        & cargo build --release --target aarch64-linux-android
        if ($LASTEXITCODE -ne 0) { throw 'the release build failed' }
    } finally { Pop-Location }
}

# Assembling replaces the payload, so the host platform comes along: without it
# the extension cannot be loaded on this machine, and the engine would export a
# demo whose classes do not exist.
Write-Step 'assembling the addon for the host and for Android'
& (Join-Path $PSScriptRoot 'assemble_addon.ps1') -Platforms (Get-HostVlcPlatform), 'android-arm64' | Select-Object -First 1

# ---------------------------------------------------------------------------
Write-Step 'importing the demo, then exporting it'
# ---------------------------------------------------------------------------
Push-Location $repoRoot
try {
    # Scripts are parsed before a GDExtension registers its classes, so a project
    # that has never been opened cannot be exported: the demo's scene script
    # extends a class that does not exist yet. One editor pass fixes the cache.
    & $godot --headless --editor --quit --path "$repoRoot/demo" *> $null

    $apkPath = if ([System.IO.Path]::IsPathRooted($Apk)) { $Apk } else { Join-Path $repoRoot $Apk }
    New-Item -Path (Split-Path -Parent $apkPath) -ItemType Directory -Force | Out-Null
    if (Test-Path -LiteralPath $apkPath) { Remove-Item -LiteralPath $apkPath -Force }
    & $godot --headless --path "$repoRoot/demo" --export-debug 'Android' $apkPath
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $apkPath)) {
        throw "the export failed; expected $apkPath"
    }
} finally { Pop-Location }
Write-Host ("  apk: {0:N0} bytes" -f (Get-Item -LiteralPath $apkPath).Length)

# A build that shipped neither the extension nor the media would install and run
# and tell nobody why there was no video.
$apkEntries = tar -tf $apkPath
foreach ($required in @('lib/arm64-v8a/libvlc.so', 'assets/test.mp4')) {
    if (-not ($apkEntries | Where-Object { $_ -eq $required })) {
        throw "the APK does not contain $required"
    }
}
$extensionEntry = $apkEntries | Where-Object { $_ -match '^lib/arm64-v8a/libgodot_vlc.*\.so$' }
if (-not $extensionEntry) { throw 'the APK does not contain the extension' }
Write-Host "  carries: $($extensionEntry -join ', '), assets/test.mp4, lib/arm64-v8a/libvlc.so"

# ---------------------------------------------------------------------------
Write-Step 'waking the device, so the activity is not recycled on arrival'
# ---------------------------------------------------------------------------
Invoke-Adb -Arguments @('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP') | Out-Null
Invoke-Adb -Arguments @('shell', 'wm', 'dismiss-keyguard') -AllowFailure | Out-Null
# Without this the screen sleeps again mid-run and the same recycling happens.
Invoke-Adb -Arguments @('shell', 'svc', 'power', 'stayon', 'true') | Out-Null

Write-Step 'installing'
Invoke-Adb -Arguments @('install', '-r', $apkPath) | Select-Object -Last 1 | ForEach-Object { Write-Host "  $_" }

# ---------------------------------------------------------------------------
Write-Step 'launching and reading the device back'
# ---------------------------------------------------------------------------
Invoke-Adb -Arguments @('logcat', '-G', '16M') -AllowFailure | Out-Null
Invoke-Adb -Arguments @('logcat', '-c') | Out-Null
Invoke-Adb -Arguments @('shell', 'monkey', '-p', $Package, '-c', 'android.intent.category.LAUNCHER', '1') | Out-Null

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$log = @()
$chosen = $null
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 5
    $log = Invoke-Adb -Arguments @('logcat', '-d') -AllowFailure
    $chosen = $log | Select-String -Pattern 'using vout display module' | Select-Object -Last 1
    if ($chosen) { break }
    if ($log | Select-String -Pattern 'Failed loading resource|Unable to start engine') { break }
}

$logPath = [System.IO.Path]::ChangeExtension($apkPath, '.logcat.txt')
$log | Set-Content -LiteralPath $logPath
Write-Host "  log: $logPath"

if ($Screenshot) {
    $shotPath = [System.IO.Path]::ChangeExtension($apkPath, '.png')
    # Through cmd, because PowerShell's redirection would re-encode the PNG.
    & cmd /c "`"$script:adb`" exec-out screencap -p > `"$shotPath`""
    Write-Host "  screenshot: $shotPath"
}

# ---------------------------------------------------------------------------
Write-Step 'the result'
# ---------------------------------------------------------------------------
$failed = @()
if (-not $chosen) {
    $failed += "LibVLC never reported a video output within $TimeoutSeconds seconds"
} elseif ($chosen.Line -notmatch 'vmem') {
    $failed += "LibVLC chose $($chosen.Line.Trim()), which the extension cannot turn into a texture"
}
if ($log | Select-String -Pattern 'failed to create video output') {
    $failed += 'LibVLC reported that it could not create a video output'
}
if ($log | Select-String -Pattern 'Failed loading resource: res://test\.mp4') {
    $failed += 'the demo could not load res://test.mp4 on the device'
}
if ($log | Select-String -Pattern 'No loader found for resource: res://test\.mp4') {
    $failed += 'the device found no loader for res://test.mp4: the APK''s extension does not register one, so the APK is older than the source'
}

foreach ($line in @('godot-vlc:', 'using vout display module', 'using video decoder module', 'using audio output module')) {
    $log | Select-String -Pattern $line | ForEach-Object { ($_.Line -split 'godot\s+: ')[-1] } |
        Select-Object -Unique | ForEach-Object { Write-Host "  $_" }
}

if ($failed.Count -gt 0) {
    Write-Host ''
    Write-Host 'device test: FAILED' -ForegroundColor Red
    $failed | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}

Write-Host ''
Write-Host 'device test: OK (the device chose the vmem video output, so frames reach the texture)' -ForegroundColor Green
# Stated rather than fallen out of: a native child that outlives its output being
# read can otherwise keep this process alive after the verdict is printed.
exit 0
