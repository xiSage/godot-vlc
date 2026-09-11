<#
.SYNOPSIS
Verifies that the assembled addon is complete and self-contained.

.DESCRIPTION
Runs the invariant that the previous Linux runtime violated:

  1. MANIFEST -- every file present under bin/<platform>/ is declared in
     godot_vlc.gdextension or covered by a declared directory, and every declared
     path exists. Godot exports a project from the [dependencies] list, so a
     runtime file that is not declared is a file that silently goes missing from
     exported games.

  2. CLOSURE -- for every shared object in the addon, every dependency must
     resolve, and every resolution must land either inside the addon (and be
     declared) or in scripts/host-provided-libs.txt (Linux) or
     scripts/host-provided-dlls.txt (Windows). This is what turns "we believe
     the runtime is self-contained" into an assertion.

Both checks refuse to report success when they could not actually run: a missing
inspector, or a declared directory that yielded no shared objects, is a failure
rather than a pass. The previous revision of this script reported "0 shared
objects inspected" and exited 0, because Get-ChildItem -Recurse does not descend
directory symlinks.

Run scripts/assemble_addon.ps1 first: this inspects the assembled addon, not the
source tree, because the assembled addon is what users receive.

.PARAMETER SelfTest
Exercises the parsing and comparison logic against synthetic inputs and exits.
The self-test needs no addon, no inspector and no Linux, so it runs anywhere.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64')]
    [string[]]$Platforms = @('win-x64', 'linux-x64'),

    [string]$GdextensionPath = 'gdextension_template/godot_vlc.gdextension',
    [string]$HostProvidedListPath = 'scripts/host-provided-libs.txt',
    [string]$HostProvidedDllListPath = 'scripts/host-provided-dlls.txt',
    [string]$AddonPrefix = 'res://addons/godot-vlc/',

    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

# Needed by both the self-test and the main pass, so it is sourced before the
# self-test dispatch below.
. "$PSScriptRoot/lib/gdextension.ps1"

# ldd and binutils translate their output, and the parsers below read it.
# Pinning the locale keeps the parse stable on non-English systems.
$env:LC_ALL = 'C'
$env:LANG = 'C'

#region Pure helpers

# Parses ldd output into { Name, Resolved, Found }. Lines without a path (the
# vDSO, or a bare absolute loader path) come back with an empty Resolved and
# Found = $true; they are host-provided by definition.
function Get-LddEntry {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    $entries = @()
    foreach ($line in $Output) {
        if ($line -match '^\s*(?<name>\S+)\s*=>\s*(?<rest>.+?)\s*$') {
            $name = $Matches['name']
            $rest = $Matches['rest']
            if ($rest -match '^not found') {
                $entries += [pscustomobject]@{ Name = $name; Resolved = ''; Found = $false }
            } elseif ($rest -match '^(?<path>\S+)\s+\(0x') {
                $entries += [pscustomobject]@{ Name = $name; Resolved = $Matches['path']; Found = $true }
            } else {
                $entries += [pscustomobject]@{ Name = $name; Resolved = $rest; Found = $true }
            }
        } elseif ($line -match '^\s*(?<path>/\S+)\s+\(0x') {
            $entries += [pscustomobject]@{
                Name     = [System.IO.Path]::GetFileName($Matches['path'])
                Resolved = $Matches['path']
                Found    = $true
            }
        } elseif ($line -match '^\s*(?<name>\S+)\s+\(0x') {
            $entries += [pscustomobject]@{ Name = $Matches['name']; Resolved = ''; Found = $true }
        }
    }
    @($entries)
}

