<#
.SYNOPSIS
Unpacks the self-compiled LibVLC runtime into thirdparty/vlc/<platform>/.

.DESCRIPTION
This script performs no network access. It consumes the artifact produced by
build/vlc/ and unpacks it into the layout build.rs expects:

    thirdparty/vlc/<platform>/include/vlc/**
    thirdparty/vlc/<platform>/lib/**
    thirdparty/vlc/<platform>/libexec/**
    thirdparty/vlc/<platform>/share/**

libexec and share are runtime rather than build-time: the first holds the
out-of-process preparser and the plugin cache generator, the second the Lua
scripts. Without them every plugin still loads and no interface can start.

tools/ (the vlc CLI) is not unpacked unless asked for. It is not what ships and
not what the acceptance test uses -- that drives libvlc directly, from
src/acceptance.rs -- so it is here for looking at a runtime by hand.

Provenance is not compared here. scripts/check_vlc_provenance.ps1 reads the
artifacts directly, because CI stages one platform per job and a check needing
both staged in one run never executed.

.PARAMETER Platforms
Which platforms to stage. Defaults to both.

.PARAMETER ArtifactsDir
Directory holding vlc-<platform>.tar.gz, relative to the repository root.

.PARAMETER IncludeTools
Also unpack tools/ (the vlc CLI), for running a runtime by hand. It never reaches
an addon: assemble_addon.ps1 only copies what the manifest declares.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64')]
    [string[]]$Platforms = @('win-x64', 'linux-x64'),

    [string]$ArtifactsDir = 'artifacts',

    [switch]$IncludeTools
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$artifactsAbs = Join-Path $repoRoot $ArtifactsDir

# Provenance recorded by build/vlc/build.ps1, keyed by platform.
$buildInfo = @{}

foreach ($platform in $Platforms) {
    $artifactName = "vlc-$platform.tar.gz"
    $artifactPath = Join-Path $artifactsAbs $artifactName

    if (-not (Test-Path -LiteralPath $artifactPath)) {
        throw "artifact '$ArtifactsDir/$artifactName' not found. Run the VLC build container first; see build/vlc/README.md."
    }

    $targetDir = Join-Path $repoRoot "thirdparty/vlc/$platform"
    if (Test-Path -LiteralPath $targetDir) {
        Remove-Item -Recurse -Force -LiteralPath $targetDir
    }
    New-Item -Path $targetDir -ItemType Directory -Force | Out-Null

    # bsdtar (shipped with Windows 10+) and GNU tar both accept this form.
    # libexec and share are runtime, not build-time: libexec carries the preparser
    # and plugin cache generator, share carries the Lua scripts. A runtime without
    # them loads every plugin and still cannot start an interface.
    $members = @('include', 'lib', 'libexec', 'share')
    if ($IncludeTools) { $members += 'tools' }
    tar -xzf $artifactPath -C $targetDir @members
    if ($LASTEXITCODE -ne 0) {
        throw "failed to unpack '$artifactName' (tar exited with $LASTEXITCODE)"
    }

    $headerCheck = Join-Path $targetDir 'include/vlc/vlc.h'
    if (-not (Test-Path -LiteralPath $headerCheck)) {
        throw "'$artifactName' does not contain include/vlc/vlc.h; refusing to continue with an incomplete runtime."
    }
    $libCheck = Join-Path $targetDir 'lib'
    if (-not (Test-Path -LiteralPath $libCheck) -or @(Get-ChildItem -LiteralPath $libCheck -Force).Count -eq 0) {
        throw "'$artifactName' does not contain a populated lib/ directory."
    }
    $libexecCheck = Join-Path $targetDir 'libexec/vlc'
    if (-not (Test-Path -LiteralPath $libexecCheck -PathType Container)) {
        throw "'$artifactName' does not contain libexec/vlc; the runtime could not start an interface."
    }
    $shareCheck = Join-Path $targetDir 'share/vlc'
    if (-not (Test-Path -LiteralPath $shareCheck -PathType Container)) {
        throw "'$artifactName' does not contain share/vlc; the Lua-based modules would be missing."
    }

    # Read build-info.txt straight out of the tarball so that it is never
    # mistaken for a shippable runtime file.
    $infoText = tar -xzf $artifactPath -O build-info.txt
    if ($LASTEXITCODE -ne 0 -or -not $infoText) {
        throw "'$artifactName' has no build-info.txt; it was not produced by build/vlc/build.ps1."
    }
    $info = @{}
    foreach ($line in $infoText) {
        if ($line -match '^([^=#]+)=(.*)$') { $info[$Matches[1].Trim()] = $Matches[2].Trim() }
    }
    $buildInfo[$platform] = $info

    # Linux ships its SONAME chains as symlinks, and the extraction must keep them
    # that way. Which tar ran decides that: on Windows the bundled bsdtar recreates
    # links, while the msys64 GNU tar that Git Bash uses silently writes a copy of
    # the target instead, producing a runtime that looks complete, loads, and is
    # not the tree CI verified. Nothing downstream can tell the difference -- the
    # file count, the sizes and ldd all agree -- so it is checked here, with the
    # same names build/vlc/build.ps1 asserts on the way out.
    if ($platform -eq 'linux-x64') {
        foreach ($name in @('libvlc.so', "libvlc.so.$($info['vlc_abi_major'])",
                            'libvlccore.so', "libvlccore.so.$($info['vlc_core_abi_major'])")) {
            $item = Get-Item -LiteralPath (Join-Path $targetDir "lib/$name") -Force -ErrorAction SilentlyContinue
            if (-not $item) {
                throw "'$artifactName' has no lib/$name; the runtime is incomplete."
            }
            if (-not $item.LinkTarget) {
                throw "'$artifactName' unpacked lib/$name as a regular file, so the SONAME chain was flattened into copies. This happens with msys64's GNU tar; use the tar Windows ships (bare 'tar') or Git Bash with MSYS=winsymlinks:nativestrict."
            }
        }
    }

    Write-Host "staged $platform (vlc $($info['vlc_describe']), API $($info['vlc_api_version_string']))"
}

if ($buildInfo.Count -gt 1) {
    # Cross-platform provenance is asserted by scripts/check_vlc_provenance.ps1,
    # which reads the artifacts directly. It used to live here, but CI stages one
    # platform per job, so a check needing both staged in one run never executed.
    Write-Host 'Both platforms staged. Run scripts/check_vlc_provenance.ps1 to assert they share a commit and ABI.'
}

Write-Host 'Done. Next: build_release.ps1 / build_debug.ps1, then assemble_addon.ps1, then check_addon.ps1'
