#!/usr/bin/env pwsh
<#
.SYNOPSIS
Builds LibVLC from source at the revision pinned in vlc.lock and stages a
relocatable runtime tree for one platform.

.DESCRIPTION
Runs inside the pinned container built from build/vlc/Dockerfile, which is where
the toolchain and the glibc floor come from.

The staged tarball layout is deliberately flat, so that scripts/stage_libvlc.ps1
can merge it into thirdparty/vlc/<platform>/ without knowing how VLC lays things
out:

    include/vlc/**              headers, for build.rs / bindgen
    lib/**                      the shipped runtime (see postprocess.ps1)
    tools/vlc                   the CLI; used by the acceptance test, never
                                copied into an addon
    build-info.txt              provenance

VLC's own build system is autotools, so bootstrap / contrib/bootstrap /
configure / make are shell scripts and are invoked as such. Everything written
by this repository is PowerShell.

Escape hatches, all optional environment variables:
    VLC_BUILD_JOBS              parallel build jobs (default: nproc)
    VLC_WORKDIR                 reuse a checkout instead of a scratch directory
    VLC_COMMIT_OVERRIDE         build a different revision, e.g. the fallback
    VLC_EXTRA_CONTRIB_ARGS      appended to contrib/bootstrap
    VLC_EXTRA_CONFIGURE_ARGS    appended to configure

.PARAMETER Platform
linux-x64 or win-x64. win-x64 is a mingw-w64 cross build.

.PARAMETER OutputDir
Directory that receives vlc-<platform>.tar.gz.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)][ValidateSet('linux-x64', 'win-x64')][string]$Platform = 'linux-x64',
    [Parameter(Position = 1)][string]$OutputDir = '/out'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot/lib/native.ps1"

# ---------------------------------------------------------------------------
# Pinned inputs
# ---------------------------------------------------------------------------
$lock = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $PSScriptRoot 'vlc.lock')) {
    if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$') { $lock[$Matches[1]] = $Matches[2] }
}
foreach ($key in @('VLC_REPO', 'VLC_COMMIT', 'VLC_API_VERSION_STRING', 'VLC_ABI_MAJOR', 'VLC_CORE_ABI_MAJOR')) {
    if (-not $lock.ContainsKey($key)) { throw "vlc.lock must define $key" }
}

$prefix = '/opt/vlc'
# Each platform gets its own work directory under VLC_WORKDIR, because they must
# not share a source tree: configure writes config.h and Makefiles into the tree
# it runs in, and re-running it with a different --host invalidates the previous
# host's build state. CI gives each platform a fresh container and never meets
# this; a shared named volume locally does, and the second platform would then
# silently discard the first platform's incremental build.
$workdirBase = if ($env:VLC_WORKDIR) { $env:VLC_WORKDIR } else { '/tmp/vlc-build' }
$workdir = Join-Path $workdirBase $Platform
$src = Join-Path $workdir 'src'
$stage = Join-Path $workdir 'stage'
$runtime = Join-Path $workdir 'runtime'
# DESTDIR staging nests the prefix, so /opt/vlc/lib lands at <stage>/opt/vlc/lib.
$stagedPrefix = Join-Path $stage $prefix.TrimStart('/')

# The lock may hold an abbreviated revision; the fallback is selected explicitly
# rather than silently.
$requestedCommit = if ($env:VLC_COMMIT_OVERRIDE) { $env:VLC_COMMIT_OVERRIDE } else { $lock['VLC_COMMIT'] }

$cross = $Platform -eq 'win-x64'
$hostTriplet = if ($cross) { 'x86_64-w64-mingw32' } else { 'x86_64-linux-gnu' }

$contribArgs = @()
$configureArgs = @("--prefix=$prefix")
# The host triplet is stated explicitly on BOTH platforms, including the native
# Linux build. Left to themselves the two halves of VLC's build disagree about
# it: contrib derives its triplet from `gcc -dumpmachine` (x86_64-linux-gnu on
# Ubuntu) and names the native build tools in contrib/bin after it, while
# configure derives host_alias from config.guess (x86_64-pc-linux-gnu on the same
# machine) and then looks for ${host_alias}-luac. The result is
# `configure: error: Could not find the LUA byte compiler` even though
# contrib/bin/x86_64-linux-gnu-luac exists and is executable.
$contribArgs += "--host=$hostTriplet"
$configureArgs += @("--host=$hostTriplet", "--build=$(& gcc -dumpmachine)")

# No GUI. VLC's Qt interface is the desktop application; this extension embeds
# libvlc behind its own renderer, and the only VLC executable the package uses is
# the CLI the acceptance test drives. Without this, configure refuses to continue
# ("If you want to build VLC without GUI, pass --disable-qt") because no Qt is
# present, in contrib or on the system.
$configureArgs += '--disable-qt'

# VLC's Vulkan stack is not shipped on either platform: neither the libplacebo
# outputs (Vulkan and GL) nor the Shaderc/SPIRV compiler that libplacebo alone
# pulls in, nor the Vulkan loader. One decision, two different reasons for it.
#
# Nothing the extension does is affected. It never uses VLC's video outputs -- it
# renders through libvlc_video_set_output_callbacks, and its GPU path is D3D11 --
# so VLC's Vulkan and libplacebo outputs are unreachable from an embedded player.
#
# Linux: this is what makes the runtime self-contained. Measured before the
# change by running ldd over all 362 plugins, exactly one host library was
# unresolvable and exactly one plugin needed it:
#
#     libvulkan.so.1   1 plugin(s): libplacebo_vk_plugin.so
#
# Everything else, including libvlc and libvlccore, already resolved. Shipping a
# file that cannot load is the defect this project exists to remove: LibVLC logs
# one error, carries on, and the user sees a plugin that silently does nothing.
#
# Windows: libplacebo does not compile against the image's mingw-w64 9 (gcc 10),
# because glslang's SPIRV/doc.cpp uses std::once_flag and std::call_once without
# including <mutex>, which gcc 10 does not pull in transitively. Other packages
# fail the same way, and each would need a patch to a third-party library for a
# feature this extension cannot reach.
#
# To reverse this, drop the flags and add libvulkan.so* to
# scripts/host-provided-libs.txt with the same justification; the closure gate in
# scripts/check_addon.ps1 is what makes the consequence visible either way.
$contribArgs += @('--disable-libplacebo', '--disable-vulkan-loader', '--disable-glslang')

if ($Platform -eq 'win-x64') {
    # The Windows runtime is built from a newer base image than the Linux one
    # (see the top of the Dockerfile), because its mingw-w64 toolchain decides
    # only what cross-compiles and nothing about the artifact's glibc floor -- a
    # Windows runtime is PE and has no glibc dependency at all. That distinction
    # is what lets the Windows build cross-compile with gcc 13 while the Linux
    # runtime keeps the 22.04 toolchain whose glibc the extension shares.
    #
    # It fixed the C++ failures that mingw-w64 9 (gcc 10) produced: glslang's
    # std::once_flag without <mutex>, ggml's std::mutex and std::thread, and
    # libaribcaption's GetUserDefaultLocaleName all compile on gcc 13.
    #
    # ggml still does not, and for a reason no gcc version addresses: mingw-w64
    # does not declare the *thread* power-throttling API that its Windows backend
    # uses, at v9 or at v13. The compiler suggests the process variant, which is
    # a different structure:
    #
    #   ggml-cpu.c: error: unknown type name 'THREAD_POWER_THROTTLING_STATE';
    #               did you mean 'PPROCESS_POWER_THROTTLING_STATE'?
    #
    # ggml backs speech-to-text, not playback, so the Windows runtime omits it
    # rather than carrying a patched copy of a third-party library for a feature
    # this extension does not use. This is a real platform difference, not an
    # oversight: Linux ships ggml, Windows does not.
    #
    # sam3 is disabled with it deliberately: `PKGS_DISABLE` does not survive a
    # dependency, and contrib/src/sam3/rules.mak carries
    # `DEPS_sam3 = ggml $(DEPS_ggml)`, so disabling ggml alone still builds it.
    # sam3 is the SAM3 segmentation filter, equally unrelated to playback.
    $contribArgs += @('--disable-ggml', '--disable-sam3')

    # A mingw plug-in that imports the gcc runtime cannot be loaded at all, and
    # LibVLC's own loader is why. It opens every plug-in with
    # LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32)
    # (src/win32/plugin.c), so a plug-in's dependencies are searched for in
    # System32 and nowhere else: not the application directory, not the plug-in's
    # own directory, and not PATH. A dependency already loaded in the process
    # still satisfies the import, which is how every plug-in resolves
    # libvlccore.dll, and why libgme_plugin.dll failed with error 126
    # (ERROR_MOD_NOT_FOUND) while libgcc_s_seh-1.dll sat in the same directory as
    # the core.
    #
    # Upstream does not meet this. Its Windows build statically links the runtime
    # (extras/package/win32/build.sh sets `-Wl,-l:libunwind.a -Wl,-l:libpthread.a
    # -static-libstdc++` for its clang builds), and where it does not, its own
    # libvlccore.dll imports the runtime, so it is in the process before any
    # plug-in is opened. Linking with a static libgcc removes the dependency
    # rather than depending on that load order.
    #
    # libgcc is the whole of it: measured over all 383 plug-ins, it is the only
    # import that is neither the core nor a system library, and the plug-in that
    # failed imports nothing else unusual.
    #
    # The flags go to contrib as well as to VLC. A plug-in links contrib's static
    # libraries, and a .la that records -lgcc_s brings the shared runtime back at
    # the final link whatever that link asks for, which is why setting LDFLAGS for
    # configure alone left the dependency in place. Contrib has to be rebuilt for
    # the change to reach its .la files, so a work directory that already holds
    # them must be discarded first.
    $env:LDFLAGS = '-static-libgcc -static-libstdc++'
}
if ($env:VLC_EXTRA_CONTRIB_ARGS) { $contribArgs += ($env:VLC_EXTRA_CONTRIB_ARGS -split '\s+') }
if ($env:VLC_EXTRA_CONFIGURE_ARGS) { $configureArgs += ($env:VLC_EXTRA_CONFIGURE_ARGS -split '\s+') }

function Copy-Tree {
    <#
    .SYNOPSIS
    Copies the contents of one directory into another, hidden entries included.
    #>
    param([Parameter(Mandatory)][string]$From, [Parameter(Mandatory)][string]$To)

    New-Item -Path $To -ItemType Directory -Force | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $From -Force) {
        Copy-Item -LiteralPath $item.FullName -Destination $To -Recurse -Force
    }
}

# ---------------------------------------------------------------------------
Write-Host "build: fetching VLC $requestedCommit ($Platform)"
# ---------------------------------------------------------------------------
if (-not (Test-Path -LiteralPath (Join-Path $src '.git'))) {
    New-Item -Path $src -ItemType Directory -Force | Out-Null
    Invoke-Native -Command 'git' -Arguments @('init', '-q', $src)
}
if (@(& git -C $src remote) -contains 'origin') {
    Invoke-Native -Command 'git' -Arguments @('-C', $src, 'remote', 'set-url', 'origin', $lock['VLC_REPO'])
} else {
    Invoke-Native -Command 'git' -Arguments @('-C', $src, 'remote', 'add', 'origin', $lock['VLC_REPO'])
}

# Fetch only what the pin needs. A single commit is identified, history is never
# consulted afterwards, and a full fetch of VLC's tags costs roughly a gigabyte
# and several minutes for nothing. GitLab refuses to serve an arbitrary commit
# this way, so a full fetch remains the fallback; the probe's stderr is captured
# so a refusal does not look like a failure of the build.
$probe = Get-NativeOutput -Command 'git' `
    -Arguments @('-C', $src, 'fetch', '-q', '--depth', '1', 'origin', $requestedCommit)
if ($probe.ExitCode -ne 0) {
    Write-Host 'build: the server does not serve the pinned commit shallowly; falling back to a full fetch'
    Invoke-Native -Command 'git' -Arguments @('-C', $src, 'fetch', '-q', '--tags', 'origin')
}

try {
    Invoke-Native -Command 'git' -Arguments @('-C', $src, 'checkout', '-q', '--force', $requestedCommit)
} catch {
    throw "cannot check out '$requestedCommit' from $($lock['VLC_REPO']). The recorded fallback is VLC_FALLBACK_COMMIT=$($lock['VLC_FALLBACK_COMMIT']); select it with VLC_COMMIT_OVERRIDE=<sha>."
}

# The lock file may record an abbreviation, so what actually gets built is
# resolved to the full revision and recorded in the artifact.
# scripts/check_vlc_provenance.ps1 then compares that across platforms, which is
# what makes "both platforms come from the same commit" a check.
$resolvedCommit = (& git -C $src rev-parse HEAD).Trim()
if (-not $resolvedCommit.StartsWith($requestedCommit)) {
    throw "resolved '$resolvedCommit' does not start with the requested '$requestedCommit'"
}
$describe = (& git -C $src describe --always --tags 2>$null)
if ($LASTEXITCODE -ne 0 -or -not $describe) { $describe = $requestedCommit }

# The plugin ABI check inside VLC is an exact string compare with no
# compatibility range, so the runtime, the plugins and the recorded pin must all
# agree. If this fires, the build reports an ABI that vlc.lock does not describe:
# update VLC_API_VERSION_STRING and re-verify, do not ignore it.
$pluginHeader = Join-Path $src 'include/vlc_plugin.h'
$match = Select-String -LiteralPath $pluginHeader -Pattern '^#\s*define\s+VLC_API_VERSION_STRING\s+"([^"]+)"' |
    Select-Object -First 1
if (-not $match) { throw "could not read VLC_API_VERSION_STRING from $pluginHeader" }
$actualApiVersion = $match.Matches[0].Groups[1].Value
if ($actualApiVersion -ne $lock['VLC_API_VERSION_STRING']) {
    throw "VLC_API_VERSION_STRING mismatch: vlc.lock says '$($lock['VLC_API_VERSION_STRING'])', this revision reports '$actualApiVersion'. Update vlc.lock and re-check the addon."
}
Write-Host "build: ABI check OK (VLC_API_VERSION_STRING=$actualApiVersion)"

# ---------------------------------------------------------------------------
Write-Host 'build: applying patches'
# ---------------------------------------------------------------------------
$patchDir = Join-Path $PSScriptRoot 'patches'
$patches = @(Get-ChildItem -LiteralPath $patchDir -Filter '*.patch' -File -ErrorAction SilentlyContinue | Sort-Object Name)
if ($patches.Count -eq 0) {
    Write-Host 'build: no patches to apply'
}
foreach ($patch in $patches) {
    Write-Host "build: applying $($patch.Name)"
    try {
        Invoke-Native -Command 'git' -Arguments @('-C', $src, 'apply', '--whitespace=nowarn', $patch.FullName)
    } catch {
        throw "$($patch.Name) no longer applies cleanly; rebase it against $requestedCommit"
    }
}

# ---------------------------------------------------------------------------
Write-Host 'build: bootstrap'
# ---------------------------------------------------------------------------
Invoke-Native -Command './bootstrap' -WorkingDirectory $src

# ---------------------------------------------------------------------------
Write-Host 'build: configuring the contrib dependencies'
# ---------------------------------------------------------------------------
# --disable-gpl / --disable-gnuv3 / --enable-ad-clauses are the upstream switches
# for an LGPL runtime; VideoLAN's own Apple, Android and wasm builds use the same
# set.
#
#   --disable-gpl       drops GPL-licensed libraries. x264 is the notable one,
#                       and it is an H.264 *encoder*, so a player does not need
#                       it; decoding H.264/VP9/AV1 goes through avcodec and
#                       dav1d.
#   --disable-gnuv3     drops (L)GPLv3-only libraries such as libidn2.
#   --enable-ad-clauses is NOT optional. freetype is the only package in the
#                       whole contrib tree gated on AD_CLAUSES, and without this
#                       flag the build stops with "Package freetype requires the
#                       GPL license". freetype is VLC's subtitle and OSD text
#                       renderer, so omitting it would cost a player visible
#                       functionality. contrib/bootstrap then reports the licence
#                       as "Lesser GPL version 2.1, with advertisement clauses",
#                       which is upstream's own designation for this combination.
$contribArgs += @('--disable-gpl', '--disable-gnuv3', '--enable-ad-clauses')

# contrib/bootstrap only writes a Makefile; it does not build anything. The
# compilation is the separate `make` below, and it is the slow part.
#
# It also has to run from the triplet directory it is meant to build for,
# because it writes its Makefile into the CURRENT directory -- its generated
# header says "if ../bootstrap is run again". Running it from the repository root
# puts a Makefile there and leaves `make` with nothing to do, which is exactly
# what happened the first time this pipeline was executed.
$contribBuildDir = Join-Path $src "contrib/$hostTriplet"
New-Item -Path $contribBuildDir -ItemType Directory -Force | Out-Null

Invoke-Native -Command '../bootstrap' `
    -Arguments $contribArgs `
    -WorkingDirectory $contribBuildDir

if (-not (Test-Path -LiteralPath (Join-Path $contribBuildDir 'Makefile'))) {
    throw "'../bootstrap' produced no Makefile in '$contribBuildDir'; the contrib layout has changed"
}

# ---------------------------------------------------------------------------
Write-Host 'build: building contrib dependencies (the slow step; upstream quotes one to two hours)'
# ---------------------------------------------------------------------------
Invoke-Native -Command 'make' -WorkingDirectory $contribBuildDir

# A previous attempt that ran bootstrap from the repository root leaves a stray
# Makefile there, which would be taken for the one configure generates.
$strayMakefile = Join-Path $src 'Makefile'
if (Test-Path -LiteralPath $strayMakefile) { Remove-Item -LiteralPath $strayMakefile -Force }

# PKG_CONFIG_PATH is additive. PKG_CONFIG_LIBDIR would replace the system search
# path and silently disable modules that use system libraries.
$contribPkgConfig = Join-Path $src "contrib/$hostTriplet/lib/pkgconfig"
if (-not (Test-Path -LiteralPath $contribPkgConfig)) {
    throw "the contrib build produced no pkg-config directory at '$contribPkgConfig'; configure would silently fall back to system libraries and the runtime would not be self-contained"
}
$env:PKG_CONFIG_PATH = if ($env:PKG_CONFIG_PATH) { "$contribPkgConfig`:$env:PKG_CONFIG_PATH" } else { $contribPkgConfig }

# ---------------------------------------------------------------------------
Write-Host 'build: configure'
# ---------------------------------------------------------------------------
# --disable-vlc is deliberately NOT passed: the vlc CLI is the cheapest headless
# acceptance test available, and it is staged under tools/ only.
Invoke-Native -Command './configure' -Arguments $configureArgs -WorkingDirectory $src

# ---------------------------------------------------------------------------
Write-Host 'build: make'
# ---------------------------------------------------------------------------
$jobs = if ($env:VLC_BUILD_JOBS) { $env:VLC_BUILD_JOBS } else { (& nproc).Trim() }
Invoke-Native -Command 'make' -Arguments @("-j$jobs") -WorkingDirectory $src

# ---------------------------------------------------------------------------
Write-Host 'build: install into a staging root'
# ---------------------------------------------------------------------------
if (Test-Path -LiteralPath $stage) { Remove-Item -Recurse -Force -LiteralPath $stage }
Invoke-Native -Command 'make' -Arguments @('install', "DESTDIR=$stage") -WorkingDirectory $src

# ---------------------------------------------------------------------------
Write-Host 'build: staging the runtime'
# ---------------------------------------------------------------------------
if (Test-Path -LiteralPath $runtime) { Remove-Item -Recurse -Force -LiteralPath $runtime }
foreach ($dir in @('lib', 'include', 'tools')) {
    New-Item -Path (Join-Path $runtime $dir) -ItemType Directory -Force | Out-Null
}

$stagedLib = Join-Path $stagedPrefix 'lib'
$stagedBin = Join-Path $stagedPrefix 'bin'
$stagedInclude = Join-Path $stagedPrefix 'include/vlc'

if ($Platform -eq 'linux-x64') {
    if (-not (Test-Path -LiteralPath $stagedLib -PathType Container)) {
        throw "expected '$stagedLib' after make install"
    }
    if (-not (Test-Path -LiteralPath $stagedInclude -PathType Container)) {
        throw "expected '$stagedInclude' after make install"
    }

    Copy-Tree -From $stagedLib -To (Join-Path $runtime 'lib')
    Copy-Tree -From $stagedInclude -To (Join-Path $runtime 'include/vlc')
    if (Test-Path -LiteralPath $stagedBin -PathType Container) {
        Copy-Tree -From $stagedBin -To (Join-Path $runtime 'tools')
    }

    # Build-only artifacts.
    foreach ($item in @(Get-ChildItem -LiteralPath (Join-Path $runtime 'lib') -Recurse -Force -File |
            Where-Object { $_.Extension -in @('.la', '.a') })) {
        Remove-Item -LiteralPath $item.FullName -Force
    }
    $pkgconfig = Join-Path $runtime 'lib/pkgconfig'
    if (Test-Path -LiteralPath $pkgconfig) { Remove-Item -Recurse -Force -LiteralPath $pkgconfig }

} else {
    $coreDll = Get-ChildItem -LiteralPath $stage -Recurse -Force -File -Filter 'libvlccore*.dll' |
        Select-Object -First 1
    if (-not $coreDll) { throw "libvlccore*.dll not found under '$stage' after make install" }

    $pluginsDir = Get-ChildItem -LiteralPath $stage -Recurse -Force -Directory -Filter 'plugins' |
        Select-Object -First 1
    if (-not $pluginsDir) { throw "no plugins directory found under '$stage' after make install" }

    $includeDir = Get-ChildItem -LiteralPath $stage -Recurse -Force -Directory |
        Where-Object { $_.FullName.Replace('\', '/') -like '*/include/vlc' } |
        Select-Object -First 1
    if (-not $includeDir) { throw "no include/vlc directory found under '$stage' after make install" }

    foreach ($dll in Get-ChildItem -LiteralPath $coreDll.DirectoryName -Force -File -Filter '*.dll') {
        Copy-Item -LiteralPath $dll.FullName -Destination (Join-Path $runtime 'lib') -Force
    }

    # Import libraries, for the extension to link against. They are build-time
    # only: the gdextension manifest does not declare them, so they stay out of
    # the addon and are read from thirdparty/ by the Rust build.
    #
    # The mingw build installs them under mingw names (libvlc.dll.a). The
    # extension is compiled with MSVC on windows-latest, where build.rs emits
    # `cargo:rustc-link-lib=vlc` and the linker therefore looks for vlc.lib, so
    # both names are provided. The mapping is the one the two conventions imply:
    # `-lX` means libX.a on GNU and X.lib on MSVC, so libX.dll.a becomes X.lib.
    # They are not two different artifacts: a mingw .dll.a and an MSVC .lib are
    # both COFF short-import libraries, and link.exe reads either.
    # scripts/test.ps1 links against them, so a mismatch here fails immediately
    # and loudly rather than at a user's first build.
    #
    # Matched by name across the whole install tree rather than in one directory:
    # the mingw install puts the DLLs in bin/ and the import libraries in lib/,
    # which are different places, so "beside the DLL" finds nothing.
    $importLibs = @(Get-ChildItem -LiteralPath $stage -Recurse -Force -File |
            Where-Object { $_.Name -in @('libvlc.dll.a', 'libvlccore.dll.a') })
    if ($importLibs.Count -lt 2) {
        throw "expected libvlc.dll.a and libvlccore.dll.a under '$stage' after make install, found $($importLibs.Count); the extension could not link against the runtime"
    }
    foreach ($importLib in $importLibs) {
        Copy-Item -LiteralPath $importLib.FullName -Destination (Join-Path $runtime 'lib') -Force

        $msvcName = ($importLib.Name -replace '^lib', '') -replace '\.dll\.a$', '.lib'
        Copy-Item -LiteralPath $importLib.FullName -Destination (Join-Path $runtime "lib/$msvcName") -Force
    }
    foreach ($name in @('libvlc.dll.a', 'vlc.lib', 'libvlccore.dll.a', 'vlccore.lib')) {
        if (-not (Test-Path -LiteralPath (Join-Path $runtime "lib/$name"))) {
            throw "expected '$name' in the runtime after make install; the extension could not link"
        }
    }
    Copy-Item -LiteralPath $pluginsDir.FullName -Destination (Join-Path $runtime 'lib/plugins') -Recurse -Force
    Copy-Tree -From $includeDir.FullName -To (Join-Path $runtime 'include/vlc')

    # There is no mingw runtime DLL to stage, and this is deliberate rather than
    # an omission. Plug-ins used to import libgcc_s_seh-1.dll, and LibVLC's loader
    # could not resolve it: it opens every plug-in with
    # LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32)
    # (src/win32/plugin.c), so a plug-in's dependencies are searched for in
    # System32 and nowhere else -- not the application directory, not the plug-in's
    # own directory, and not PATH. libgme_plugin.dll failed with error 126 while
    # the DLL sat next to the core, because nothing had loaded it into the process
    # first.
    #
    # LDFLAGS below now links the runtime statically, so no plug-in imports it:
    # measured over all 383, none does. Staging it would therefore ship a file
    # nothing uses, and scripts/check_addon.ps1 would report it as present but
    # undeclared.

    # The CLI, for the acceptance test only.
    if (Test-Path -LiteralPath $stagedBin -PathType Container) {
        foreach ($tool in Get-ChildItem -LiteralPath $stagedBin -Force -File |
                Where-Object { $_.Extension -in @('.exe', '.dll') }) {
            Copy-Item -LiteralPath $tool.FullName -Destination (Join-Path $runtime 'tools') -Force
        }
    }
}

