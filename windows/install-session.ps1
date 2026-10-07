# One native owner across prepare, maintenance, publication and recovery.
[CmdletBinding(DefaultParameterSetName='Install')]
param(
    [Parameter(Mandatory=$true)][string]$SourceDirectory,
    [Parameter(Mandatory=$true,ParameterSetName='Install')][string]$BashPath,
    [Parameter(ParameterSetName='Install')][string]$PrepareArtifact,
    [Parameter(ParameterSetName='Install')][string]$PrebuiltArtifact,
    [Parameter(ParameterSetName='Install')][string]$ExpectedBuild,
    [Parameter(Mandatory=$true,ParameterSetName='Update')][string]$UpdaterPath,
    [Parameter(Mandatory=$true,ParameterSetName='Update')][string]$HomePath,
    [Parameter(ParameterSetName='Update')][switch]$AutoUpdate,
    [Parameter(Mandatory=$true,ParameterSetName='Validate')][switch]$ValidateOnly
)
$sessionMode = $PSCmdlet.ParameterSetName
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
Initialize-InstallerPowerShell
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'shared-setup.ps1')
if ($ValidateOnly) {
    if (-not $env:CAMBRIAN_INSTALL_ELEVATION) { throw 'Updater validation requires an existing elevation owner.' }
    Initialize-ElevationSession
    if ($env:AIRC_UPDATE_SESSION_OWNER -and $env:AIRC_UPDATE_SESSION_OWNER -ne [string]$script:InstallElevationSession.OwnerPid) {
        throw 'Updater owner marker does not match the validated elevation owner.'
    }
    exit 0
}
$previousUpdateOwner = $env:AIRC_UPDATE_SESSION_OWNER
try {
    Initialize-ElevationSession
    if ($sessionMode -eq 'Update') {
        if ($script:InstallElevationSession.Borrowed -or $previousUpdateOwner) { throw 'Updater owner cannot reenter an existing session.' }
        if (-not (Get-Command Invoke-InstallerProcess).Parameters.ContainsKey('PreserveChildrenOnExitCode')) {
            throw 'Shared setup helper lacks verified recovery handoff support.'
        }
        $env:AIRC_UPDATE_SESSION_OWNER = [string]$PID
        $arguments = @('--home', $HomePath, 'update')
        if ($AutoUpdate) { $arguments += '--auto' }
        Invoke-InstallerProcess -OwnProcessTree -PreserveChildrenOnSuccess -PreserveChildrenOnExitCode @(200) $UpdaterPath $arguments
        # Internal 200 means verified recovery, not successful update.
        $result = if ($LASTEXITCODE -eq 200) { 1 } else { $LASTEXITCODE }
    } else {
        $arguments = @('--noprofile', '--norc', ((Join-Path $SourceDirectory 'install.sh') -replace '\\','/'))
        if ($PrepareArtifact) { $arguments += @('--prepare-artifact', ($PrepareArtifact -replace '\\','/')) }
        if ($PrebuiltArtifact) { $arguments += @('--prebuilt', ($PrebuiltArtifact -replace '\\','/')) }
        if ($ExpectedBuild) { $arguments += @('--expected-build', $ExpectedBuild) }
        Invoke-InstallerProcess -OwnProcessTree -PreserveChildrenOnSuccess $BashPath $arguments
        $result = $LASTEXITCODE
    }
} finally {
    $env:AIRC_UPDATE_SESSION_OWNER = $previousUpdateOwner
    Clear-Elevation
}
exit $result
} # Installer diagnostic process boundary.
