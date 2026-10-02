# Normal-token adoption, with one narrowly verified elevated-owner recovery.
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$AircPath)
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
    Initialize-InstallerPowerShell
    $ErrorActionPreference='Stop'
    . (Join-Path $PSScriptRoot 'shared-setup.ps1')
    if (Test-IsAdmin) { throw 'AIRC adoption must run from the normal user token, not an administrator token.' }
    $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $endpoint=@(Invoke-InstallerProcess -OwnProcessTree $AircPath @('ipc-endpoint','--native'))
    if ($LASTEXITCODE -ne 0 -or $endpoint.Count -ne 1) { throw 'Installed binary could not resolve its canonical endpoint.' }
    $arguments=@('setup-recover-elevated-owner','--endpoint',[string]$endpoint[0],'--caller-sid',$sid,'--installed-binary',$AircPath)
    Invoke-InstallerProcess -OwnProcessTree $AircPath ($arguments+@('--probe'))
    $probe=$LASTEXITCODE
    if ($probe -ne 0 -and $probe -ne 5) { throw "Endpoint observation failed (exit $probe); adoption refused." }
    try {
        Initialize-ElevationSession
        if ($probe -eq 5) {
            Invoke-Elevated -Reason 'verifying and gracefully stopping the selected same-account elevated AIRC owner' -CommandLine (@($AircPath)+$arguments)
            if ($LASTEXITCODE -ne 0) { throw "Elevated-owner recovery refused or failed (exit $LASTEXITCODE); no forced stop or permission change attempted." }
        }
        Invoke-InstallerProcess -OwnProcessTree -PreserveChildrenOnSuccess $AircPath @('update','--adopt-installed')
        if ($LASTEXITCODE -ne 0) { throw "Installed build adoption failed (exit $LASTEXITCODE)." }
    } finally { Clear-Elevation }
}
