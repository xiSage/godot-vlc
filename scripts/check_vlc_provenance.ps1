<#
.SYNOPSIS
Asserts that every LibVLC artifact was built from the same pinned revision and
reports the same plugin ABI.

.DESCRIPTION
The two platforms used to be built from two different VLC snapshots, which made
any cross-platform behaviour difference impossible to attribute. The plugin ABI
check inside LibVLC is an exact string compare with no compatibility range, so a
runtime whose plugins and core disagree simply refuses to load those plugins.

The comparison lives here rather than inside stage_libvlc.ps1 because staging
happens one platform per CI job, so a check that needs both platforms staged
never ran at all. This script reads the artifacts directly, so it works wherever
both artifacts are present.

.PARAMETER ArtifactsDir
Directory holding vlc-<platform>.tar.gz, relative to the repository root.

.PARAMETER SelfTest
Exercises the comparison against synthetic provenance and exits.
#>
[CmdletBinding()]
param(
    [ValidateSet('win-x64', 'linux-x64')]
    [string[]]$Platforms = @('win-x64', 'linux-x64'),

    [string]$ArtifactsDir = 'artifacts',

    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

function ConvertFrom-BuildInfo {
    <#
    .SYNOPSIS
    Parses the key=value provenance file written by build/vlc/build.ps1.
    #>
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines)

    $info = @{}
    foreach ($line in $Lines) {
        if ($line -match '^([^=#]+)=(.*)$') { $info[$Matches[1].Trim()] = $Matches[2].Trim() }
    }
    $info
}

function Test-Provenance {
    <#
    .SYNOPSIS
    Returns a violation for every field on which the platforms disagree.
    #>
    param(
        [Parameter(Mandatory)][hashtable]$Provenance,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Fields
    )

    $violations = @()
    if ($Provenance.Count -lt 2) { return @() }

    foreach ($field in $Fields) {
        $values = @($Provenance.Values | ForEach-Object { $_[$field] } | Sort-Object -Unique)
        if ($values.Count -gt 1) {
            $detail = ($Provenance.Keys | Sort-Object | ForEach-Object { "$_=$($Provenance[$_][$field])" }) -join ', '
            $violations += "artifacts disagree on ${field}: $detail"
        } elseif ([string]::IsNullOrEmpty($values[0])) {
            $violations += "artifacts do not report ${field} at all"
        }
    }
    @($violations)
}

if ($SelfTest) {
    $script:failures = 0
    function Assert-Equal {
        param([string]$Description, $Expected, $Actual)
        if ("$Expected" -eq "$Actual") { Write-Host "ok   - $Description" }
        else { Write-Host "FAIL - $Description (expected '$Expected', got '$Actual')"; $script:failures++ }
    }

    $parsed = ConvertFrom-BuildInfo -Lines @(
        'platform=linux-x64'
        '# a comment'
        'vlc_commit=546e18e53e000000000000000000000000000000'
        'vlc_api_version_string=4.0.6'
    )
    Assert-Equal 'parses provenance' '546e18e53e000000000000000000000000000000' $parsed['vlc_commit']
    Assert-Equal 'ignores comments' 3 $parsed.Count

    $agree = @{
        'win-x64'   = @{ vlc_commit = 'abc'; vlc_api_version_string = '4.0.6' }
        'linux-x64' = @{ vlc_commit = 'abc'; vlc_api_version_string = '4.0.6' }
    }
    Assert-Equal 'accepts agreeing artifacts' 0 `
        (Test-Provenance -Provenance $agree -Fields @('vlc_commit', 'vlc_api_version_string')).Count

    $disagree = @{
        'win-x64'   = @{ vlc_commit = 'abc'; vlc_api_version_string = '4.0.6' }
        'linux-x64' = @{ vlc_commit = 'def'; vlc_api_version_string = '4.0.6' }
    }
    Assert-Equal 'rejects a commit mismatch' 1 `
        (Test-Provenance -Provenance $disagree -Fields @('vlc_commit', 'vlc_api_version_string')).Count

    $abiDisagree = @{
        'win-x64'   = @{ vlc_commit = 'abc'; vlc_api_version_string = '4.0.5' }
        'linux-x64' = @{ vlc_commit = 'abc'; vlc_api_version_string = '4.0.6' }
    }
    Assert-Equal 'rejects an ABI mismatch' 1 `
        (Test-Provenance -Provenance $abiDisagree -Fields @('vlc_commit', 'vlc_api_version_string')).Count

    $missingField = @{
        'win-x64'   = @{ vlc_commit = 'abc' }
        'linux-x64' = @{ vlc_commit = 'abc' }
    }
    Assert-Equal 'rejects absent provenance fields' 1 `
        (Test-Provenance -Provenance $missingField -Fields @('vlc_commit', 'vlc_api_version_string')).Count

    Assert-Equal 'a single artifact has nothing to compare' 0 `
        (Test-Provenance -Provenance @{ 'win-x64' = @{ vlc_commit = 'abc' } } -Fields @('vlc_commit')).Count

    if ($script:failures -ne 0) {
        Write-Host "self-test: $($script:failures) case(s) failed" -ForegroundColor Red
        exit 1
    }
    Write-Host 'self-test: all cases passed'
    exit 0
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$artifactsAbs = Join-Path $repoRoot $ArtifactsDir

$provenance = @{}
$missing = @()
foreach ($platform in $Platforms) {
    $artifact = Join-Path $artifactsAbs "vlc-$platform.tar.gz"
    if (-not (Test-Path -LiteralPath $artifact)) { $missing += "vlc-$platform.tar.gz"; continue }

    # Read build-info.txt straight out of the tarball; the runtime itself is not
    # needed to compare provenance.
    $infoText = tar -xzf $artifact -O build-info.txt
    if ($LASTEXITCODE -ne 0 -or -not $infoText) {
        throw "'$artifact' has no build-info.txt; it was not produced by build/vlc/build.ps1."
    }
    $provenance[$platform] = ConvertFrom-BuildInfo -Lines $infoText
}

if ($missing.Count -gt 0) {
    throw "missing artifact(s) in '$ArtifactsDir': $($missing -join ', ')"
}

$violations = Test-Provenance -Provenance $provenance -Fields @('vlc_commit', 'vlc_api_version_string')
if ($violations.Count -gt 0) {
    Write-Host 'FAILED:' -ForegroundColor Red
    $violations | ForEach-Object { Write-Host "  - $_" }
    Write-Host 'Rebuild both platforms from the same pin.'
    exit 1
}

foreach ($platform in $Platforms | Sort-Object) {
    $info = $provenance[$platform]
    Write-Host "$platform : commit $($info['vlc_commit'])  ABI $($info['vlc_api_version_string'])  ($($info['vlc_describe']))"
}
Write-Host 'check_vlc_provenance: OK' -ForegroundColor Green
