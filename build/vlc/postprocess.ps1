#!/usr/bin/env pwsh
<#
.SYNOPSIS
Normalises the runtime tree produced by `make install` so that it is
relocatable, then asserts the result.

.DESCRIPTION
LibVLC is built with a compile-time install prefix and libtool bakes that
absolute path into the libraries it produces. Once the tree is moved into a
Godot addon those paths are wrong. Replacing them by hand is what made the
previous runtime unreproducible, so it is done here, on every build, and then
checked.

The tree is also stripped of debug information here, because VLC is built with
-g and DWARF is most of what came out: measured on the artifacts before this
existed, 666 MB of the Linux tree's 798 MB of shared objects and 827 MB of the
Windows tree's 1026 MB were debug sections. That is the size users downloaded.
See Remove-DebugInfo for the reasoning and for what is deliberately left alone.

What the runpaths have to be:

  every shipped .so, one $ORIGIN step for each directory it sits below the
  runtime root

      lib/libvlccore.so.9            depth 1   $ORIGIN:$ORIGIN/..
      lib/vlc/libvlc_xcb_events.so.0 depth 2   ...:$ORIGIN/../..
      lib/vlc/plugins/codec/x.so     depth 3   ...:$ORIGIN/../../..
      lib/vlc/plugins/access/rtp/x.so depth 4  ...:$ORIGIN/../../../..

  The ladder is computed from the depth rather than written out, because a fixed
  path is right for the libraries that happen to sit at that depth and wrong for
  the rest. See Get-RelocatableRunpath in lib/runpath.ps1.

Only regular files are patched. patchelf replaces a symlink with a regular file,
so patching libvlc.so.12 (a symlink to libvlc.so.12.0.0) would silently break the
SONAME chain; the resolved file is patched instead and the symlinks keep pointing
at it.

.PARAMETER Platform
linux-x64 or win-x64.

.PARAMETER Runtime
Directory containing include/ and lib/, patched in place.

.PARAMETER AbiMajor
libvlc SONAME major, from vlc.lock.

.PARAMETER CoreAbiMajor
libvlccore SONAME major, from vlc.lock.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('linux-x64', 'win-x64')][string]$Platform,
    [Parameter(Mandatory)][string]$Runtime,
    [Parameter(Mandatory)][int]$AbiMajor,
    [Parameter(Mandatory)][int]$CoreAbiMajor
)

$ErrorActionPreference = 'Stop'

# Both the runpath rule and the native-command helpers are shared with
# build.ps1 and check-runpath.ps1 so the gate and the patcher cannot disagree.
. "$PSScriptRoot/lib/runpath.ps1"
. "$PSScriptRoot/lib/native.ps1"

if (-not (Test-Path -LiteralPath $Runtime -PathType Container)) {
    throw "runtime directory '$Runtime' does not exist"
}



function Get-RealSharedObject {
    <#
    .SYNOPSIS
    Resolves shared objects to the file that actually carries the ELF headers.

    .DESCRIPTION
    A staged install contains SONAME symlinks, so the same inode can appear under
    several names. Resolving first means each file is patched exactly once, and
    that a symlink is never handed to patchelf.
    #>
    param(
        [Parameter(Mandatory)][string]$Directory,
        [switch]$Recurse
    )

    $items = if ($Recurse) {
        Get-ChildItem -LiteralPath $Directory -Recurse -Force -File
    } else {
        Get-ChildItem -LiteralPath $Directory -Force -File
    }

    $seen = [System.Collections.Generic.HashSet[string]]::new()
    foreach ($item in $items) {
        if ($item.Name -notlike '*.so' -and $item.Name -notlike '*.so.*') { continue }

        $real = if ($item.LinkType) { $item.ResolveLinkTarget($true).FullName } else { $item.FullName }
        if ($seen.Add($real)) { Get-Item -LiteralPath $real -Force }
    }
}

