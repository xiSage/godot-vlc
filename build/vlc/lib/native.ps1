<#
Helpers for calling external tools from the build scripts.

PowerShell does not treat a non-zero exit code from a native command as an
error, so `git checkout` or `make` failing would silently continue and the build
would produce a broken artifact. Every external call in this directory goes
through these so a failure stops the build where it happened.
#>

function Invoke-Native {
    <#
    .SYNOPSIS
    Runs an external command and throws if it exits non-zero.
    #>
    param(
        [Parameter(Mandatory)][string]$Command,
        # Not Mandatory: a default of @() combined with Mandatory makes
        # PowerShell prompt for the value instead of using the default, which
        # turns a legitimate no-argument call such as `./bootstrap` into an
        # interactive prompt that fails in a non-interactive build.
        [AllowEmptyCollection()][AllowEmptyString()][string[]]$Arguments = @(),
        [string]$WorkingDirectory
    )

    $rendered = "$Command $($Arguments -join ' ')"
    Write-Host "  $ $rendered"

    if ($WorkingDirectory) {
        Push-Location -LiteralPath $WorkingDirectory
        try { & $Command @Arguments } finally { Pop-Location }
    } else {
        & $Command @Arguments
    }

    if ($LASTEXITCODE -ne 0) {
        throw "'$rendered' exited with $LASTEXITCODE"
    }
}

function Get-NativeOutput {
    <#
    .SYNOPSIS
    Runs an external command and returns its exit code plus its combined output.

    .DESCRIPTION
    For tools whose output is parsed rather than merely displayed. The caller
    decides what a non-zero exit code means, because tools such as `patchelf
    --print-rpath` legitimately fail on a file that has no runpath.
    #>
    param(
        [Parameter(Mandatory)][string]$Command,
        # See Invoke-Native: Mandatory plus a default prompts instead of using it.
        [AllowEmptyCollection()][AllowEmptyString()][string[]]$Arguments = @()
    )

    $output = & $Command @Arguments 2>&1 | ForEach-Object { "$_" }
    # $LASTEXITCODE must be read before anything else runs.
    $exitCode = $LASTEXITCODE
    [pscustomobject]@{ ExitCode = $exitCode; Output = @($output) }
}