# Parses the import table out of `llvm-objdump -p` / `objdump -p` output.
#
# The match is case-sensitive on purpose: objdump prints imported DLLs as
# "DLL Name:" and the file's own name as "DLL name:". A case-insensitive match
# would treat the object as importing itself.
function Get-PeImportEntry {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    $entries = @()
    foreach ($line in $Output) {
        if ($line -cmatch '^\s*DLL Name:\s*(?<name>\S+)\s*$') {
            # PE resolution is by name and search path, so nothing can be
            # resolved statically; an empty Resolved means "must come from the
            # host" unless the caller finds it inside the addon.
            $entries += [pscustomobject]@{ Name = $Matches['name']; Resolved = ''; Found = $true }
        }
    }
    @($entries)
}

# Rewrites entries whose DLL is shipped inside the addon so that the shared
# dependency rules apply to them.
function Resolve-PeImportEntry {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Entries,
        [Parameter(Mandatory)][AllowEmptyCollection()][hashtable]$AddonDllIndex,
        [Parameter(Mandatory)][string]$AddonRoot
    )

    $root = $AddonRoot.Replace('\', '/').TrimEnd('/')
    $resolved = @()
    foreach ($entry in $Entries) {
        $shipped = $AddonDllIndex[$entry.Name.ToLowerInvariant()]
        if ($shipped) {
            $resolved += [pscustomobject]@{
                Name     = $entry.Name
                Resolved = "$root/$shipped"
                Found    = $true
            }
        } else {
            $resolved += $entry
        }
    }
    @($resolved)
}

function Test-HostProvided {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Patterns
    )

    foreach ($pattern in $Patterns) {
        if ($Name -like $pattern) { return $true }
    }
    $false
}

# A relative path is covered when it is declared outright or lives under a
# declared directory (the addon declares `vlc` and `plugins` as directories).
function Test-Declared {
    param(
        [Parameter(Mandatory)][string]$RelativePath,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$DeclaredPaths
    )

    foreach ($declared in $DeclaredPaths) {
        if ($RelativePath -eq $declared) { return $true }
        if ($RelativePath.StartsWith("$declared/", [System.StringComparison]::Ordinal)) { return $true }
    }
    $false
}

function Test-DependencyEntries {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Entries,
        [Parameter(Mandatory)][string]$AddonRoot,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$DeclaredPaths,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$HostProvidedPatterns,
        [string]$Source = ''
    )

    $root = $AddonRoot.Replace('\', '/').TrimEnd('/')
    $violations = @()

    foreach ($entry in $Entries) {
        $where = if ($Source) { " (needed by $Source)" } else { '' }

        if (-not $entry.Found) {
            $violations += "unresolved dependency: $($entry.Name)$where"
            continue
        }

        if ([string]::IsNullOrEmpty($entry.Resolved)) {
            if (-not (Test-HostProvided -Name $entry.Name -Patterns $HostProvidedPatterns)) {
                $violations += "dependency '$($entry.Name)' is neither shipped nor in the host-provided list$where"
            }
            continue
        }

        $resolved = $entry.Resolved.Replace('\', '/')

        if ($resolved.StartsWith("$root/", [System.StringComparison]::Ordinal)) {
            $relative = $resolved.Substring($root.Length + 1)
            if (-not (Test-Declared -RelativePath $relative -DeclaredPaths $DeclaredPaths)) {
                $violations += "shipped dependency is missing from the [dependencies] manifest: $relative$where"
            }
        } elseif (-not (Test-HostProvided -Name $entry.Name -Patterns $HostProvidedPatterns)) {
            $violations += "depends on host library '$($entry.Name)' -> $resolved, which is not in the host-provided list$where"
        }
    }

    @($violations)
}

function Test-ManifestCoverage {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$PresentPaths,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$ExtensionPaths,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$DeclaredPaths
    )

    $violations = @()
    foreach ($present in $PresentPaths) {
        if ($ExtensionPaths -contains $present) { continue }
        if (Test-Declared -RelativePath $present -DeclaredPaths $DeclaredPaths) { continue }
        $violations += "present in the addon but not declared in godot_vlc.gdextension: $present"
    }
    @($violations)
}

