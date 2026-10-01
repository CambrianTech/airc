# Native process owner for direct Git Bash installs/updates. The shared Bash
# coordinator still performs every install stage; this adapter only owns consent.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$SourceDirectory,
    [Parameter(Mandatory=$true)][string]$BashPath,
    [string]$PrepareArtifact,
    [string]$PrebuiltArtifact,
    [string]$ExpectedBuild
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'shared-setup.ps1')
try {
    Initialize-ElevationSession
    $arguments = @('--noprofile', '--norc', ((Join-Path $SourceDirectory 'install.sh') -replace '\\','/'))
    if ($PrepareArtifact) { $arguments += @('--prepare-artifact', ($PrepareArtifact -replace '\\','/')) }
    if ($PrebuiltArtifact) { $arguments += @('--prebuilt', ($PrebuiltArtifact -replace '\\','/')) }
    if ($ExpectedBuild) { $arguments += @('--expected-build', $ExpectedBuild) }
    & $BashPath @arguments
    $result = $LASTEXITCODE
} finally { Clear-Elevation }
exit $result
