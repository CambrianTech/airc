# Process adapter for the shared setup coordinator. Never embed filesystem paths
# in PowerShell source: -File preserves spaces, apostrophes and literal dollars.
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$AircPath)
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
Initialize-InstallerPowerShell
$ErrorActionPreference = 'Stop'
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$helper = Join-Path $PSScriptRoot 'firewall-allow.ps1'
. (Join-Path $PSScriptRoot 'shared-setup.ps1')
$ownerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
Invoke-InstallerProcess $powershell @('-NoProfile', '-ExecutionPolicy', 'RemoteSigned', '-File', $helper, '-AircPath', $AircPath, '-OwnerSid', $ownerSid, '-CheckOnly')
if ($LASTEXITCODE -eq 0) { exit 0 }
Write-Host 'AIRC setup will configure TCP and UDP access for this app from the local subnet.'
Write-Host 'Setup uses its shared administrator session and waits for consent when needed.'
try {
    Initialize-ElevationSession
    Invoke-Elevated -Reason 'configuring AIRC TCP/UDP local-subnet firewall policy' -CommandLine @(
        $powershell, '-NoProfile', '-ExecutionPolicy', 'RemoteSigned', '-File', $helper, '-AircPath', $AircPath, '-OwnerSid', $ownerSid)
    $result = $LASTEXITCODE
} finally { Clear-Elevation }
if ($result -ne 0) { throw "Windows firewall setup failed (exit $result). Rerun AIRC setup to resume." }
Invoke-InstallerProcess $powershell @('-NoProfile', '-ExecutionPolicy', 'RemoteSigned', '-File', $helper, '-AircPath', $AircPath, '-OwnerSid', $ownerSid, '-CheckOnly')
if ($LASTEXITCODE -eq 4) {
    # Apply verifies effective policy inside the elevated child before exit 0.
    Write-Host 'AIRC firewall policy was verified by the administrator setup process; this Windows account cannot read firewall policy without elevation.'
} elseif ($LASTEXITCODE -ne 0) {
    throw 'Firewall verification failed after applying the rule. AIRC setup is incomplete.'
}

} # Installer diagnostic process boundary.
