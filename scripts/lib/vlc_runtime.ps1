<#
Shared runtime-location knowledge for the scripts that execute the
self-compiled LibVLC (scripts/test.ps1, scripts/acceptance_test.ps1).

Two rules live here because both callers need them and because getting either
one wrong produces a failure that looks like something else entirely:

  * where the loader looks for the runtime: PATH on Windows, LD_LIBRARY_PATH on
    Linux. Without it the test binary dies with STATUS_DLL_NOT_FOUND or
    "cannot open shared object file", which reads as a broken test suite rather
    than a missing search path.

  * that LibVLC's plugin directory is <lib>/vlc on Linux, because LibVLC appends
    /plugins to VLC_LIB_PATH. That layout rule is also implemented in
    src/vlc_runtime.rs; the two must agree.
#>

function Get-HostVlcPlatform {
    <#
    .SYNOPSIS
    The staged platform matching the machine running this script.
    #>
    # $IsWindows is an automatic variable, so the local is named differently.
    $onWindows = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [System.Runtime.InteropServices.OSPlatform]::Windows
    )
    if ($onWindows) { 'win-x64' } else { 'linux-x64' }
}

function Add-VlcRuntimeToSearchPath {
    <#
    .SYNOPSIS
    Makes the staged LibVLC resolvable, and pins the directories it needs.

    .DESCRIPTION
    On Linux this sets all three of LibVLC 4's path overrides rather than letting
    it derive them from its own location, because deriving them produces paths
    that may not exist, silently:

      VLC_LIB_PATH      the plugin directory's parent
      VLC_LIBEXEC_PATH  the out-of-process preparser and the cache generator
      VLC_DATA_PATH     the Lua scripts

    Missing the last two is not obvious. Every plugin still loads, and the failure
    only appears later as "Fail to create Process in process_pool" followed by
    "cannot start any interface". These are the same variables the extension sets
    in src/vlc_runtime.rs, so exercising them here tests the mechanism the
    extension relies on.
    #>
    param(
        [Parameter(Mandatory)][string]$LibDir,
        [Parameter(Mandatory)][ValidateSet('win-x64', 'linux-x64')][string]$Platform
    )

    if ($Platform -eq 'win-x64') {
        # In the assembled addon this alone is enough: VLC resolves
        # <directory of libvlccore.dll>\plugins, and the addon keeps plugins/,
        # libexec/ and share/ as siblings of the DLLs. That is the layout the
        # embedded case relies on and what check_addon.ps1 asserts.
        #
        # The CLI is a different case. It lives in thirdparty/<platform>/tools/ with
        # its own copy of libvlccore.dll, so VLC looks for tools/plugins and finds
        # nothing: `vlc --list` reports the core module and no others, which looks
        # like an empty runtime rather than a path problem. VLC_PLUGIN_PATH is
        # additive, so pointing it at the shipped plugins makes the CLI use the same
        # modules the addon ships.
        $env:PATH = "$LibDir;$env:PATH"
        $pluginDir = Join-Path $LibDir 'plugins'
        if (Test-Path -LiteralPath $pluginDir) { $env:VLC_PLUGIN_PATH = $pluginDir }
    } else {
        # The assembled addon keeps vlc/ beside the libraries, matching what the
        # extension sets at runtime; VLC's own install layout keeps it under lib/.
        # Both are accepted so a staged runtime can be checked before it has been
        # assembled, which is the only way to test it when the extension binary
        # for that platform cannot be built locally.
        $libraryRoot = if (Test-Path -LiteralPath (Join-Path $LibDir 'vlc')) { $LibDir } else { Join-Path $LibDir 'lib' }

        $env:LD_LIBRARY_PATH = if ($env:LD_LIBRARY_PATH) { "$libraryRoot`:$env:LD_LIBRARY_PATH" } else { $libraryRoot }
        $env:VLC_LIB_PATH = Join-Path $libraryRoot 'vlc'
        $env:VLC_LIBEXEC_PATH = Join-Path $LibDir 'libexec/vlc'
        $env:VLC_DATA_PATH = Join-Path $LibDir 'share/vlc'
    }
}
