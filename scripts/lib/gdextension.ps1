<#
Shared reader for gdextension_template/godot_vlc.gdextension.

The assembler (scripts/assemble_addon.ps1) and the checker
(scripts/check_addon.ps1) must agree on two answers: which files are the
extension libraries, and which runtime files have to ship. They read the same
manifest through the same code here; when they disagree, the checker ends up
validating a manifest the assembler never followed.

This is the single source of truth for "what ships". Anything present in the
assembled addon but absent from the manifest is either not exported by Godot or
reported by the checker as a missing declaration.
#>

function Get-IniSection {
    <#
    .SYNOPSIS
    The raw lines of one [section], stopping at the next section header.
    #>
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines,
        [Parameter(Mandatory)][string]$Section
    )

    $inside = $false
    foreach ($line in $Lines) {
        if ($line -match '^\s*\[(?<name>[^\]]+)\]\s*$') {
            $inside = ($Matches['name'] -eq $Section)
            continue
        }
        if ($inside) { $line }
    }
}

function Get-GdextensionPaths {
    <#
    .SYNOPSIS
    The paths declared for one key in [libraries] or [dependencies].

    .DESCRIPTION
    In [libraries] a key maps to a single quoted path. In [dependencies] a key
    maps to a brace-delimited block of `"path": ""` entries, which may be
    written across several lines or all on one.

    Only the keys are extracted. A bare scan for quoted strings would also match
    the empty values, and because the pattern spans newlines it would splice
    fragments of adjacent lines together.
    #>
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines,
        [Parameter(Mandatory)][string]$Section,
        [Parameter(Mandatory)][string]$Key
    )

    $text = (Get-IniSection -Lines $Lines -Section $Section) -join "`n"
    $escapedKey = [regex]::Escape($Key)
    $options = [System.Text.RegularExpressions.RegexOptions]::Multiline -bor
               [System.Text.RegularExpressions.RegexOptions]::Singleline

    if ($Section -eq 'dependencies') {
        $block = [regex]::Match($text, "^\s*$escapedKey\s*=\s*\{(?<body>.*?)\}", $options)
        if (-not $block.Success) { return @() }
        return @(
            [regex]::Matches($block.Groups['body'].Value, '"([^"]+)"\s*:') |
                ForEach-Object { $_.Groups[1].Value }
        )
    }

    $single = [regex]::Match($text, "^\s*$escapedKey\s*=\s*""(?<path>[^""]*)""", $options)
    if (-not $single.Success) { return @() }
    @($single.Groups['path'].Value)
}

function Get-AddonRelativePath {
    <#
    .SYNOPSIS
    Maps a res:// path from the manifest to a path relative to the addon root.
    #>
    param(
        [Parameter(Mandatory)][string]$ResPath,
        [Parameter(Mandatory)][string]$Prefix
    )

    if (-not $ResPath.StartsWith($Prefix, [System.StringComparison]::Ordinal)) {
        throw "manifest path '$ResPath' does not start with '$Prefix'"
    }
    $ResPath.Substring($Prefix.Length).Replace('\', '/').Trim('/')
}

function Get-PlatformManifestKeys {
    <#
    .SYNOPSIS
    The [libraries] / [dependencies] keys belonging to one platform.

    .DESCRIPTION
    The Android keys name the architecture the way GDExtension does -- arm64, not
    arm64-v8a -- because the export plugin matches [libraries] against the arch
    names the export platform is given. A key written with the ABI name never
    matches, and the library is then absent from the APK without the export
    failing: Godot writes an entry for a declared library it cannot find, and that
    entry is empty.
    #>
    param([Parameter(Mandatory)][ValidateSet('win-x64', 'linux-x64', 'android-arm64')][string]$Platform)

    if ($Platform -eq 'win-x64') {
        @{ Libraries = @('windows.debug.x86_64', 'windows.release.x86_64'); Dependencies = 'windows.x86_64' }
    } elseif ($Platform -eq 'linux-x64') {
        @{ Libraries = @('linux.debug.x86_64', 'linux.release.x86_64'); Dependencies = 'linux.x86_64' }
    } else {
        @{ Libraries = @('android.debug.arm64', 'android.release.arm64'); Dependencies = 'android.arm64' }
    }
}
