# Read-only adapter, entered through install.ps1's existing elevation owner.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Endpoint,
    [Parameter(Mandatory=$true)][string]$CallerSid
)
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
    Initialize-InstallerPowerShell
    $ErrorActionPreference = 'Stop'
    if ($CallerSid -notmatch '^S-1-(?:[0-9]+-)+[0-9]+$') { throw 'Invalid original caller SID.' }
    Add-Type -Path (Join-Path $PSScriptRoot 'daemon-diagnostics.cs')
    $receipt = [AircDaemonDiagnostics]::Inspect($Endpoint)
    $receipt['callerSid'] = $CallerSid
    $receipt['diagnosticOnly'] = $true
    # UNKNOWN is evidence, not absence and not authorization to replace a daemon.
    $receipt | ConvertTo-Json -Depth 5 -Compress
}
