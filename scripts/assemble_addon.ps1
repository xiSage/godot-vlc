<#
.SYNOPSIS
Assembles the Godot addon payload under gdextension_template/bin/.

.DESCRIPTION
Copies the built GDExtension binaries and the self-compiled LibVLC runtime into
the layout that gdextension_template/godot_vlc.gdextension declares.

The manifest drives this completely: the extension libraries come from the
[libraries] keys, and every runtime file comes from the [dependencies] entry for
the platform. Nothing else is copied. That makes "what ships" a single
reviewable list, and it lets scripts/check_addon.ps1 assert that the payload
matches the manifest instead of discovering that it does not.

Files are copied, not symlinked. Symlinks pointed back into thirdparty/, so the
assembled tree was not the thing users receive: an ldd of it resolved outside the
addon, the closure check could not tell whether a dependency shipped, and zipping
the addon depended on how the archiver treated links. Copying costs a few seconds
and makes the payload exactly what is delivered.

Every source is validated before anything is deleted, so a missing input cannot
leave the payload half-built.

This script downloads nothing. Run, in order:
    1. the VLC build container (build/vlc/)   -> an artifact tarball
    2. scripts/stage_libvlc.ps1               -> thirdparty/vlc/<platform>/
    3. scripts/build_release.ps1 / build_debug.ps1 -> target/{release,debug}/
then this script, then scripts/check_addon.ps1.

.PARAMETER Platforms
Platforms to assemble. Defaults to both. A requested platform whose build output
or runtime is absent is a hard error rather than a silent skip.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64', 'android-arm64')]
    [string[]]$Platforms = @('win-x64', 'linux-x64'),

    [string]$GdextensionPath = 'gdextension_template/godot_vlc.gdextension',
    [string]$AddonPrefix = 'res://addons/godot-vlc/'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot/lib/gdextension.ps1"

$repoRoot = Split-Path -Parent $PSScriptRoot
# Destinations are addon-root relative (they mirror the res:// paths in the
# manifest), so the addon root is the join base, not the bin directory.
$addonRoot = Join-Path $repoRoot (Split-Path -Parent $GdextensionPath.Replace('\', '/'))
$binDir = Join-Path $addonRoot 'bin'

# Cargo's output file name per platform. The manifest records where a library
# must end up, which is not the name the linker produces: the debug extension is
# shipped as godot_vlc_debug.dll but built as godot_vlc.dll.
$linkerOutput = @{
    'win-x64'       = 'godot_vlc.dll'
    'linux-x64'     = 'libgodot_vlc.so'
    'android-arm64' = 'libgodot_vlc.so'
}

# Where cargo leaves that output. A host build lands in target/<profile>; a cross
# build lands under the target triple, so the Android extension is not beside the
# desktop ones.
$linkerTriple = @{
    'android-arm64' = 'aarch64-linux-android'
}

$gdextensionAbs = Join-Path $repoRoot $GdextensionPath
if (-not (Test-Path -LiteralPath $gdextensionAbs)) { throw "'$GdextensionPath' not found." }
$manifestLines = Get-Content -LiteralPath $gdextensionAbs

<#
Builds the full (destination, source) plan first. Any missing input aborts
before the existing payload is removed.
#>
function New-AddonPlan {
    param([Parameter(Mandatory)][ValidateSet('win-x64', 'linux-x64', 'android-arm64')][string]$Platform)

    $keys = Get-PlatformManifestKeys -Platform $Platform
    $targetDir = if ($linkerTriple.ContainsKey($Platform)) { "target/$($linkerTriple[$Platform])" } else { 'target' }
    $plan = [System.Collections.Generic.List[object]]::new()

    foreach ($libraryKey in $keys.Libraries) {
        $profile = if ($libraryKey -match '\.debug\.') { 'debug' } else { 'release' }
        foreach ($resPath in Get-GdextensionPaths -Lines $manifestLines -Section 'libraries' -Key $libraryKey) {
            $plan.Add([pscustomobject]@{
                    Dest   = Get-AddonRelativePath -ResPath $resPath -Prefix $AddonPrefix
                    Source = "$targetDir/$profile/$($linkerOutput[$Platform])"
                })
        }
    }

    $declared = @(Get-GdextensionPaths -Lines $manifestLines -Section 'dependencies' -Key $keys.Dependencies)
    if ($declared.Count -eq 0) {
        throw "the manifest has no [$($keys.Dependencies)] entry in [dependencies]; nothing would be packaged."
    }
    foreach ($resPath in $declared) {
        $relative = Get-AddonRelativePath -ResPath $resPath -Prefix $AddonPrefix
        $name = [System.IO.Path]::GetFileName($relative)

        # Most runtime entries live under lib/, but libexec/ and share/ sit beside
        # it in VLC's install layout, so the platform root is the fallback. The
        # source layout is otherwise a straight mirror of the destination.
        $source = "thirdparty/vlc/$Platform/lib/$name"
        if (-not (Test-Path -LiteralPath (Join-Path $repoRoot $source))) {
            $source = "thirdparty/vlc/$Platform/$name"
        }

        $plan.Add([pscustomobject]@{ Dest = $relative; Source = $source })
    }

    $plan
}

function Test-AddonPlan {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Plan)

    foreach ($entry in $Plan) {
        if (-not (Test-Path -LiteralPath (Join-Path $repoRoot $entry.Source))) {
            throw "'$($entry.Source)' is missing but the manifest declares '$($entry.Dest)'. Stage the runtime and build the extension first; refusing to touch the existing payload."
        }
    }
}

function Invoke-AddonPlan {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Plan)

    if (Test-Path -LiteralPath $binDir) {
        Remove-Item -Recurse -Force -LiteralPath $binDir
    }

    foreach ($entry in $Plan) {
        $sourceAbs = Join-Path $repoRoot $entry.Source
        $destAbs = Join-Path $addonRoot $entry.Dest
        $destParent = Split-Path -Parent $destAbs
        if (-not (Test-Path -LiteralPath $destParent)) {
            New-Item -Path $destParent -ItemType Directory -Force | Out-Null
        }

        # Copy-Item dereferences symlinks, so a SONAME entry such as
        # libvlccore.so.9 becomes a real file in the addon rather than a link
        # back into thirdparty/.
        Copy-Item -LiteralPath $sourceAbs -Destination $destAbs -Recurse -Force
    }
}

# ---------------------------------------------------------------------------
# Everything is planned and validated first.
# ---------------------------------------------------------------------------
$plan = [System.Collections.Generic.List[object]]::new()
foreach ($platform in $Platforms) {
    $plan.AddRange([object[]](New-AddonPlan -Platform $platform))
}
Test-AddonPlan -Plan $plan

Invoke-AddonPlan -Plan $plan

# The demo project consumes the addon through this link. It stays a link: it
# points at a directory inside the repository, not at anything shipped.
$demoLink = Join-Path $repoRoot 'demo/addons/godot-vlc'
$demoParent = Split-Path -Parent $demoLink
if (-not (Test-Path -LiteralPath $demoParent)) {
    New-Item -Path $demoParent -ItemType Directory -Force | Out-Null
}
New-Item -Path $demoLink -ItemType SymbolicLink -Force `
    -Value ([System.IO.Path]::GetRelativePath($demoParent, (Join-Path $repoRoot 'gdextension_template')).Replace('\', '/')) |
    Out-Null

Write-Host "Assembled addon payload for $($Platforms -join ', '): $($plan.Count) entries"
foreach ($entry in $plan) { Write-Host "  $($entry.Dest)" }