# Guards against a closure check that inspected nothing. A declared directory
# that yields no shared objects means the enumeration did not descend it, which
# is how this script previously reported success while inspecting no plugins at
# all.
function Test-InspectedCoverage {
    <#
    .SYNOPSIS
    Asserts that every declared directory was actually populated.

    .DESCRIPTION
    The closure check can only inspect what it finds, so a declared directory that
    contributes nothing is indistinguishable from a pass unless it is checked
    separately. That is not hypothetical: the payload was once assembled with a
    symlink, Get-ChildItem -Recurse does not descend directory symlinks, and the
    declared `plugins` directory yielded none of its 412 files while the closure
    check passed vacuously.

    The requirement is that a declared directory contains files, not that it
    contains shared objects. libexec/ and share/ are data -- the out-of-process
    preparser, the plugin cache generator and the Lua scripts -- and hold no
    shared objects at all, so requiring some from them failed on a correct addon.
    #>
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$DeclaredDirectories,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$InspectedPaths,
        [Parameter(Mandatory)][string]$AddonRoot
    )

    $violations = @()
    foreach ($directory in $DeclaredDirectories) {
        $full = Join-Path $AddonRoot $directory
        $files = @(Get-ChildItem -LiteralPath $full -Recurse -Force -File -ErrorAction SilentlyContinue)
        if ($files.Count -eq 0) {
            $violations += "declared directory '$directory' contains no files; it was declared but never populated"
            continue
        }

        $found = @($InspectedPaths | Where-Object { $_.StartsWith("$directory/", [System.StringComparison]::Ordinal) })
        Write-Verbose "'$directory' holds $($files.Count) file(s), of which $($found.Count) were inspected as shared objects"
    }
    @($violations)
}

#endregion Pure helpers

#region Self-test

