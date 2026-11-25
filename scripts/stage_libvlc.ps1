<#
.SYNOPSIS
Unpacks the self-compiled LibVLC runtime into thirdparty/vlc/<platform>/.

.DESCRIPTION
This script performs no network access. It consumes the artifact produced by
build/vlc/ (or build/vlc-android/) and unpacks it into the layout build.rs
expects:

    thirdparty/vlc/<platform>/include/vlc/**
    thirdparty/vlc/<platform>/lib/**
    thirdparty/vlc/<platform>/libexec/**
    thirdparty/vlc/<platform>/share/**

libexec and share are runtime rather than build-time: the first holds the
compiled Lua scripts (metadata readers, playlist parsers, service discovery),
the second the Lua files that are read as data. Without them every plugin still
loads and the Lua-backed modules then do nothing, without saying so.

The Android runtime, which build/vlc-android/ produces, has neither. It is
monolithic -- every module linked into libvlc.so -- so there is no plugin tree to
find in a directory, no preparser to run out of process, and its data path is
compiled in as /system/usr/share. It ships include/ and lib/ and nothing else.

.PARAMETER Platforms
Which platforms to stage. Defaults to the two desktop ones; the Android runtime is
named explicitly, because it is built by a separate job and its artifact is not
present in every checkout.

.PARAMETER ArtifactsDir
Directory holding vlc-<platform>.tar.gz, relative to the repository root.

.PARAMETER IncludeTools
Also unpack tools/ (the vlc CLI), for running a runtime by hand. It never reaches
an addon: assemble_addon.ps1 only copies what the manifest declares. The Android
runtime ships no CLI, so this is ignored for it.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64', 'android-arm64')]
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
    # libexec and share are runtime, not build-time: libexec carries the compiled
    # Lua scripts, share carries the Lua files that are read as data. A runtime
    # without them loads every plugin and the Lua-backed modules then do nothing,
    # without saying so. The Android runtime has neither and is unpacked as what
    # it is.
    $isAndroid = $platform -like 'android-*'
    $members = if ($isAndroid) { @('include', 'lib') } else { @('include', 'lib', 'libexec', 'share') }
    if ($IncludeTools -and -not $isAndroid) { $members += 'tools' }
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
    if ($isAndroid) {
        $libraryCheck = Join-Path $targetDir 'lib/libvlc.so'
        if (-not (Test-Path -LiteralPath $libraryCheck -PathType Leaf)) {
            throw "'$artifactName' does not contain lib/libvlc.so; a monolithic runtime is exactly that one file."
        }
    } else {
        $libexecCheck = Join-Path $targetDir 'libexec/vlc'
        if (-not (Test-Path -LiteralPath $libexecCheck -PathType Container)) {
            throw "'$artifactName' does not contain libexec/vlc; the Lua metadata readers and playlist parsers would be missing."
        }
        $shareCheck = Join-Path $targetDir 'share/vlc'
        if (-not (Test-Path -LiteralPath $shareCheck -PathType Container)) {
            throw "'$artifactName' does not contain share/vlc; the Lua-based modules would be missing."
        }

        # `make install` also puts two executables here, and neither is ever run
        # by this runtime, so both are dropped before anything can copy them into
        # an addon:
        #
        #   vlc-cache-gen   writes plugins/plugins.dat, and only ever from VLC's
        #                   own install-exec-hook (bin/Makefile.am:166-180), which
        #                   runs it only when the build is native; a cross build
        #                   prints "Cross-compilation: cache generation skipped!"
        #                   and ships no cache at all. No source file mentions
        #                   the name, so no runtime path can reach it. The cache
        #                   is an optimisation: without it the module bank logs
        #                   one line and scans the plugin directories instead.
        #
        #   vlc-preparser   the out-of-process preparser, spawned from
        #                   src/preparser/external.c when a preparser is built
        #                   with external_process = true. playlist and
        #                   medialibrary set that; the media parse and thumbnail
        #                   APIs this binding exposes set it false (lib/core.c:272
        #                   and :293) and preparse in process, and the main
        #                   playlist is created lazily, when an interface is added
        #                   (src/libvlc.c:489-535), which this binding never does.
        #
        # Both are compiled executables, and the Godot Asset Store refuses a
        # package containing "Standalone, compiled executables". Dropping them
        # costs nothing measured: with and without, all 43 acceptance tests pass
        # and the run logs are identical line for line.
        #
        # They are dropped here rather than in build/vlc/build.ps1 because that
        # script is part of the runtime cache key in .github/workflows/build.yml,
        # so editing it would rebuild both runtimes from contrib, and because the
        # tarball is an internal cache that is never downloaded by users. BaseName
        # matches vlc-preparser and vlc-preparser.exe alike.
        foreach ($name in @('vlc-cache-gen', 'vlc-preparser')) {
            foreach ($item in @(Get-ChildItem -LiteralPath $libexecCheck -File -Force |
                    Where-Object { $_.BaseName -eq $name })) {
                Remove-Item -LiteralPath $item.FullName -Force
                Write-Host "stage_libvlc: dropped libexec/vlc/$($item.Name) (compiled executable, never run)"
            }
        }
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
    # platform per job, so a check needing several staged in one run never ran.
    Write-Host 'Run scripts/check_vlc_provenance.ps1 to assert the staged platforms share a commit and ABI.'
}

Write-Host 'Done. Next: build_release.ps1 / build_debug.ps1, then assemble_addon.ps1, then check_addon.ps1'
