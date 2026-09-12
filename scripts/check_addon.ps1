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
     declared) or in the host-provided list for that platform:

       Linux    scripts/host-provided-libs.txt
       Windows  scripts/host-provided-dlls.txt
       Android  scripts/host-provided-libs-android.txt

     This is what turns "we believe the runtime is self-contained" into an
     assertion.

  3. DEBUG INFORMATION -- no shared object in the shipped payload may carry
     debug information. A section nothing ever reads is not a feature; it is
     download weight, and a forgotten strip is invisible in behaviour and
     enormous in bytes. Measured on the payload this check was added for, DWARF
     was 666.0 MB of the 798.7 MB linux-x64 runtime and 827.1 MB of the
     1025.7 MB win-x64 one, against 0 bytes in the stripped android-arm64 one:
     five times the download, for information no user can act on. The strip
     happens in build/vlc/postprocess.ps1, and this asserts the outcome at the
     addon boundary so that any layer which forgets it fails loudly here
     instead of quietly multiplying what users download.

     One exception is carved out deliberately: the extension library the
     manifest declares as a debug variant (`*.debug.*` in [libraries]). That
     file is this repository's own cargo debug build rather than the vendored
     runtime, and its debug information is the point of the artifact.

The three platforms do not share an inspector, because they do not share a
binary format or a runtime shape:

  * Windows uses `llvm-objdump -p` (or objdump -p) and the PE import table.
  * Linux uses ldd, which resolves each dependency through the host loader.
  * Android uses `llvm-readelf -d` (or readelf -d) and the ELF DT_NEEDED tags.

Android cannot use ldd. The payload is an AArch64 Android ELF and ldd resolves a
binary by running it: on Windows ldd cannot read the file at all, and on Linux it
would resolve against the host's glibc, which says nothing about what an Android
device provides. DT_NEEDED is a static read and is therefore the honest check.

Android also has a different runtime shape, which is why it needs its own
host-provided list rather than the Linux one: its LibVLC build is monolithic.
There is one libvlc.so with every module linked into it, no plugins/ directory and
no libvlccore.so, so the entire closure is a handful of platform libraries on a
single file plus the one dependency the extension has on the runtime shipped
beside it.

The debug-information check splits by file format rather than by platform: both
ELF payloads (linux-x64, android-arm64) are read with `llvm-readelf -S` (or
readelf -S) and the PE payload with `llvm-objdump -h` (or objdump -h).

Every check refuses to report success when it could not actually run: a missing
inspector, a shared object the inspector could not read, a declared directory
that yielded no shared objects, a section table the inspector never printed, or a
section table that parsed to zero sections, is a failure rather than a pass. The
previous revision of this script reported "0 shared objects inspected" and
exited 0, because Get-ChildItem -Recurse does not descend directory symlinks.

Run scripts/assemble_addon.ps1 first: this inspects the assembled addon, not the
source tree, because the assembled addon is what users receive.

.PARAMETER Platforms
Platforms to check. Defaults to the two desktop platforms; android-arm64 has to
be requested explicitly, so a machine without the Android payload does not fail
the desktop gate.

.PARAMETER SelfTest
Exercises the parsing and comparison logic against synthetic inputs and exits.
The self-test needs no addon, no inspector and no Linux, so it runs anywhere.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64', 'android-arm64')]
    [string[]]$Platforms = @('win-x64', 'linux-x64'),

    [string]$GdextensionPath = 'gdextension_template/godot_vlc.gdextension',
    [string]$HostProvidedListPath = 'scripts/host-provided-libs.txt',
    [string]$HostProvidedDllListPath = 'scripts/host-provided-dlls.txt',
    [string]$HostProvidedAndroidListPath = 'scripts/host-provided-libs-android.txt',
    [string]$ForbiddenListPath = 'scripts/forbidden-in-addon.txt',
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

# Parses `llvm-readelf -d` / `readelf -d` output into { Name, Resolved, Found },
# the same entry shape Get-LddEntry produces, so Test-DependencyEntries can
# consume it unchanged.
#
# Input shape -- one dynamic-section table, where only the NEEDED rows name a
# dependency:
#
#   Dynamic section at offset 0x3127440 contains 34 entries:
#     Tag                Type           Name/Value
#     0x0000000000000001 (NEEDED)       Shared library: [libEGL.so]
#     0x000000000000000e (SONAME)       Library soname: [libvlc.so]
#     0x000000000000001e (FLAGS)        SYMBOLIC BIND_NOW
#     0x0000000000000019 (INIT_ARRAY)   0x312b1e0
#
# The literal (NEEDED) tag is matched rather than "any line carrying a bracketed
# name", because SONAME describes the object itself: taking it for a dependency
# would make every Android library depend on its own name, and libvlc.so would
# then have to be justified in the host-provided list to satisfy a check about
# what the host must supply. FLAGS, RELA, INIT_ARRAY, VERSYM, VERNEED and the
# rest name no library at all.
#
# Every entry comes back with an empty Resolved and Found = $true. That is not a
# claim that the library was found; it is the same state Get-PeImportEntry
# produces for a PE import, and it means "cannot be located from here, so it is
# legitimate only if it is shipped inside the addon or justified in the
# host-provided list". Neither can be decided here -- an AArch64 Android ELF
# cannot be resolved by the host's loader -- so the caller supplies that context.
function Get-ElfNeededEntry {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    $entries = @()
    foreach ($line in $Output) {
        if ($line -match '\(NEEDED\)\s+Shared library:\s*\[(?<name>[^\]]+)\]') {
            $entries += [pscustomobject]@{ Name = $Matches['name']; Resolved = ''; Found = $true }
        }
    }
    @($entries)
}