function Assert-Runpath {
    param(
        [Parameter(Mandatory)][string]$File,
        [Parameter(Mandatory)][string]$Expected
    )

    # -W (wide) is required: readelf wraps at 80 columns by default and the
    # plugin runpath is long enough that the closing bracket would land on a
    # continuation line.
    $readelf = Get-NativeOutput -Command 'readelf' -Arguments @('-W', '-d', $File)
    if ($readelf.ExitCode -ne 0) { throw "readelf failed on '$File'" }

    $actual = Get-RunpathValue -Output $readelf.Output
    if ($actual -ne $Expected) {
        throw "expected a RUNPATH of '$Expected' on '$File', got '$(if ($null -eq $actual) { '<none>' } else { $actual })'"
    }
}

function Assert-Relocatable {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$SharedObjects)

    foreach ($file in $SharedObjects) {
        $readelf = Get-NativeOutput -Command 'readelf' -Arguments @('-W', '-d', $file.FullName)
        if ($readelf.ExitCode -ne 0) { throw "readelf failed on '$($file.FullName)'" }
        $tags = Get-RelativePathTag -Output $readelf.Output
        foreach ($bad in Get-NonRelocatableComponent -Values $tags.Value) {
            throw "non-relocatable RPATH/RUNPATH component '$bad' on '$($file.Name)'"
        }
    }
}

function Remove-DebugInfo {
    <#
    .SYNOPSIS
    Strips the debug sections from every shipped shared object, then asserts none
    survived.

    .DESCRIPTION
    VLC is configured with -g, and nothing downstream was removing the result, so
    the addon shipped it. Measured on the artifacts from before this existed: the
    Linux tree's 798 MB of shared objects carried 666 MB of .debug_* sections, the
    Windows tree's 1026 MB carried 827 MB, and the packed Linux tarball was 307 MB
    where the same tree stripped comes to 53 MB. A game addon is not a debugging
    symbol package, VLC's own releases are stripped, and the extension beside the
    runtime is a cargo release build, which carries none either.

    --strip-debug rather than --strip-unneeded, deliberately. The debug sections
    are the whole of the problem, and leaving the static symbol table in place
    keeps this change from being the one that surprises somebody: --strip-unneeded
    also removes symbols a shared object does not need at run time, which is more
    than this was asked to do.

    Two tools, because one binutils does not read both formats: strip for ELF and
    the mingw-w64 triplet's strip for PE, which is the same binutils built for
    that target. The result is read back with the matching objdump/readelf,
    because a strip that silently did nothing would leave exactly the artifact
    this exists to prevent -- and scripts/check_addon.ps1 asserts the same
    property again on the assembled addon.
    #>
    param(
        [Parameter(Mandatory)][ValidateSet('linux-x64', 'win-x64')][string]$Platform,
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$SharedObjects
    )

    if ($SharedObjects.Count -eq 0) { throw "no shared objects to strip for $Platform" }

    $strip = if ($Platform -eq 'linux-x64') { 'strip' } else { 'x86_64-w64-mingw32-strip' }
    if (-not (Get-Command $strip -ErrorAction SilentlyContinue)) { throw "$strip not found" }

    # For PE the triplet's objdump is preferred: the host's objdump is built for
    # ELF and reports the format rather than the sections.
    $inspector = if ($Platform -eq 'linux-x64') { 'readelf' }
    elseif (Get-Command 'x86_64-w64-mingw32-objdump' -ErrorAction SilentlyContinue) { 'x86_64-w64-mingw32-objdump' }
    else { 'objdump' }
    $inspectArguments = if ($Platform -eq 'linux-x64') { @('-W', '-S') } else { @('-h') }

    foreach ($file in $SharedObjects) {
        Invoke-Native -Command $strip -Arguments @('--strip-debug', $file.FullName)
    }

    # Anchored to the section-name column: objdump prints the file's path in its
    # first line, and a path is free to contain ".debug" without being one. The
    # index is bracketed in readelf's table and bare in objdump's, and both are
    # accepted here -- a pattern that only knew one of them would find no
    # survivors in the other format, which is indistinguishable from a clean file
    # and would make this check decorative.
    $tableMarker = if ($Platform -eq 'linux-x64') { 'Section Headers:' } else { 'Sections:' }
    $sectionLine = '^\s*(\[\s*\d+\]\s+|\d+\s+)?\.z?debug'

    $survivors = @()
    foreach ($file in $SharedObjects) {
        $dump = Get-NativeOutput -Command $inspector -Arguments ($inspectArguments + @($file.FullName))
        if ($dump.ExitCode -ne 0) { throw "$inspector failed on '$($file.FullName)'" }
        # An inspector that printed no table at all -- the wrong format, a tool
        # that failed quietly -- would otherwise leave a file looking clean,
        # because a section that was never listed cannot be matched.
        if (-not ($dump.Output | Select-String -SimpleMatch $tableMarker)) {
            throw "$inspector printed no section table for '$($file.FullName)'"
        }
        $names = @(
            $dump.Output |
                Select-String -Pattern $sectionLine |
                ForEach-Object { ($_.Line.Trim() -split '\s+' | Where-Object { $_ -like '.*' } | Select-Object -First 1) }
        )
        if ($names.Count -gt 0) { $survivors += "$($file.Name): $($names[0])" }
    }
    if ($survivors.Count -gt 0) {
        throw "debug sections survived stripping in $($survivors.Count) file(s); first is $($survivors[0])"
    }

    Write-Host "  stripped $($SharedObjects.Count) shared object(s) of debug information"
}

