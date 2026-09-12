<#
.SYNOPSIS
Prepares this repository for development on the machine it runs on.

.DESCRIPTION
One entry point for a contributor: work out whether this machine can build the
extension, obtain the LibVLC runtime it links against, and then chain the existing
scripts in the order they need to run.

The boundaries, which are choices rather than accidents:

  * It configures the HOST platform only. A runtime for another platform cannot be
    built on for tests here, so preparing one would produce a tree nothing runs.

  * It DETECTS prerequisites and never installs them. The toolchain versions this
    project depends on are pinned elsewhere in the repository, and a setup script
    that quietly installs a different one would recreate the class of problem the
    pins exist to remove. Missing required tools are reported with the command
    that installs them.

  * It fetches the LibVLC runtime as the artifact CI produced, and will not start
    a container build on its own. That build takes one to two hours; beginning it
    because a download failed would be indistinguishable from a hang. `libvlc`
    starts it, explicitly.

  * The scripts it chains are the ones CI runs, in the same order, with the same
    arguments. What may be laxer here is the prerequisite report above, because a
    missing Godot does not affect building or testing anything. Nothing below that
    layer is laxer: the gates fail on the same violations CI fails on.

Prerequisites it deliberately does not install:
    Rust (see rust-toolchain.toml), libclang (bindgen), an MSVC toolchain on
    Windows, tar, and objdump or llvm-objdump for the gates. Docker and the GitHub
    CLI are optional: they are two ways to obtain the runtime, and at least one is
    needed.

.PARAMETER Action
    check    report this machine's prerequisites and stop
    stage    obtain the runtime into thirdparty/ (default, with check)
    libvlc   build the runtime in a container (explicit; one to two hours)
    build    compile the extension, debug and release
    test     run the unit tests
    addon    assemble the addon and run the gates over it
    accept   decode a real H.264 file through the assembled addon
    reset    delete the generated directories

.PARAMETER SkipFetch
Do not use the network: work only from artifacts already in artifacts/.

.PARAMETER All
With reset, also delete target/ (which costs a full recompile).

.PARAMETER LinuxTarget
With check, also lint for the Linux target when it is installed. Windows never
compiles the #[cfg(target_os = "linux")] code, so that is the only way to see its
lints before CI does.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('check', 'stage', 'libvlc', 'build', 'test', 'addon', 'accept', 'reset')]
    [string]$Action,

    [switch]$SkipFetch,
    [switch]$All,
    [switch]$LinuxTarget
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $repoRoot

. "$PSScriptRoot/lib/vlc_runtime.ps1"
. "$PSScriptRoot/../build/vlc/lib/native.ps1"

$platform = Get-HostVlcPlatform
$containerImage = if ($platform -eq 'win-x64') { 'godot-vlc/vlc-build-win' } else { 'godot-vlc/vlc-build' }
$artifactPath = Join-Path $repoRoot "artifacts/vlc-$platform.tar.gz"
$generatedDirs = @('thirdparty/vlc', 'artifacts', 'gdextension_template/bin')

function Write-Step {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "setup: $Message"
}

function Invoke-RepoScript {
    # The existing scripts are the pipeline. This runs them; it does not reimplement
    # them, so what happens here is what happens in CI.
    param(
        [Parameter(Mandatory)][string]$Script,
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$ScriptArguments
    )

    Write-Step "running scripts/$Script"
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot $Script) @ScriptArguments
    if ($LASTEXITCODE -ne 0) {
        throw "scripts/$Script exited with $LASTEXITCODE"
    }
}

# ---------------------------------------------------------------------------
# check
# ---------------------------------------------------------------------------

function Get-CommandVersion {
    param(
        [Parameter(Mandatory)][string]$Command,
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$VersionArguments
    )

    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) { return $null }
    $output = & $Command @VersionArguments 2>&1 | Select-Object -First 1
    if ($LASTEXITCODE -ne 0 -and -not $output) { return $null }
    "$output".Trim()
}