# Parses section names out of `llvm-readelf -S` / `readelf -S` output.
#
# Input shape -- a section headers table, one section per row:
#
#   There are 6 section headers, starting at offset 0x81c:
#
#   Section Headers:
#     [Nr] Name              Type            Address          Off    Size   ES Flg Lk Inf Al
#     [ 0]                   NULL            0000000000000000 000000 000000 00      0   0  0
#     [ 1] .dynsym           DYNSYM          0000000000000308 000308 000af8 18   A  6   1  8
#     [ 3] .debug_info       PROGBITS        0000000000000000 020000 061ca5 00      0   0  1
#
# binutils prints the same table two lines per section (address and offset on the
# first line, size and alignment on the second); LLVM prints one line per
# section. Requiring an index, a name, a type and an address on the *same* line
# matches exactly one row per section in both layouts: the second line of a
# binutils entry carries no index, and the column header row's index column is
# the literal "Nr" rather than a number.
#
# The type token is required to start with a letter, or to be a `0x`-prefixed
# number (which is how both tools render a section type they have no name for).
# That is what makes section 0 -- whose name is empty, so its row is an
# all-whitespace field followed by the type "NULL" -- come back with an empty
# name instead of with "NULL" taken for the name: a name of "NULL" would leave
# the *address* in the type column, and an address is neither letter-initial nor
# 0x-prefixed. The distinction is invisible to a check that matches a name
# prefix, but it is what lets this parser count rows, and the caller compares
# that count against Get-ElfSectionCount's summary line. A regex that silently
# stopped matching rows would otherwise drop a `.debug_*` section the same way
# the addon would quietly drop a strip step, which is the failure mode this file
# exists to make loud.
#
# The name is deliberately not bounded to its column width. Both tools truncate
# a long name (`[...]`) when not asked for wide output, and they do not agree on
# the truncated width -- binutils pads to 17 columns, LLVM's padding has varied
# between releases -- so a bound would have to be either too tight (dropping
# rows) or meaningless. An unbounded name cannot hide a section either way: a
# `.debug_str_offsets` printed as `.debug_str_o[...]` still starts with
# `.debug`, and the match in Get-DebugSection is a prefix match.
function Get-ElfSectionName {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    $names = @()
    foreach ($line in $Output) {
        if ($line -match '^\s*\[\s*\d+\]\s(?<name>\S*)\s+(?:[A-Za-z_][A-Za-z0-9_]*|0x[0-9a-fA-F]+)\s+[0-9a-fA-F]{8,}') {
            $names += $Matches['name']
        }
    }
    @($names)
}

# The number of section headers readelf says it printed, from the summary line
# above the table, or -1 when it printed no such line:
#
#   There are 28 section headers, starting at offset 0x2cc7fc0:
#
# This is the honest half of the parse: a table that claims 28 sections and
# yields 3 rows means the regex above has stopped matching the tool's layout, and
# the sections it failed to match are exactly the ones a check must not skip. The
# caller fails on a disagreement rather than trusting the short list.
#
# Reading it line by line rather than with `$Output -match` is deliberate:
# PowerShell does not populate $Matches when -match is applied to a collection,
# so an array match would silently report 0.
function Get-ElfSectionCount {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    foreach ($line in $Output) {
        if ($line -match 'There are (?<count>\d+) section headers') { return [int]$Matches['count'] }
    }
    -1
}

# Parses section names out of `llvm-objdump -h` / `objdump -h` output.
#
# Input shape -- a header row, then one row per section:
#
#   libvlc.dll:	file format coff-x86-64
#
#   Sections:
#   Idx Name            Size     VMA              Type
#     0 .text           00016b80 0000000140001000 TEXT, DATA
#    13 .debug_info     00061ca5 000000014002d000 DATA, DEBUG
#
# binutils prints a wider table (an extra LMA column, File off and Algn) and a
# trailing flags line with no index:
#
#   Idx Name          Size      VMA               LMA               File off  Algn
#     0 .text         00016b80  0000000140001000  0000000140001000  00000200  2**4
#                     CONTENTS, ALLOC, LOAD, READONLY, CODE
#
# Requiring a row to start with an index, then a name, then two hexadecimal
# fields covers both and rejects both the header row and the flags line. PE
# section names are at most 8 bytes and are never truncated, unlike readelf's
# 17-column names.
function Get-PeSectionName {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    $names = @()
    foreach ($line in $Output) {
        if ($line -match '^\s+(?<index>\d+)\s+(?<name>\S+)\s+[0-9a-fA-F]+\s+[0-9a-fA-F]+') {
            $names += $Matches['name']
        }
    }
    @($names)
}

# The subset of a section table that carries debug information.
#
# Matched by prefix rather than by equality for two reasons. The DWARF section
# family is open-ended -- .debug_info, .debug_abbrev, .debug_line,
# .debug_line_str, .debug_str_offsets, .debug_addr, .debug_rnglists and whatever
# the next revision adds -- and an exhaustive list would silently miss the next
# one. And a long name comes back truncated from the tools' columns, so an
# equality test against the full name would match nothing at all.
#
# `.zdebug*` is the same information after `objcopy
# --compress-debug-sections`: smaller, and still megabytes of debug data that no
# consumer can use. The rule is about what a download weighs, and a compressed
# section is not a stripped one.
#
# The comparison is case-sensitive (-clike) because section names are: a
# hypothetical `.DEBUG_INFO` is a different section from `.debug_info`, and the
# tools that put DWARF in a PE (mingw) spell it in lower case.
function Get-DebugSection {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Names)

    $debug = @()
    foreach ($name in $Names) {
        if ($name -clike '.debug*' -or $name -clike '.zdebug*') { $debug += $name }
    }
    @($debug)
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

