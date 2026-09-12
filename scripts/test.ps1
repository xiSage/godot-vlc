<#
.SYNOPSIS
Runs the Rust test suite with the LibVLC runtime on the library search path.

.DESCRIPTION
The crate links against LibVLC, so the test binary has to resolve
libvlc/libvlccore when it starts. They are not installed system-wide; they live
under thirdparty/vlc/<platform>/lib. On Windows that means PATH, on Linux
LD_LIBRARY_PATH.

Without this, the test binary fails to start with STATUS_DLL_NOT_FOUND (Windows)
or "cannot open shared object file" (Linux), which looks like a broken test
suite rather than a missing search path.

Extra arguments are forwarded to cargo, e.g.:
    pwsh scripts/test.ps1 -- --nocapture
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments)][string[]]$CargoArgs = @()
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot

. "$PSScriptRoot/lib/vlc_runtime.ps1"

$platform = Get-HostVlcPlatform
$libDir = Join-Path $repoRoot "thirdparty/vlc/$platform/lib"
if (-not (Test-Path -LiteralPath $libDir)) {
    throw "thirdparty/vlc/$platform/lib is missing. Stage the runtime first; see build/vlc/README.md."
}

Add-VlcRuntimeToSearchPath -LibDir $libDir -Platform $platform

Write-Host "Using LibVLC runtime from thirdparty/vlc/$platform/lib"

& cargo test @CargoArgs
exit $LASTEXITCODE