function Invoke-SelfTest {
    $script:selfTestFailures = 0

    function Assert-Equal {
        param([string]$Description, $Expected, $Actual)
        $expectedText = ($Expected | ForEach-Object { "$_" }) -join ' | '
        $actualText = ($Actual | ForEach-Object { "$_" }) -join ' | '
        if ($expectedText -eq $actualText) {
            Write-Host "ok   - $Description"
        } else {
            Write-Host "FAIL - $Description"
            Write-Host "       expected: $expectedText"
            Write-Host "       actual:   $actualText"
            $script:selfTestFailures++
        }
    }

    $manifest = @(
        '[configuration]'
        'entry_symbol = "gdext_rust_init"'
        ''
        '[libraries]'
        'linux.debug.x86_64 = "res://addons/godot-vlc/bin/linux-x64/libgodot_vlc_debug.so"'
        'linux.release.x86_64 = "res://addons/godot-vlc/bin/linux-x64/libgodot_vlc.so"'
        'windows.release.x86_64 = "res://addons/godot-vlc/bin/win-x64/godot_vlc.dll"'
        ''
        '[dependencies]'
        'linux.x86_64 = {'
        "`t`"res://addons/godot-vlc/bin/linux-x64/libvlc.so.12`": `"`","
        "`t`"res://addons/godot-vlc/bin/linux-x64/libvlccore.so.9`": `"`","
        "`t`"res://addons/godot-vlc/bin/linux-x64/vlc`": `"`""
        '}'
        'windows.x86_64 = { "res://addons/godot-vlc/bin/win-x64/libvlc.dll": "", "res://addons/godot-vlc/bin/win-x64/plugins": "" }'
        ''
        '[icons]'
    )

    $deps = Get-GdextensionPaths -Lines $manifest -Section 'dependencies' -Key 'linux.x86_64'
    Assert-Equal 'parses a multi-line dependency block' 3 $deps.Count
    Assert-Equal 'keeps dependency order' 'res://addons/godot-vlc/bin/linux-x64/libvlc.so.12' $deps[0]

    $inline = Get-GdextensionPaths -Lines $manifest -Section 'dependencies' -Key 'windows.x86_64'
    Assert-Equal 'parses a single-line dependency block' 2 $inline.Count

    $libs = Get-GdextensionPaths -Lines $manifest -Section 'libraries' -Key 'linux.release.x86_64'
    Assert-Equal 'parses a library entry' 1 $libs.Count

    $missing = Get-GdextensionPaths -Lines $manifest -Section 'dependencies' -Key 'darwin.x86_64'
    Assert-Equal 'unknown key yields nothing' 0 $missing.Count

    Assert-Equal 'maps a res path to a local path' 'bin/linux-x64/vlc' `
        (Get-AddonRelativePath -ResPath 'res://addons/godot-vlc/bin/linux-x64/vlc' -Prefix 'res://addons/godot-vlc/')

    # --- ldd ---------------------------------------------------------------
    $ldd = @(
        '        libvlccore.so.9 => /addon/bin/linux-x64/libvlccore.so.9 (0x00007f00)'
        '        libavformat.so.60 => not found'
        '        libm.so.6 => /usr/lib/libm.so.6 (0x00007f01)'
        '        linux-vdso.so.1 (0x00007ffe)'
        '        /usr/lib64/ld-linux-x86-64.so.2 (0x00007f02)'
        ''
        'ERROR: you do not have execution permission for `./x.so'''
    )
    $entries = Get-LddEntry -Output $ldd
    Assert-Equal 'parses every ldd resolution shape' 5 $entries.Count
    Assert-Equal 'marks an unresolved library' $false ($entries | Where-Object Name -eq 'libavformat.so.60').Found
    Assert-Equal 'resolves an absolute loader path' '/usr/lib64/ld-linux-x86-64.so.2' `
        ($entries | Where-Object Name -eq 'ld-linux-x86-64.so.2').Resolved
    Assert-Equal 'ignores ldd chatter' 0 ($entries | Where-Object Name -eq 'ERROR:').Count

    # --- PE imports --------------------------------------------------------
    $objdump = @(
        '        DLL Name: libvlccore.dll'
        '        DLL Name: KERNEL32.dll'
        '        DLL Name: libgcc_s_seh-1.dll'
        ' DLL name: libavcodec_plugin.dll'
        '        vma: 0x00000000'
    )
    $pe = Get-PeImportEntry -Output $objdump
    Assert-Equal 'parses PE imports' 3 $pe.Count
    Assert-Equal 'ignores the object own DLL name line (case-sensitive)' 0 `
        ($pe | Where-Object Name -eq 'libavcodec_plugin.dll').Count

    $index = @{ 'libvlccore.dll' = 'bin/win-x64/libvlccore.dll' }
    $peResolved = Resolve-PeImportEntry -Entries $pe -AddonDllIndex $index -AddonRoot '/addon'
    Assert-Equal 'resolves a shipped DLL to its path inside the addon' '/addon/bin/win-x64/libvlccore.dll' `
        ($peResolved | Where-Object Name -eq 'libvlccore.dll').Resolved
    Assert-Equal 'leaves a system DLL unresolved' '' `
        ($peResolved | Where-Object Name -eq 'KERNEL32.dll').Resolved

    # --- dependency rules --------------------------------------------------
    $hostPatterns = @('libc.so*', 'libm.so*', 'ld-linux*.so*', 'linux-vdso.so*')
    $declared = @('bin/linux-x64/libvlccore.so.9', 'bin/linux-x64/vlc')

    $clean = @(
        [pscustomobject]@{ Name = 'libvlccore.so.9'; Resolved = '/addon/bin/linux-x64/libvlccore.so.9'; Found = $true }
        [pscustomobject]@{ Name = 'libm.so.6'; Resolved = '/usr/lib/libm.so.6'; Found = $true }
        [pscustomobject]@{ Name = 'linux-vdso.so.1'; Resolved = ''; Found = $true }
        [pscustomobject]@{ Name = 'libvlc_pulse.so.0'; Resolved = '/addon/bin/linux-x64/vlc/libvlc_pulse.so.0'; Found = $true }
    )
    Assert-Equal 'accepts a self-contained plugin' 0 `
        (Test-DependencyEntries -Entries $clean -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    $unresolved = @([pscustomobject]@{ Name = 'libavformat.so.60'; Resolved = ''; Found = $false })
    Assert-Equal 'rejects an unresolved dependency' 1 `
        (Test-DependencyEntries -Entries $unresolved -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    $notShipped = @([pscustomobject]@{ Name = 'libvpx.so.9'; Resolved = ''; Found = $true })
    Assert-Equal 'rejects a dependency that is neither shipped nor host-provided' 1 `
        (Test-DependencyEntries -Entries $notShipped -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    $libraryLeak = @([pscustomobject]@{ Name = 'libidn.so.12'; Resolved = '/addon/bin/linux-x64/libidn.so.12'; Found = $true })
    Assert-Equal 'rejects a shipped library missing from the manifest' 1 `
        (Test-DependencyEntries -Entries $libraryLeak -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    # --- manifest coverage -------------------------------------------------
    Assert-Equal 'accepts a file under a declared directory' 0 `
        (Test-ManifestCoverage -PresentPaths @('bin/linux-x64/vlc') -ExtensionPaths @() -DeclaredPaths $declared).Count
    Assert-Equal 'rejects an undeclared file' 1 `
        (Test-ManifestCoverage -PresentPaths @('bin/linux-x64/libidn.so.12') -ExtensionPaths @() -DeclaredPaths $declared).Count
    Assert-Equal 'accepts a declared extension library' 0 `
        (Test-ManifestCoverage -PresentPaths @('bin/linux-x64/libgodot_vlc.so') `
            -ExtensionPaths @('bin/linux-x64/libgodot_vlc.so') -DeclaredPaths $declared).Count

    # --- inspected coverage ------------------------------------------------
    # A fixture on disk, because the check is about what was copied rather than
    # about what the manifest says.
    $coverageRoot = Join-Path ([System.IO.Path]::GetTempPath()) "godot-vlc-coverage-$([guid]::NewGuid().ToString('N'))"
    try {
        New-Item -Path (Join-Path $coverageRoot 'bin/linux-x64/vlc/plugins/codec') -ItemType Directory -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $coverageRoot 'bin/linux-x64/vlc/plugins/codec/x.so') -Value 'x' -NoNewline
        New-Item -Path (Join-Path $coverageRoot 'bin/linux-x64/libexec') -ItemType Directory -Force | Out-Null

        Assert-Equal 'accepts a declared directory that was populated' 0 `
            (Test-InspectedCoverage -DeclaredDirectories @('bin/linux-x64/vlc') `
                -InspectedPaths @('bin/linux-x64/vlc/plugins/codec/x.so') -AddonRoot $coverageRoot).Count
        Assert-Equal 'accepts a declared data directory with no shared objects' 0 `
            (Test-InspectedCoverage -DeclaredDirectories @('bin/linux-x64/vlc') `
                -InspectedPaths @() -AddonRoot $coverageRoot).Count
        Assert-Equal 'rejects a declared directory that was never populated' 1 `
            (Test-InspectedCoverage -DeclaredDirectories @('bin/linux-x64/libexec') `
                -InspectedPaths @() -AddonRoot $coverageRoot).Count
        Assert-Equal 'rejects the directory itself being absent' 1 `
            (Test-InspectedCoverage -DeclaredDirectories @('bin/linux-x64/share') `
                -InspectedPaths @() -AddonRoot $coverageRoot).Count
    } finally {
        Remove-Item -Recurse -Force -LiteralPath $coverageRoot -ErrorAction SilentlyContinue
    }

    if ($script:selfTestFailures -ne 0) {
        Write-Host "self-test: $($script:selfTestFailures) case(s) failed" -ForegroundColor Red
        exit 1
    }
    Write-Host 'self-test: all cases passed'
}

#endregion Self-test

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

#region Main

$repoRoot = Split-Path -Parent $PSScriptRoot
$gdextensionAbs = Join-Path $repoRoot $GdextensionPath
$addonRoot = Join-Path $repoRoot (Split-Path -Parent $GdextensionPath.Replace('\', '/'))

if (-not (Test-Path -LiteralPath $gdextensionAbs)) { throw "'$GdextensionPath' not found." }

$manifestLines = Get-Content -LiteralPath $gdextensionAbs

function Read-HostProvidedPatterns {
    param([Parameter(Mandatory)][string]$RelativePath)

    $absolute = Join-Path $repoRoot $RelativePath
    if (-not (Test-Path -LiteralPath $absolute)) {
        throw "'$RelativePath' not found; the closure check cannot tell which libraries the host is expected to provide."
    }
    @(
        Get-Content -LiteralPath $absolute |
            ForEach-Object { $_.Trim() } |
            Where-Object { $_ -and -not $_.StartsWith('#') }
    )
}

$hostPatterns = @{
    'linux-x64' = Read-HostProvidedPatterns -RelativePath $HostProvidedListPath
    'win-x64'   = Read-HostProvidedPatterns -RelativePath $HostProvidedDllListPath
}

# Windows builds ship the PE tools with LLVM; binutils is not guaranteed there.
$peTool = @('llvm-objdump', 'objdump') |
    ForEach-Object { Get-Command $_ -ErrorAction SilentlyContinue } |
    Select-Object -First 1
$lddAvailable = [bool](Get-Command ldd -ErrorAction SilentlyContinue)

$addonRootPosix = $addonRoot.Replace('\', '/')
$allViolations = @()

foreach ($platform in $Platforms) {
    Write-Host ''
    Write-Host "checking $platform"

    $keys = Get-PlatformManifestKeys -Platform $platform
    $extensionPaths = @()
    foreach ($libraryKey in $keys.Libraries) {
        $extensionPaths += Get-GdextensionPaths -Lines $manifestLines -Section 'libraries' -Key $libraryKey |
            ForEach-Object { Get-AddonRelativePath -ResPath $_ -Prefix $AddonPrefix }
    }

    $declaredPaths = @(
        Get-GdextensionPaths -Lines $manifestLines -Section 'dependencies' -Key $keys.Dependencies |
            ForEach-Object { Get-AddonRelativePath -ResPath $_ -Prefix $AddonPrefix }
    )

    if ($declaredPaths.Count -eq 0) {
        $allViolations += "$platform has no [dependencies] entry ('$($keys.Dependencies)'); the runtime would not be exported"
    }

    $platformDir = Join-Path $addonRoot "bin/$platform"
    if (-not (Test-Path -LiteralPath $platformDir)) {
        $allViolations += "${platform}: 'bin/$platform' does not exist; run assemble_addon.ps1 first"
        continue
    }

    # 1. Declared paths must exist on disk.
    foreach ($declared in $declaredPaths) {
        $absolute = Join-Path $addonRoot ($declared -replace '/', [System.IO.Path]::DirectorySeparatorChar)
        if (-not (Test-Path -LiteralPath $absolute)) {
            $allViolations += "${platform}: declared in the manifest but missing on disk: $declared"
        }
    }

    # 2. Everything present must be declared.
    $presentPaths = @(
        Get-ChildItem -LiteralPath $platformDir -Force |
            ForEach-Object { "bin/$platform/$($_.Name)" }
    )
    $allViolations += Test-ManifestCoverage -PresentPaths $presentPaths `
        -ExtensionPaths $extensionPaths -DeclaredPaths $declaredPaths |
        ForEach-Object { "${platform}: $_" }

    # 3. Closure.
    $declaredDirectories = @(
        $declaredPaths | Where-Object {
            Test-Path -LiteralPath (Join-Path $addonRoot ($_ -replace '/', [System.IO.Path]::DirectorySeparatorChar)) -PathType Container
        } | ForEach-Object { "bin/$platform/$([System.IO.Path]::GetFileName($_))" }
    )

    $pattern = if ($platform -eq 'linux-x64') { '*.so*' } else { '*.dll' }
    $sharedObjects = @(
        Get-ChildItem -LiteralPath $platformDir -Recurse -Force -File |
            Where-Object { $_.Name -like $pattern }
    )
    $inspectedPaths = @(
        $sharedObjects | ForEach-Object {
            "bin/$platform/$($_.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
        }
    )

    $allViolations += Test-InspectedCoverage -DeclaredDirectories $declaredDirectories -InspectedPaths $inspectedPaths -AddonRoot $addonRoot |
        ForEach-Object { "${platform}: $_" }

    if ($platform -eq 'linux-x64') {
        if (-not $lddAvailable) {
            $allViolations += "${platform}: ldd is unavailable, so the dependency closure could not be verified"
        } else {
            foreach ($sharedObject in $sharedObjects) {
                $entries = Get-LddEntry -Output (& ldd $sharedObject.FullName 2>&1)
                $relative = "bin/$platform/$($sharedObject.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
                $allViolations += Test-DependencyEntries -Entries $entries `
                    -AddonRoot $addonRootPosix `
                    -DeclaredPaths $declaredPaths `
                    -HostProvidedPatterns $hostPatterns[$platform] `
                    -Source $relative |
                    ForEach-Object { "${platform}: $_" }
            }
        }
    } else {
        if (-not $peTool) {
            $allViolations += "${platform}: neither llvm-objdump nor objdump is available, so the import closure could not be verified"
        } else {
            # Index every DLL the addon ships so imports can be attributed to it.
            $dllIndex = @{}
            foreach ($dll in $sharedObjects) {
                $relative = "bin/$platform/$($dll.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
                $dllIndex[$dll.Name.ToLowerInvariant()] = $relative
            }

            foreach ($sharedObject in $sharedObjects) {
                $entries = Get-PeImportEntry -Output (& $peTool.Source -p $sharedObject.FullName 2>&1)
                $entries = Resolve-PeImportEntry -Entries $entries -AddonDllIndex $dllIndex -AddonRoot $addonRootPosix
                $relative = "bin/$platform/$($sharedObject.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
                $allViolations += Test-DependencyEntries -Entries $entries `
                    -AddonRoot $addonRootPosix `
                    -DeclaredPaths $declaredPaths `
                    -HostProvidedPatterns $hostPatterns[$platform] `
                    -Source $relative |
                    ForEach-Object { "${platform}: $_" }
            }
        }
    }

    Write-Host "  inspected $($inspectedPaths.Count) shared object(s)"
}

Write-Host ''
if ($allViolations.Count -gt 0) {
    Write-Host "FAILED with $($allViolations.Count) violation(s):" -ForegroundColor Red
    $allViolations | ForEach-Object { Write-Host "  - $_" }

    # On GitHub Actions, each violation also becomes an annotation. The log needs
    # a signed-in reader, and annotations do not, so a failure reports itself
    # instead of requiring somebody to go and copy the text out.
    if ($env:GITHUB_ACTIONS) {
        foreach ($violation in $allViolations) {
            $escaped = $violation -replace '%', '%25' -replace "`r", '%0D' -replace "`n", '%0A'
            Write-Host "::error::$escaped"
        }
    }

    Write-Host ''
    Write-Host 'A host library in the list means the build did not internalise it; prefer fixing the'
    Write-Host 'build over extending the host-provided list. A file that is present but undeclared'
    Write-Host 'means the manifest and the runtime have drifted apart.'
    exit 1
}

Write-Host 'check_addon: OK' -ForegroundColor Green

#endregion Main