# Rewrites the ELF entries that name a library the addon itself ships, so the
# shared dependency rules apply to them -- the ELF counterpart of
# Resolve-PeImportEntry.
#
# This is what stops the Android extension libraries from being reported as host
# dependencies. They link the runtime that ships beside them, so
# libgodot_vlc.so has DT_NEEDED libvlc.so; with every NEEDED name left
# unresolved, the gate would demand that libvlc.so be justified in
# host-provided-libs-android.txt, and the only way to silence it would be to
# declare the shipped runtime a host library. That is precisely the conflation
# this file exists to prevent: libvlc.so is an addon file, declared in
# [dependencies] as android.arm64, and the gate has to check it is still
# packaged and still declared rather than wave it through.
#
# Names are compared case-sensitively (-ceq) because the Android loader that will
# resolve them is case-sensitive; the Windows index lowercases for the mirror
# reason. $Shipped is an array of { Name, Relative } rather than a lookup table
# for the same reason: PowerShell's @{} is case-insensitive.
function Resolve-ElfNeededEntry {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Entries,
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Shipped,
        [Parameter(Mandatory)][string]$AddonRoot
    )

    $root = $AddonRoot.Replace('\', '/').TrimEnd('/')
    $resolved = @()
    foreach ($entry in $Entries) {
        # Not named $shipped: PowerShell variable names are case-insensitive, so
        # that would silently overwrite the $Shipped parameter before the loop.
        $match = $null
        foreach ($candidate in $Shipped) {
            if ($candidate.Name -ceq $entry.Name) { $match = $candidate; break }
        }

        if ($match) {
            $resolved += [pscustomobject]@{
                Name     = $entry.Name
                Resolved = "$root/$($match.Relative)"
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
            # A library the host is expected to provide does not have to be
            # installed on the machine running this check. A headless CI runner has
            # no libxkbcommon-x11, and that says nothing about the addon; what has
            # to be rejected is a library that is neither shipped nor justified,
            # which is exactly what the runtime this replaces depended on.
            if (-not (Test-HostProvided -Name $entry.Name -Patterns $HostProvidedPatterns)) {
                $violations += "unresolved dependency: $($entry.Name)$where"
            }
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

# Files that only exist to build something, and so must never be shipped.
#
# A directory declared in the manifest declares its whole subtree, which is what
# keeps the manifest readable, and the consequence is that anything nested inside
# it goes unchecked. That is how 766 libtool and import-library files reached the
# assembled addon from the mingw module tree without this script noticing: they
# were inside a declared directory rather than at the top level. This is the check
# for that class, and it walks the whole platform directory rather than only its
# declared entries.
function Test-ForbiddenNames {
    <#
    .SYNOPSIS
    Every path whose file name matches a forbidden pattern.

    .DESCRIPTION
    Takes paths relative to the platform directory, so the message names the file
    as it appears in the addon. Matched against the file name, so a pattern
    applies at any depth.
    #>
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Paths,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$ForbiddenPatterns
    )

    $violations = @()
    foreach ($path in $Paths) {
        $name = [System.IO.Path]::GetFileName($path)
        foreach ($pattern in $ForbiddenPatterns) {
            if ($name -like $pattern) {
                $violations += "shipped a file that only exists to build something: $path (matches '$pattern')"
                break
            }
        }
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

    # --- ELF DT_NEEDED (Android) -------------------------------------------
    # The real shape of `llvm-readelf -d` on the assembled Android runtime,
    # including every non-library tag the parser has to leave alone. SONAME is
    # the one that matters: it names the object itself, so treating it as a
    # dependency would make libvlc.so depend on libvlc.so.
    $readelf = @(
        ''
        'Dynamic section at offset 0x3127440 contains 34 entries:'
        '  Tag                Type           Name/Value'
        '  0x0000000000000001 (NEEDED)       Shared library: [libEGL.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libGLESv2.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libm.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [liblog.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libc.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libdl.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libandroid.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libmediandk.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libc++_shared.so]'
        '  0x000000000000000e (SONAME)       Library soname: [libvlc.so]'
        '  0x000000000000001e (FLAGS)        SYMBOLIC BIND_NOW '
        '  0x000000006ffffffb (FLAGS_1)      NOW '
        '  0x0000000000000007 (RELA)         0x29e4a0'
        '  0x0000000000000019 (INIT_ARRAY)   0x312b1e0'
        '  0x0000000000000014 (PLTREL)       RELA'
        '  0x000000006ffffff0 (VERSYM)       0xdef58'
        '  0x000000006ffffffe (VERNEED)      0xf1860'
        '  0x0000000000000000 (NULL)         0x0'
    )
    $elf = Get-ElfNeededEntry -Output $readelf
    Assert-Equal 'parses every DT_NEEDED entry' 9 $elf.Count
    Assert-Equal 'extracts DT_NEEDED names in order and nothing else' `
        'libEGL.so | libGLESv2.so | libm.so | liblog.so | libc.so | libdl.so | libandroid.so | libmediandk.so | libc++_shared.so' `
        ($elf | ForEach-Object Name)
    Assert-Equal 'ignores SONAME rather than making the object its own dependency' 0 `
        ($elf | Where-Object Name -eq 'libvlc.so').Count
    Assert-Equal 'ignores FLAGS, RELA and the other non-library dynamic tags' 0 `
        ($elf | Where-Object { $_.Name -notlike '*.so' }).Count
    Assert-Equal 'leaves a NEEDED name unlocated for the caller to attribute' '' $elf[0].Resolved

    # The extension libraries link the runtime shipped beside them, which is the
    # shape that must not be reported as a host dependency.
    $extension = Get-ElfNeededEntry -Output @(
        '  0x0000000000000001 (NEEDED)       Shared library: [libvlc.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libdl.so]'
        '  0x0000000000000001 (NEEDED)       Shared library: [libc.so]'
    )
    $shipped = @([pscustomobject]@{ Name = 'libvlc.so'; Relative = 'bin/android-arm64/libvlc.so' })
    $extensionResolved = Resolve-ElfNeededEntry -Entries $extension -Shipped $shipped -AddonRoot '/addon'
    Assert-Equal 'attributes a shipped Android runtime to its path inside the addon' `
        '/addon/bin/android-arm64/libvlc.so' ($extensionResolved | Where-Object Name -eq 'libvlc.so').Resolved
    Assert-Equal 'leaves an Android platform library unlocated' '' `
        ($extensionResolved | Where-Object Name -eq 'libc.so').Resolved

    # Case matters on Android, so a differently-cased shipped name is a
    # different library and must not be resolved to the addon.
    $wrongCase = @([pscustomobject]@{ Name = 'LIBVLC.SO'; Relative = 'bin/android-arm64/LIBVLC.SO' })
    Assert-Equal 'matches a soname case-sensitively' '' `
        (Resolve-ElfNeededEntry -Entries $extension -Shipped $wrongCase -AddonRoot '/addon' |
            Where-Object Name -eq 'libvlc.so').Resolved

    # --- ELF section tables (debug information) ----------------------------
    # `llvm-readelf -S`, which prints one line per section. The fixture carries
    # DWARF and a symtab, which is the shape the rule has to reject.
    $elfSections = @(
        ''
        'There are 6 section headers, starting at offset 0x81c:'
        ''
        'Section Headers:'
        '  [Nr] Name              Type            Address          Off    Size   ES Flg Lk Inf Al'
        '  [ 0]                   NULL            0000000000000000 000000 000000 00      0   0  0'
        '  [ 1] .dynsym           DYNSYM          0000000000000308 000308 000af8 18   A  6   1  8'
        '  [ 2] .text             PROGBITS        0000000000010000 010000 016b80 00  AX  0   0 16'
        '  [ 3] .debug_info       PROGBITS        0000000000000000 020000 061ca5 00      0   0  1'
        '  [ 4] .debug_abbrev     PROGBITS        0000000000000000 081ca5 000898 00      0   0  1'
        '  [ 5] .symtab           SYMTAB          0000000000000000 08253d 000c30 18      6 640  8'
    )
    $elfNames = Get-ElfSectionName -Output $elfSections
    Assert-Equal 'parses every row of an llvm-readelf -S table' 6 $elfNames.Count
    Assert-Equal 'extracts ELF section names and leaves the empty NULL name empty' `
        ',.dynsym,.text,.debug_info,.debug_abbrev,.symtab' ($elfNames -join ',')
    Assert-Equal 'flags the DWARF sections of an ELF' '.debug_info,.debug_abbrev' `
        ((Get-DebugSection -Names $elfNames) -join ',')

    $elfStripped = @(
        ''
        'There are 3 section headers, starting at offset 0x1a4:'
        ''
        'Section Headers:'
        '  [Nr] Name              Type            Address          Off    Size   ES Flg Lk Inf Al'
        '  [ 0]                   NULL            0000000000000000 000000 000000 00      0   0  0'
        '  [ 1] .dynsym           DYNSYM          0000000000000238 000238 000090 18   A  2   1  8'
        '  [ 2] .text             PROGBITS        0000000000010000 010000 016b80 00  AX  0   0 16'
    )
    Assert-Equal 'accepts a stripped ELF' 0 `
        (Get-DebugSection -Names (Get-ElfSectionName -Output $elfStripped)).Count

    # binutils prints the same table as two lines per section and truncates a
    # name to 17 columns, so a long DWARF name arrives shortened --
    # `.debug_str_offsets` becomes `.debug_str_o[...]` on this payload. The
    # match has to be a prefix match, or the gate would stop seeing exactly the
    # sections it exists for. `.zdebug_*` is the same data after
    # --compress-debug-sections: smaller, and still not stripped.
    $elfTruncated = @(
        'There are 4 section headers, starting at offset 0x81c:'
        ''
        'Section Headers:'
        '  [Nr] Name              Type             Address           Offset'
        '       Size              EntSize          Flags  Link  Info  Align'
        '  [ 0]                   NULL             0000000000000000  00000000'
        '       0000000000000000  0000000000000000           0     0     0'
        '  [ 1] .debug_str_o[...] PROGBITS         0000000000000000  02000000'
        '       0000000000001234  0000000000000000           0     0     1'
        '  [ 2] .zdebug_line      PROGBITS         0000000000000000  03000000'
        '       0000000000000400  0000000000000000           0     0     1'
        '  [ 3] .text             PROGBITS         0000000000010000  01000000'
        '       0000000000016b80  0000000000000000  AX       0     0    16'
    )
    $truncatedNames = Get-ElfSectionName -Output $elfTruncated
    Assert-Equal 'parses a two-line-per-section binutils table once per section' 4 $truncatedNames.Count
    Assert-Equal 'flags a DWARF name the tool truncated to its column' 1 `
        @(Get-DebugSection -Names $truncatedNames | Where-Object { $_ -clike '.debug*' }).Count
    Assert-Equal 'flags compressed debug sections' 1 `
        @(Get-DebugSection -Names $truncatedNames | Where-Object { $_ -clike '.zdebug*' }).Count

    # The guard the main pass relies on: a table the tool printed but that
    # parses to no rows at all is a parser/tool mismatch, not a clean object,
    # and must fail rather than pass.
    $elfNoRows = @(
        ''
        'There are 1 section headers, starting at offset 0x40:'
        ''
        'Section Headers:'
        '  [Nr] Name              Type            Address          Off    Size   ES Flg Lk Inf Al'
    )
    Assert-Equal 'parses no section out of a section table with no rows' 0 `
        (Get-ElfSectionName -Output $elfNoRows).Count

    # The summary line readelf prints above its table is the honest half of the
    # parse, and the caller fails when it disagrees with the rows parsed: a
    # parser that stopped matching rows would drop exactly the `.debug_*`
    # sections this check exists for.
    Assert-Equal 'reads the section count readelf declares' 6 `
        (Get-ElfSectionCount -Output $elfSections)
    Assert-Equal 'reports no declared count when the tool printed no summary line' -1 `
        (Get-ElfSectionCount -Output @('', 'Section Headers:'))

    $elfPartial = @(
        ''
        'There are 4 section headers, starting at offset 0x40:'
        ''
        'Section Headers:'
        '  [Nr] Name              Type            Address          Off    Size   ES Flg Lk Inf Al'
        '  [ 1] .text             PROGBITS        0000000000010000 010000 016b80 00  AX  0   0 16'
        '  [ 2] .vendor          0x70000001      0000000000020000 020000 000010 00      0   0  8'
        '  [ 3] .debug_info      PROGBITS        0x2000000 0x61ca5'
        '  [ 4] .debug_abbrev    PROGBITS        0x81ca5 0x898'
    )
    Assert-Equal 'parses a row whose section type readelf has no name for' '.text,.vendor' `
        ((Get-ElfSectionName -Output $elfPartial) -join ',')
    # Rows 3 and 4 are deliberately in a layout this parser does not recognise
    # (0x-prefixed addresses), which is what a tool version change looks like:
    # the two numbers disagreeing is the whole signal, and the caller turns that
    # into a failure rather than reading the short list as a clean object.
    Assert-Equal 'exposes a short parse against the declared count' 4 `
        (Get-ElfSectionCount -Output $elfPartial)

    # --- PE section tables (debug information) -----------------------------
    # `llvm-objdump -h`, which the Windows pass prefers. The column header row
    # must not be mistaken for a section.
    $peSections = @(
        ''
        "libvlc.dll:`tfile format coff-x86-64"
        ''
        'Sections:'
        'Idx Name            Size     VMA              Type'
        '  0 .text           00016b80 0000000140001000 TEXT, DATA'
        '  1 .rdata          00003500 0000000140019000 DATA'
        ' 12 .debug_info     00061ca5 000000014002d000 DATA, DEBUG'
        ' 13 .debug_abbrev   00008980 000000014008f000 DATA, DEBUG'
        ' 14 .reloc          000000e4 000000014002b000 DATA'
    )
    $peNames = Get-PeSectionName -Output $peSections
    Assert-Equal 'parses every row of an llvm-objdump -h table' 5 $peNames.Count
    Assert-Equal 'extracts PE section names and ignores the column header' `
        '.text,.rdata,.debug_info,.debug_abbrev,.reloc' ($peNames -join ',')
    Assert-Equal 'flags the DWARF sections of a PE' '.debug_info,.debug_abbrev' `
        ((Get-DebugSection -Names $peNames) -join ',')

    $peStripped = @(
        ''
        "libvlc.dll:`tfile format coff-x86-64"
        ''
        'Sections:'
        'Idx Name            Size     VMA              Type'
        '  0 .text           00016b80 0000000140001000 TEXT, DATA'
        '  1 .data           000000b0 0000000140018000 DATA'
        '  2 .rsrc           00000808 000000014002a000 DATA'
    )
    Assert-Equal 'accepts a stripped PE' 0 `
        (Get-DebugSection -Names (Get-PeSectionName -Output $peStripped)).Count

    # binutils objdump -h, the fallback on a machine without LLVM: an extra LMA
    # column, and a flags line under each section that carries no index and so
    # must not become a section of its own.
    $binutilsSections = @(
        ''
        'libvlc.dll:     file format pei-x86-64'
        ''
        'Sections:'
        'Idx Name          Size      VMA               LMA               File off  Algn'
        '  0 .text         00016b80  0000000140001000  0000000140001000  00000200  2**4'
        '                  CONTENTS, ALLOC, LOAD, READONLY, CODE'
        '  1 .debug_info   00061ca5  000000014002d000  000000014002d000  0017c000  2**0'
        '                  CONTENTS, READONLY, DEBUGGING, EXCLUDE'
        '  2 .reloc        000000e4  000000014002b000  000000014002b000  0017ac00  2**0'
        '                  CONTENTS, ALLOC, LOAD, READONLY, DATA'
    )
    $binutilsNames = Get-PeSectionName -Output $binutilsSections
    Assert-Equal 'parses binutils objdump -h rows and ignores the flags lines' `
        '.text,.debug_info,.reloc' ($binutilsNames -join ',')
    Assert-Equal 'flags DWARF sections in binutils objdump output' '.debug_info' `
        ((Get-DebugSection -Names $binutilsNames) -join ',')
    Assert-Equal 'parses no section out of a PE table with no rows' 0 `
        (Get-PeSectionName -Output @('Sections:', 'Idx Name            Size     VMA              Type')).Count

    # --- dependency rules --------------------------------------------------
    $hostPatterns = @('libc.so*', 'libm.so*', 'ld-linux*.so*', 'linux-vdso.so*', 'libxkbcommon-x11.so*')
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

    # The distinction that matters: not found on this machine, but justified, is
    # not a violation. Not found and not justified is the defect this gate exists
    # for, and the case above covers it.
    $hostAbsent = @([pscustomobject]@{ Name = 'libxkbcommon-x11.so.0'; Resolved = ''; Found = $false })
    Assert-Equal 'accepts a host-provided library this machine does not have' 0 `
        (Test-DependencyEntries -Entries $hostAbsent -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    $notShipped = @([pscustomobject]@{ Name = 'libvpx.so.9'; Resolved = ''; Found = $true })
    Assert-Equal 'rejects a dependency that is neither shipped nor host-provided' 1 `
        (Test-DependencyEntries -Entries $notShipped -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    $libraryLeak = @([pscustomobject]@{ Name = 'libidn.so.12'; Resolved = '/addon/bin/linux-x64/libidn.so.12'; Found = $true })
    Assert-Equal 'rejects a shipped library missing from the manifest' 1 `
        (Test-DependencyEntries -Entries $libraryLeak -AddonRoot '/addon' -DeclaredPaths $declared -HostProvidedPatterns $hostPatterns).Count

    # --- Android host-provided rules ---------------------------------------
    # The closure of the assembled Android payload: the platform libraries
    # libvlc.so needs, plus the extension's dependency on the shipped runtime
    # already attributed to the addon above.
    $androidNeeded = @($elf) + @($extensionResolved)
    $androidPatterns = @(
        'libEGL.so', 'libGLESv2.so', 'libm.so', 'liblog.so', 'libc.so', 'libdl.so',
        'libandroid.so', 'libmediandk.so', 'libc++_shared.so'
    )
    $androidDeclared = @('bin/android-arm64/libvlc.so')

    Assert-Equal 'accepts a platform library the Android runtime is allowed to expect' 0 `
        (Test-DependencyEntries -Entries $androidNeeded -AddonRoot '/addon' `
            -DeclaredPaths $androidDeclared -HostProvidedPatterns $androidPatterns).Count
    Assert-Equal 'accepts the extension linking the runtime it ships beside' 0 `
        (Test-DependencyEntries -Entries ($extensionResolved | Where-Object Name -eq 'libvlc.so') -AddonRoot '/addon' `
            -DeclaredPaths $androidDeclared -HostProvidedPatterns @()).Count

    # The reverse verification the platform branch depends on: drop one entry
    # from scripts/host-provided-libs-android.txt and the gate must report a
    # violation rather than pass quietly.
    $androidMissing = $androidPatterns | Where-Object { $_ -ne 'liblog.so' }
    $androidViolations = @(Test-DependencyEntries -Entries $androidNeeded -AddonRoot '/addon' `
            -DeclaredPaths $androidDeclared -HostProvidedPatterns $androidMissing `
            -Source 'bin/android-arm64/libvlc.so')
    Assert-Equal 'rejects an Android dependency that is neither shipped nor host-provided' 1 $androidViolations.Count
    Assert-Equal 'names the Android library that lost its justification' `
        "dependency 'liblog.so' is neither shipped nor in the host-provided list (needed by bin/android-arm64/libvlc.so)" `
        $androidViolations[0]
    Assert-Equal 'rejects a shipped Android library missing from the manifest' 1 `
        (Test-DependencyEntries -Entries $extensionResolved -AddonRoot '/addon' `
            -DeclaredPaths @() -HostProvidedPatterns $androidPatterns).Count

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

    # --- forbidden names ---------------------------------------------------
    # The class that reached the addon from the mingw module tree: nested, so the
    # top-level declaration check could not see it.
    $forbidden = @('*.la', '*.a', '*.lib', '*.exp', '*.pdb')
    Assert-Equal 'accepts a plugin and its data' 0 `
        (Test-ForbiddenNames -ForbiddenPatterns $forbidden -Paths @(
                'bin/win-x64/plugins/codec/libavcodec_plugin.dll'
                'bin/win-x64/libexec/vlc/vlc-cache-gen.exe'
                'bin/win-x64/share/vlc/lua/playlist/youtube.luac')).Count
    Assert-Equal 'rejects a libtool archive beside a plugin' 1 `
        (Test-ForbiddenNames -ForbiddenPatterns $forbidden -Paths @('bin/win-x64/plugins/codec/libavcodec_plugin.la')).Count
    Assert-Equal 'rejects a mingw import library beside a plugin' 1 `
        (Test-ForbiddenNames -ForbiddenPatterns $forbidden -Paths @('bin/win-x64/plugins/codec/libavcodec_plugin.dll.a')).Count
    Assert-Equal 'rejects debug symbols' 1 `
        (Test-ForbiddenNames -ForbiddenPatterns $forbidden -Paths @('bin/win-x64/godot_vlc.pdb')).Count
    Assert-Equal 'is not fooled by a similar extension' 0 `
        (Test-ForbiddenNames -ForbiddenPatterns $forbidden -Paths @('bin/win-x64/plugins/a.so', 'bin/win-x64/plugins/a.dll', 'bin/win-x64/plugins/liblua_plugin.so')).Count
    Assert-Equal 'reports every offender, not just the first' 2 `
        (Test-ForbiddenNames -ForbiddenPatterns $forbidden -Paths @('bin/win-x64/plugins/x.la', 'bin/win-x64/plugins/y.dll.a')).Count

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
        throw "'$RelativePath' not found; a policy list this check relies on is missing."
    }
    @(
        Get-Content -LiteralPath $absolute |
            ForEach-Object { $_.Trim() } |
            Where-Object { $_ -and -not $_.StartsWith('#') }
    )
}

$hostPatterns = @{
    'linux-x64'     = Read-HostProvidedPatterns -RelativePath $HostProvidedListPath
    'win-x64'       = Read-HostProvidedPatterns -RelativePath $HostProvidedDllListPath
    'android-arm64' = Read-HostProvidedPatterns -RelativePath $HostProvidedAndroidListPath
}

$forbiddenPatterns = Read-HostProvidedPatterns -RelativePath $ForbiddenListPath

# Windows builds ship the PE tools with LLVM; binutils is not guaranteed there.
$peTool = @('llvm-objdump', 'objdump') |
    ForEach-Object { Get-Command $_ -ErrorAction SilentlyContinue } |
    Select-Object -First 1
$lddAvailable = [bool](Get-Command ldd -ErrorAction SilentlyContinue)

# The Android payload is an AArch64 Android ELF, so ldd is not an option: it
# resolves a binary by running it through the host loader, which cannot read an
# Android executable at all on Windows and would answer a different question on
# Linux. Its DT_NEEDED tags are read statically instead. llvm-readelf ships with
# the NDK and the LLVM toolchains, binutils' readelf is the alternative; if
# neither is present the Android closure is reported as unverifiable rather than
# skipped.
$elfTool = @('llvm-readelf', 'readelf') |
    ForEach-Object { Get-Command $_ -ErrorAction SilentlyContinue } |
    Select-Object -First 1

$addonRootPosix = $addonRoot.Replace('\', '/')
$allViolations = @()

foreach ($platform in $Platforms) {
    Write-Host ''
    Write-Host "checking $platform"

    $keys = Get-PlatformManifestKeys -Platform $platform
    $extensionPaths = @()
    $debugExtensionPaths = @()
    foreach ($libraryKey in $keys.Libraries) {
        $libraryPaths = @(
            Get-GdextensionPaths -Lines $manifestLines -Section 'libraries' -Key $libraryKey |
                ForEach-Object { Get-AddonRelativePath -ResPath $_ -Prefix $AddonPrefix }
        )
        $extensionPaths += $libraryPaths

        # The debug variant is the one artifact whose debug information is the
        # point of it: it is this repository's own cargo debug build, and the
        # debug-information rule below is about the vendored runtime rather than
        # about it. Recognised by the same `*.debug.*` key shape
        # scripts/assemble_addon.ps1 uses to pick the cargo profile.
        if ($libraryKey -match '\.debug\.') { $debugExtensionPaths += $libraryPaths }
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

    # 2b. Nothing that only exists to build something, at any depth. Step 2 sees
    # only the top level, and a declared directory deliberately covers its whole
    # subtree, so this is the check for what can hide inside one.
    $allPaths = @(
        Get-ChildItem -LiteralPath $platformDir -Recurse -Force -File |
            ForEach-Object { "bin/$platform/$($_.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))" }
    )
    $allViolations += Test-ForbiddenNames -Paths $allPaths -ForbiddenPatterns $forbiddenPatterns |
        ForEach-Object { "${platform}: $_" }

    # 3. Closure.
    $declaredDirectories = @(
        $declaredPaths | Where-Object {
            Test-Path -LiteralPath (Join-Path $addonRoot ($_ -replace '/', [System.IO.Path]::DirectorySeparatorChar)) -PathType Container
        } | ForEach-Object { "bin/$platform/$([System.IO.Path]::GetFileName($_))" }
    )

    # A platform's shared-object pattern follows its binary format. It is not
    # decided by whether the tree happens to carry a plugins/ directory: Android
    # ships neither plugins/ nor libvlccore.so -- its runtime is one monolithic
    # libvlc.so -- and matching '*.dll' there would enumerate nothing and pass.
    $pattern = if ($platform -eq 'win-x64') { '*.dll' } else { '*.so*' }
    $sharedObjects = @(
        Get-ChildItem -LiteralPath $platformDir -Recurse -Force -File |
            Where-Object { $_.Name -like $pattern }
    )
    $inspectedPaths = @(
        $sharedObjects | ForEach-Object {
            "bin/$platform/$($_.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
        }
    )

    # A closure check that inspected nothing verified nothing. Test-InspectedCoverage
    # notices an empty enumeration through a declared directory, and Android has
    # none: its payload is flat, so this is the guard for that shape.
    if ($sharedObjects.Count -eq 0) {
        $allViolations += "${platform}: no shared object matched '$pattern' under bin/$platform, so the dependency closure was not verified"
    }

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
    } elseif ($platform -eq 'win-x64') {
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
    } else {
        # Android: read DT_NEEDED statically, because the payload cannot be
        # resolved by the host's loader.
        if (-not $elfTool) {
            $allViolations += "${platform}: neither llvm-readelf nor readelf is available, so the DT_NEEDED closure could not be verified"
        } else {
            # Index every shared object the addon ships so a NEEDED name the
            # payload itself satisfies is attributed to the addon.
            $shippedLibraries = @(
                foreach ($library in $sharedObjects) {
                    [pscustomobject]@{
                        Name     = $library.Name
                        Relative = "bin/$platform/$($library.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
                    }
                }
            )

            foreach ($sharedObject in $sharedObjects) {
                $relative = "bin/$platform/$($sharedObject.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
                $raw = @(& $elfTool.Source -d $sharedObject.FullName 2>&1)

                # Distinguishes "this object needs nothing" from "the reader could
                # not read it": the latter parses to zero entries just as
                # silently, and a silent zero is the failure mode this whole
                # script exists to prevent. Matched case-sensitively, because
                # readelf's "There is no dynamic section in this file" carries the
                # same two words.
                if (-not ($raw -cmatch 'Dynamic section')) {
                    $allViolations += "${platform}: $relative has no readable dynamic section (read with '$($elfTool.Name)'), so its DT_NEEDED closure was not verified"
                }

                $entries = Get-ElfNeededEntry -Output $raw
                $entries = Resolve-ElfNeededEntry -Entries $entries -Shipped $shippedLibraries -AddonRoot $addonRootPosix
                Write-Verbose "  $relative`: $($entries.Count) DT_NEEDED entry(ies) via '$($elfTool.Name)'"
                $allViolations += Test-DependencyEntries -Entries $entries `
                    -AddonRoot $addonRootPosix `
                    -DeclaredPaths $declaredPaths `
                    -HostProvidedPatterns $hostPatterns[$platform] `
                    -Source $relative |
                    ForEach-Object { "${platform}: $_" }
            }
        }
    }

    # 4. No debug information in anything that ships.
    #
    # A section nothing ever reads is not a feature, it is download weight, and a
    # strip step that gets skipped is invisible in behaviour: the addon works
    # exactly as it did before, and only what users download changes. Measured on
    # the payload this check was added for, DWARF was 666.0 MB of the 798.7 MB
    # linux-x64 runtime and 827.1 MB of the 1025.7 MB win-x64 one, against 0
    # bytes in the stripped android-arm64 one. The strip happens in
    # build/vlc/postprocess.ps1; asserting the outcome here is what makes any
    # layer that forgets it fail loudly instead of quietly multiplying the
    # download.
    #
    # The one deliberate exception is the extension library the manifest
    # declares as a debug variant ($debugExtensionPaths): that is this
    # repository's own cargo debug build, where the debug information is the
    # point of the artifact, rather than the vendored runtime this rule is about.
    $debugTool = if ($platform -eq 'win-x64') { $peTool } else { $elfTool }
    if (-not $debugTool) {
        $debugTools = if ($platform -eq 'win-x64') { 'llvm-objdump nor objdump' } else { 'llvm-readelf nor readelf' }
        $allViolations += "${platform}: neither $debugTools is available, so the payload could not be checked for debug information"
    } else {
        foreach ($sharedObject in $sharedObjects) {
            $relative = "bin/$platform/$($sharedObject.FullName.Substring($platformDir.Length + 1).Replace('\', '/'))"
            if ($debugExtensionPaths -contains $relative) { continue }

            if ($platform -eq 'win-x64') {
                # -h is the section table; -p (the import table) is a different
                # question that the closure check above already asked.
                $raw = @(& $debugTool.Source -h $sharedObject.FullName 2>&1)
                $hasSectionTable = [bool]($raw -cmatch 'Sections:')
            } else {
                # -W asks for untruncated names. The match below is a prefix
                # match, so wide output is for the message rather than for the
                # detection.
                $raw = @(& $debugTool.Source -S -W $sharedObject.FullName 2>&1)
                # Matched case-sensitively against the table's title line: the
                # summary line above it ("There are N section headers") carries
                # the same two words in lower case, and a file without a section
                # table reports "no sections in this file" the same way.
                $hasSectionTable = [bool]($raw -cmatch 'Section Headers:')
            }

            if (-not $hasSectionTable) {
                $allViolations += "${platform}: $relative has no readable section table (read with '$($debugTool.Name)'), so it was not checked for debug information"
                continue
            }

            if ($platform -eq 'win-x64') {
                $sectionNames = Get-PeSectionName -Output $raw
            } else {
                $sectionNames = Get-ElfSectionName -Output $raw
            }

            if ($sectionNames.Count -eq 0) {
                $allViolations += "${platform}: $relative printed a section table that parsed to no sections (read with '$($debugTool.Name)'), so the debug-information check did not run"
                continue
            }

            # A parser that silently stopped matching rows would drop a trailing
            # `.debug_*` section, which is the same class of silent omission as a
            # skipped strip, so the row count is cross-checked against the count
            # the tool reported for its own table.
            if ($platform -ne 'win-x64') {
                $declaredSections = Get-ElfSectionCount -Output $raw
                if ($declaredSections -ge 0 -and $declaredSections -ne $sectionNames.Count) {
                    $allViolations += "${platform}: $relative printed a table of $declaredSections section header(s) of which $($sectionNames.Count) parsed (read with '$($debugTool.Name)'), so the debug-information check did not run"
                    continue
                }
            }

            $debugSections = Get-DebugSection -Names $sectionNames
            if ($debugSections.Count -gt 0) {
                $allViolations += "${platform}: $relative carries debug information ($($debugSections -join ', ')); the shipped runtime must be stripped"
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
    Write-Host 'means the manifest and the runtime have drifted apart. Debug information means a'
    Write-Host 'strip step was skipped: fix the strip (build/vlc/postprocess.ps1), never this check.'
    exit 1
}

Write-Host 'check_addon: OK' -ForegroundColor Green

#endregion Main
