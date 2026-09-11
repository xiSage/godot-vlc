<#
The rule for a relocatable RUNPATH, shared by check-runpath.ps1 (the gate) and
postprocess.ps1 (the patcher).

LibVLC is built with a compile-time install prefix, and libtool bakes that
absolute path into the libraries it produces. Once the tree moves into a Godot
addon those paths are wrong. Replacing them by hand is what made the previous
runtime unreproducible, so the rule lives in one place and both scripts use it.

A component is accepted only if it starts with $ORIGIN. A bare relative
component such as "../lib" is rejected as well: it resolves against the process
working directory rather than against the library, so it guarantees nothing.
#>

function Get-RelativePathTag {
    <#
    .SYNOPSIS
    Extracts the RPATH/RUNPATH entries from `readelf -W -d` output.

    .DESCRIPTION
    Both tags are read. Patching sets RUNPATH, but a file that arrived with a
    DT_RPATH would otherwise go unchecked, and an absolute RPATH is exactly the
    defect this is looking for.

    The match avoids the human-readable description after the tag, because
    binutils translates it; only the tag in parentheses and the bracketed value
    are relied on.
    #>
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    foreach ($line in $Output) {
        if ($line -match '\((?<tag>RUNPATH|RPATH)\)[^:]*:\s*\[(?<value>.*)\]\s*$') {
            [pscustomobject]@{ Tag = $Matches['tag']; Value = $Matches['value'] }
        }
    }
}

function Get-NonRelocatableComponent {
    <#
    .SYNOPSIS
    Every RPATH/RUNPATH component that is not rooted at $ORIGIN.
    #>
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Values)

    $violations = @()
    foreach ($value in $Values) {
        if ([string]::IsNullOrEmpty($value)) { continue }
        foreach ($component in $value.Split(':')) {
            if ($component -notlike '$ORIGIN*') { $violations += $component }
        }
    }
    @($violations)
}

function Get-RunpathValue {
    <#
    .SYNOPSIS
    The RUNPATH value of `readelf -W -d` output, or $null.
    #>
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Output)

    $entry = Get-RelativePathTag -Output $Output | Where-Object Tag -eq 'RUNPATH' | Select-Object -First 1
    if ($entry) { $entry.Value } else { $null }
}

function Get-RunpathDepth {
    <#
    .SYNOPSIS
    How many directories a relative path descends.

    .DESCRIPTION
    Takes the path of a library's directory relative to the runtime root, so
    "lib/vlc/plugins/access/rtp" is 4 and the root itself is 0.
    #>
    param([Parameter(Mandatory)][AllowEmptyString()][string]$RelativeDirectory)

    @($RelativeDirectory -split '[\\/]' | Where-Object { $_ }).Count
}

function Get-RelocatableRunpath {
    <#
    .SYNOPSIS
    The $ORIGIN ladder for a library that sits $Depth directories below the root.

    .DESCRIPTION
    A shipped library finds its siblings through $ORIGIN and the runtime through
    the directories above it, and how many steps that is depends on where the
    library sits: libvlccore.so.9 one level below the runtime root, the helper
    libraries beside the plugins two, a plugin under plugins/codec three, one
    under plugins/access/rtp four.

    A fixed two- or three-step path therefore works for the libraries that happen
    to sit at that depth and silently fails for the rest, which is what happened:
    libvlc_xcb_events.so and every plugin under access/rtp could not find
    libvlccore.so.9, and LibVLC reports that as a plugin it could not load rather
    than as a path that is wrong.

    Listing every step up to the root costs nothing at load time, because a
    directory that does not exist is skipped, and it takes the arithmetic out of
    the picture.
    #>
    param([Parameter(Mandatory)][int]$Depth)

    $parts = @('$ORIGIN')
    for ($level = 1; $level -le $Depth; $level++) {
        $parts += '$ORIGIN' + ('/..' * $level)
    }
    $parts -join ':'
}