$lib = Join-Path $Runtime 'lib'
if (-not (Test-Path -LiteralPath $lib -PathType Container)) { throw "expected '$lib'" }

if ($Platform -eq 'linux-x64') {
    if (-not (Get-Command patchelf -ErrorAction SilentlyContinue)) { throw 'patchelf not found' }

    if (-not (Test-Path -LiteralPath (Join-Path $lib "libvlccore.so.$CoreAbiMajor"))) {
        throw "missing expected runtime library: lib/libvlccore.so.$CoreAbiMajor"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $lib "libvlc.so.$AbiMajor"))) {
        throw "missing expected runtime library: lib/libvlc.so.$AbiMajor"
    }

    # The plugin directory is a direct child of lib/ (lib/vlc/plugins). Looking
    # for it this way rather than by recursion is deliberate: Get-ChildItem
    # -Recurse does not descend directory symlinks, so a recursive search could
    # silently find nothing and let the build pass with no plugins checked.
    $pluginsDir = Get-ChildItem -LiteralPath $lib -Directory -Force |
        ForEach-Object { Join-Path $_.FullName 'plugins' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Container } |
        Select-Object -First 1
    if (-not $pluginsDir) { throw "no plugins directory found under '$lib'" }

    # The helper libraries live beside the plugins directory (lib/vlc), the
    # plugins below it, and the core in lib itself. All of them are shipped shared
    # objects and all of them are patched by the same rule below.
    $helperDir = Split-Path -Parent $pluginsDir
    $plainLibraries = @()
    $plainLibraries += Get-RealSharedObject -Directory $lib
    $plainLibraries += Get-RealSharedObject -Directory $helperDir
    $plugins = @(Get-RealSharedObject -Directory $pluginsDir -Recurse)
    if ($plugins.Count -eq 0) { throw "'$pluginsDir' contains no modules" }

    # --- patch every shipped shared object ----------------------------------
    # One rule, computed from where each file sits, rather than one rule for the
    # libraries and a different one for the plugins: the plugins are not all at
    # the same depth, and the helper libraries beside them are at another.
    $runtimeRoot = (Resolve-Path -LiteralPath $Runtime).Path.TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $linuxObjects = @($plainLibraries + $plugins)
    # Before the runpaths are patched, so that everything asserted below is what
    # actually ships.
    Remove-DebugInfo -Platform 'linux-x64' -SharedObjects $linuxObjects
    foreach ($file in $linuxObjects) {
        $directory = Split-Path -Parent $file.FullName
        if (-not $directory.StartsWith($runtimeRoot, [System.StringComparison]::Ordinal)) {
            throw "'$($file.FullName)' is not inside '$runtimeRoot'"
        }

        $relative = $directory.Substring($runtimeRoot.Length)
        $expected = Get-RelocatableRunpath -Depth (Get-RunpathDepth -RelativeDirectory $relative)
        Invoke-Native -Command 'patchelf' -Arguments @('--set-rpath', $expected, $file.FullName)
        Assert-Runpath -File $file.FullName -Expected $expected
    }

    # The core must be a shared library the modules link against. The direct test
    # for a statically linked core is that lib/libvlccore.so.<major> exists as a
    # separate file at all, which is asserted above. This adds early evidence that
    # the modules actually use it.
    #
    # Not every module records the dependency: a few upstream leaf modules (the
    # dummy window and text providers, the audio mixers, the spdif filter) call no
    # core symbol, so the linker records nothing for them. Requiring literally
    # all of them fails on those, so the requirement here is that the bulk of the
    # modules link the core; scripts/check_addon.ps1 resolves every module's
    # dependencies on the assembled addon, which is the exhaustive check.
    $linkedToCore = 0
    foreach ($file in $plugins) {
        $readelf = Get-NativeOutput -Command 'readelf' -Arguments @('-W', '-d', $file.FullName)
        if ($readelf.Output | Select-String -Pattern "NEEDED.*libvlccore\.so\.$CoreAbiMajor" -Quiet) {
            $linkedToCore++
        }
    }
    if ($linkedToCore * 2 -le $plugins.Count) {
        throw "only $linkedToCore of $($plugins.Count) plugin modules link libvlccore.so.$CoreAbiMajor; the core does not look like the shared library the modules use"
    }
    Write-Host "  $linkedToCore of $($plugins.Count) plugin modules link libvlccore.so.$CoreAbiMajor"

    Assert-Relocatable -SharedObjects (@($plainLibraries) + @($plugins))

    Write-Host "  patched and verified $($plugins.Count) plugin modules in $pluginsDir"

} else {
    foreach ($name in @('libvlccore.dll', 'libvlc.dll')) {
        if (-not (Test-Path -LiteralPath (Join-Path $lib $name))) {
            throw "missing expected runtime library: lib/$name"
        }
    }

    # On Windows libvlccore resolves its plugin directory as <directory of the
    # module containing it>\plugins, so plugins/ must be a sibling of the DLLs.
    # There is no runpath concept to normalise.
    $pluginsDir = Join-Path $lib 'plugins'
    if (-not (Test-Path -LiteralPath $pluginsDir -PathType Container)) {
        throw "expected '$pluginsDir' next to the DLLs"
    }

    $dlls = @(Get-ChildItem -LiteralPath $pluginsDir -Recurse -Force -File -Filter '*.dll')
    if ($dlls.Count -eq 0) { throw "'$pluginsDir' contains no modules" }

    $elf = @(Get-ChildItem -LiteralPath $lib -Recurse -Force -File |
        Where-Object { $_.Name -like '*.so' -or $_.Name -like '*.so.*' })
    if ($elf.Count -gt 0) { throw "found ELF shared objects in the Windows runtime: $($elf[0].Name)" }

    # The core DLLs sit beside the plugin directory rather than inside it, so the
    # list the plugins come from is not the whole runtime.
    Remove-DebugInfo -Platform 'win-x64' -SharedObjects (@(
            Get-Item -LiteralPath (Join-Path $lib 'libvlccore.dll')
            Get-Item -LiteralPath (Join-Path $lib 'libvlc.dll')
        ) + $dlls)

    Write-Host "  verified $($dlls.Count) plugin modules"
}

Write-Host 'postprocess: OK'
