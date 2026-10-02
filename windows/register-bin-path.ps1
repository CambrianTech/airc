[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$BinDirectory)
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
Initialize-InstallerPowerShell
$ErrorActionPreference = 'Stop'
$current = [Environment]::GetEnvironmentVariable('PATH','User')
$entries = @($current -split ';' | Where-Object { $_ })
if ($entries -notcontains $BinDirectory) {
    [Environment]::SetEnvironmentVariable('PATH', (($entries + $BinDirectory) -join ';'), 'User')
}

} # Installer diagnostic process boundary.
