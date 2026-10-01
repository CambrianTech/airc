[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$BinDirectory)
$ErrorActionPreference = 'Stop'
$current = [Environment]::GetEnvironmentVariable('PATH','User')
$entries = @($current -split ';' | Where-Object { $_ })
if ($entries -notcontains $BinDirectory) {
    [Environment]::SetEnvironmentVariable('PATH', (($entries + $BinDirectory) -join ';'), 'User')
}