# ---------------------------------------------------------------------------
# Beyond the libraries, VLC needs two more installation directories at runtime,
# and both are easy to miss because every plugin loads without them:
#
#   libexec/vlc   the out-of-process preparser and the plugin cache generator,
#                 plus the compiled Lua scripts
#   share/vlc     the Lua playlist parsers and service discovery scripts
#
# Without libexec an interface cannot start at all: VLC reports "Fail to create
# Process in process_pool" and then "cannot start any interface", which reads
# like a plugin problem and is not one. That is how this was found on Linux.
#
# Both platforms stage them. The Windows runtime is built by the same script from
# the same sources, so there is no reason to expect it to need less, and the
# windows package that this replaces shipped neither. If the Windows install
# layout turns out not to produce them, the assertion below says so rather than
# producing a package that quietly cannot start.
foreach ($extra in @('libexec', 'share/vlc')) {
    $from = Join-Path $stagedPrefix $extra
    if (-not (Test-Path -LiteralPath $from -PathType Container)) {
        throw "expected '$from' after make install"
    }
    Copy-Tree -From $from -To (Join-Path $runtime $extra)
}

# ---------------------------------------------------------------------------
Write-Host 'build: normalising runpaths and asserting the result'
# ---------------------------------------------------------------------------
& "$PSScriptRoot/postprocess.ps1" -Platform $Platform -Runtime $runtime `
    -AbiMajor ([int]$lock['VLC_ABI_MAJOR']) -CoreAbiMajor ([int]$lock['VLC_CORE_ABI_MAJOR'])

# ---------------------------------------------------------------------------
Write-Host 'build: packaging'
# ---------------------------------------------------------------------------
@(
    "platform=$Platform"
    "vlc_repo=$($lock['VLC_REPO'])"
    "vlc_commit=$resolvedCommit"
    "vlc_commit_requested=$requestedCommit"
    "vlc_describe=$describe"
    "vlc_api_version_string=$actualApiVersion"
    "vlc_abi_major=$($lock['VLC_ABI_MAJOR'])"
    "vlc_core_abi_major=$($lock['VLC_CORE_ABI_MAJOR'])"
    'gpl_free=yes'
) | Set-Content -LiteralPath (Join-Path $runtime 'build-info.txt')

New-Item -Path $OutputDir -ItemType Directory -Force | Out-Null
$artifact = Join-Path $OutputDir "vlc-$Platform.tar.gz"
if (Test-Path -LiteralPath $artifact) { Remove-Item -LiteralPath $artifact -Force }
Invoke-Native -Command 'tar' -Arguments @('-czf', $artifact, '-C', $runtime, 'include', 'lib', 'libexec', 'share', 'tools', 'build-info.txt')

Write-Host ''
Write-Host "build: artifact $artifact"
Write-Host "build: size     $([math]::Round((Get-Item -LiteralPath $artifact).Length / 1MB, 1)) MB"
Write-Host "build: vlc      $describe (API $actualApiVersion)"
