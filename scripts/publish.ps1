<#
.SYNOPSIS
Builds and packages the addon for distribution.

.DESCRIPTION
Produces <name>_v<version>.zip containing the assembled addon, which is what
users install.

The addon is assembled and checked before packaging. A package whose LibVLC
runtime is missing or incomplete installs cleanly and then fails at runtime with
"Codec not supported", so the checks are part of publishing rather than an
optional extra.

Every platform in the release must have been staged first:
    scripts/stage_libvlc.ps1 -Platforms win-x64,linux-x64,android-arm64

.PARAMETER Platforms
Platforms to include. Defaults to all three, because a release that supports
Windows, Linux and Android needs all three runtimes. A platform that is not there
is an error rather than a quiet omission, so a checkout that means to publish less
has to name what it means.

.PARAMETER SkipBuild
Skip the cargo builds and package what is already in target/. Used by CI, where
the per-platform build jobs have already produced the binaries.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64', 'android-arm64')]
    [string[]]$Platforms = @('win-x64', 'linux-x64', 'android-arm64'),

    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not $SkipBuild) {
    & "$PSScriptRoot/build_debug.ps1"
    & "$PSScriptRoot/build_release.ps1"
}
& "$PSScriptRoot/assemble_addon.ps1" -Platforms $Platforms
& "$PSScriptRoot/check_addon.ps1" -Platforms $Platforms

$manifest = cargo read-manifest | ConvertFrom-Json
$fileName = "$($manifest.name)_v$($manifest.version).zip"

Remove-Item -Path $fileName -ErrorAction SilentlyContinue
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    "demo/addons",
    $fileName,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $true
)

Write-Host "Wrote $fileName"