function Get-PinnedRustVersion {
    $toolchain = Join-Path $repoRoot 'rust-toolchain.toml'
    if (-not (Test-Path -LiteralPath $toolchain)) { return $null }
    foreach ($line in Get-Content -LiteralPath $toolchain) {
        if ($line -match '^\s*channel\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    $null
}

function Get-DemoGodotVersion {
    # Godot rewrites this field whenever it opens the project, so it tracks whoever
    # last opened it; the demo is meant to follow the newest Godot while the
    # declared minimum stays in Cargo.toml and the .gdextension.
    $project = Join-Path $repoRoot 'demo/project.godot'
    if (-not (Test-Path -LiteralPath $project)) { return $null }
    foreach ($line in Get-Content -LiteralPath $project) {
        if ($line -match 'config/features\s*=\s*PackedStringArray\(\s*"([^"]+)"') { return $Matches[1] }
    }
    $null
}

function Invoke-Check {
    $required = [System.Collections.Generic.List[object]]::new()
    $optional = [System.Collections.Generic.List[object]]::new()

    function Add-Row {
        param([string]$Name, [string]$Version, [bool]$IsRequired, [string]$Advice)
        $row = [pscustomobject]@{ Name = $Name; Version = $Version; Advice = $Advice }
        if ($IsRequired) { $required.Add($row) } else { $optional.Add($row) }
    }

    $cargo = Get-CommandVersion -Command 'cargo' -VersionArguments @('--version')
    $pinned = Get-PinnedRustVersion
    # --component takes one value. Repeated, rather than space-separated: rustup
    # reads the second word as a [TOOLCHAIN] argument and fails with "invalid
    # toolchain name: 'clippy'".
    $rustAdvice = if ($pinned) { "install the pinned toolchain: rustup toolchain install $pinned --component rustfmt --component clippy" } else { 'install Rust from https://rustup.rs' }
    Add-Row -Name 'cargo' -Version $cargo -IsRequired $true -Advice $rustAdvice
    if ($cargo -and $pinned) {
        $active = Get-CommandVersion -Command 'rustc' -VersionArguments @('--version')
        if ($active -and $active -notlike "*$pinned*") {
            $optional.Add([pscustomobject]@{
                    Name    = 'rust toolchain'
                    Version = "$active (rust-toolchain.toml pins $pinned)"
                    Advice  = "rustup toolchain install $pinned --component rustfmt --component clippy"
                })
        }
    }

    Add-Row -Name 'libclang (LIBCLANG_PATH)' -Version $env:LIBCLANG_PATH -IsRequired $true `
        -Advice 'install LLVM and set LIBCLANG_PATH to the directory holding libclang; bindgen needs it'

    if ($platform -eq 'win-x64') {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
        $vs = if (Test-Path -LiteralPath $vswhere) { (& $vswhere -latest -property installationVersion 2>$null) } else { $null }
        Add-Row -Name 'MSVC' -Version $vs -IsRequired $true -Advice 'install Visual Studio with the C++ workload'
    }

    Add-Row -Name 'tar' -Version (Get-CommandVersion -Command 'tar' -VersionArguments @('--version')) `
        -IsRequired $true -Advice 'tar is needed to unpack the runtime artifact'
    Add-Row -Name 'objdump' -Version ((Get-CommandVersion -Command 'llvm-objdump' -VersionArguments @('--version')) ??
        (Get-CommandVersion -Command 'objdump' -VersionArguments @('--version'))) `
        -IsRequired $true -Advice 'install LLVM (llvm-objdump) or binutils (objdump); the dependency gate reads imported libraries with it'

    Add-Row -Name 'docker' -Version (Get-CommandVersion -Command 'docker' -VersionArguments @('--version')) `
        -IsRequired $false -Advice 'only needed for setup.ps1 libvlc, which builds the runtime in a container'
    Add-Row -Name 'gh' -Version (Get-CommandVersion -Command 'gh' -VersionArguments @('--version')) `
        -IsRequired $false -Advice 'only needed to download the runtime artifact from CI'
    if (Get-Command 'gh' -ErrorAction SilentlyContinue) {
        & gh auth status *> $null
        if ($LASTEXITCODE -ne 0) {
            $optional.Add([pscustomobject]@{ Name = 'gh auth'; Version = 'not logged in'; Advice = 'run gh auth login to download the runtime artifact' })
        }
    }

    $godot = Get-CommandVersion -Command 'godot' -VersionArguments @('--version')
    Add-Row -Name 'Godot' -Version $godot -IsRequired $false `
        -Advice 'only needed to open demo/; building and the acceptance test do not use the engine'

    Write-Host ''
    Write-Host "setup: this machine ($platform)"
    foreach ($row in ($required + $optional)) {
        $mark = if ($row.Version) { 'ok  ' } elseif ($required.Contains($row)) { 'MISS' } else { '--  ' }
        $shown = if ($row.Version) { $row.Version } else { 'not found' }
        Write-Host ("  {0} {1,-24} {2}" -f $mark, $row.Name, $shown)
    }

    $demoGodot = Get-DemoGodotVersion
    if ($godot -and $demoGodot -and $godot -notlike "$demoGodot*") {
        Write-Host ''
        Write-Host "  note: demo/project.godot records Godot $demoGodot and this machine has $godot."
        Write-Host '        The demo is meant to follow the newest Godot; opening it will update that field.'
    }

    if ($LinuxTarget) {
        $targets = & rustup target list --installed 2>$null
        if ($targets -contains 'x86_64-unknown-linux-gnu') {
            Write-Host ''
            Write-Step 'linting for x86_64-unknown-linux-gnu (Windows never compiles that code)'
            & cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
            if ($LASTEXITCODE -ne 0) { throw 'clippy failed for the Linux target' }
        } else {
            Write-Host ''
            Write-Host '  note: rustup target add x86_64-unknown-linux-gnu would let this check the Linux-only code locally.'
        }
    }

    $missing = @($required | Where-Object { -not $_.Version })
    if ($missing.Count -gt 0) {
        Write-Host ''
        Write-Host "setup: cannot proceed; $($missing.Count) required tool(s) are missing:" -ForegroundColor Red
        foreach ($row in $missing) { Write-Host "  - $($row.Name): $($row.Advice)" }
        exit 1
    }
}

# ---------------------------------------------------------------------------
# stage: get the runtime, from what is here or from CI
# ---------------------------------------------------------------------------

function Get-ArtifactRunId {
    <#
    The run whose artifact matches this checkout, newest first: this commit if CI has
    built it, otherwise the branch's latest successful run. The provenance check in
    Invoke-Stage decides whether what it produced is actually usable, so trying the
    branch is safe: a run from another commit still carries the same pinned VLC when
    only the extension changed.
    #>
    $head = (& git rev-parse HEAD).Trim()
    $run = & gh run list --workflow build.yml --status success --limit 20 `
        --json databaseId,headSha,number |
        ConvertFrom-Json |
        Where-Object { $_.headSha -eq $head } |
        Select-Object -First 1
    if ($run) { return $run }

    & gh run list --workflow build.yml --status success --limit 1 --json databaseId,headSha,number |
        ConvertFrom-Json |
        Select-Object -First 1
}

function Invoke-FetchArtifact {
    if ($SkipFetch) {
        Write-Step 'not fetching: -SkipFetch was given'
        return $false
    }
    if (-not (Get-Command 'gh' -ErrorAction SilentlyContinue)) {
        Write-Step 'cannot fetch: the GitHub CLI (gh) is not installed'
        return $false
    }
    & gh auth status *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Step 'cannot fetch: gh is not logged in (gh auth login)'
        return $false
    }

    $run = Get-ArtifactRunId
    if (-not $run) {
        Write-Step 'cannot fetch: no successful build.yml run found'
        return $false
    }

    Write-Step "downloading vlc-$platform from run #$($run.number) ($($run.headSha.Substring(0, 7)))"
    New-Item -Path (Join-Path $repoRoot 'artifacts') -ItemType Directory -Force | Out-Null
    & gh run download $run.databaseId --name "vlc-$platform" --dir (Join-Path $repoRoot 'artifacts')
    if ($LASTEXITCODE -ne 0) {
        Write-Step "cannot fetch: gh run download exited with $LASTEXITCODE"
        return $false
    }
    Write-Step "this runtime was built by CI, not on this machine"
    return $true
}

function Invoke-Stage {
    if (-not (Test-Path -LiteralPath $artifactPath)) {
        Write-Step "no runtime artifact at artifacts/vlc-$platform.tar.gz"
        if (-not (Invoke-FetchArtifact)) {
            Write-Host ''
            Write-Host "setup: cannot obtain the LibVLC runtime for $platform." -ForegroundColor Red
            Write-Host '       Build it here (two container images, one to two hours):'
            Write-Host '           pwsh scripts/setup.ps1 libvlc'
            Write-Host '       Or put artifacts/vlc-<platform>.tar.gz in place yourself and re-run this.'
            exit 1
        }
    }

    # The artifact has to match the pin the repository holds; a stale one is the
    # failure this whole pipeline is arranged around.
    Invoke-RepoScript -Script 'check_vlc_provenance.ps1' -ScriptArguments @()
    Invoke-RepoScript -Script 'stage_libvlc.ps1' -ScriptArguments @('-Platforms', $platform)
}

# ---------------------------------------------------------------------------
# libvlc: the container build, only ever on request
# ---------------------------------------------------------------------------

function Invoke-LibVlc {
    if (-not (Get-Command 'docker' -ErrorAction SilentlyContinue)) {
        throw 'docker is required to build LibVLC here; install Docker, or obtain artifacts/vlc-<platform>.tar.gz another way'
    }

    Write-Host ''
    Write-Step 'building LibVLC in a container: this takes one to two hours from cold'
    Write-Step "the container cache under build/vlc is reused, so a rebuild after a small change is much faster"

    $base = if ($platform -eq 'win-x64') { 'ubuntu:24.04' } else { 'ubuntu:22.04' }
    Invoke-Native -Command 'docker' -WorkingDirectory $repoRoot -Arguments @(
        'build', '--build-arg', "BASE_IMAGE=$base", '-t', $containerImage, 'build/vlc'
    )

    New-Item -Path (Join-Path $repoRoot 'artifacts') -ItemType Directory -Force | Out-Null
    Invoke-Native -Command 'docker' -WorkingDirectory $repoRoot -Arguments @(
        'run', '--rm', '-v', "$(Join-Path $repoRoot 'artifacts'):/out",
        '-v', 'godot-vlc-work:/tmp/vlc-build', $containerImage, $platform, '/out'
    )

    Write-Step 'container build finished; staging what it produced'
    Invoke-Stage
}

# ---------------------------------------------------------------------------
# reset
# ---------------------------------------------------------------------------

function Invoke-Reset {
    $targets = if ($All) { $generatedDirs + 'target' } else { $generatedDirs }
    foreach ($relative in $targets) {
        $path = Join-Path $repoRoot $relative
        if (Test-Path -LiteralPath $path) {
            Write-Step "removing $relative"
            Remove-Item -Recurse -Force -LiteralPath $path
        }
    }
    if (-not $All) {
        Write-Host '  target/ was kept; add -All to remove it as well (it costs a full recompile)'
    }
}

# ---------------------------------------------------------------------------

switch ($Action) {
    'check' { Invoke-Check }
    'stage' { Invoke-Stage }
    'libvlc' { Invoke-LibVlc }
    'build' {
        Invoke-RepoScript -Script 'build_debug.ps1' -ScriptArguments @()
        Invoke-RepoScript -Script 'build_release.ps1' -ScriptArguments @()
    }
    'test' { Invoke-RepoScript -Script 'test.ps1' -ScriptArguments @() }
    'addon' {
        Invoke-RepoScript -Script 'assemble_addon.ps1' -ScriptArguments @('-Platforms', $platform)
        Invoke-RepoScript -Script 'check_addon.ps1' -ScriptArguments @('-Platforms', $platform)
        if ($platform -eq 'linux-x64') {
            Invoke-RepoScript -Script 'check_glibc_floor.ps1' -ScriptArguments @(
                '-Path', 'target/release/libgodot_vlc.so,gdextension_template/bin/linux-x64')
        }
    }
    'accept' { Invoke-RepoScript -Script 'acceptance_test.ps1' -ScriptArguments @('-Platform', $platform) }
    'reset' { Invoke-Reset }
    default {
        Invoke-Check
        Invoke-Stage
        Write-Host ''
        Write-Step 'ready. Next: pwsh scripts/setup.ps1 build'
    }
}
