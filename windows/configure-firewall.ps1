# Process adapter for the shared setup coordinator. Never embed filesystem paths
# in PowerShell source: -File preserves spaces, apostrophes and literal dollars.
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$AircPath)
$ErrorActionPreference = 'Stop'
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$helper = Join-Path $PSScriptRoot 'firewall-allow.ps1'
& $powershell -NoProfile -ExecutionPolicy RemoteSigned -File $helper -AircPath $AircPath -CheckOnly
if ($LASTEXITCODE -eq 0) { exit 0 }
Write-Host 'AIRC setup will configure TCP and UDP access for this app from the local subnet.'
Write-Host 'Windows may ask administrator consent for PowerShell. Setup waits; no firewall profile checkboxes need to be configured manually.'
$logPath = [IO.Path]::GetTempFileName()
$arguments = '-NoProfile -ExecutionPolicy RemoteSigned -File "{0}" -AircPath "{1}" -LogPath "{2}"' -f $helper,$AircPath,$logPath
try {
    $process = Start-Process -FilePath $powershell -Verb RunAs -WindowStyle Hidden -ArgumentList $arguments -PassThru
    $null = $process.Handle
    $process.WaitForExit()
    $result = $process.ExitCode
    $process.Dispose()
} catch { throw "Windows firewall consent failed: $($_.Exception.Message). Rerun AIRC setup to resume." }
finally {
    # RunAs cannot redirect stdout. Preserve elevated diagnostics in setup's
    # visible output instead of losing them with the hidden child window.
    if (Test-Path -LiteralPath $logPath) {
        Get-Content -LiteralPath $logPath | ForEach-Object { Write-Host $_ }
        Remove-Item -LiteralPath $logPath -Force
    }
}
if ($result -ne 0) { throw "Windows firewall setup failed (exit $result). Rerun AIRC setup to resume." }
& $powershell -NoProfile -ExecutionPolicy RemoteSigned -File $helper -AircPath $AircPath -CheckOnly
if ($LASTEXITCODE -eq 4) {
    # Apply verifies effective policy inside the elevated child before exit 0.
    Write-Host 'AIRC firewall policy was verified by the administrator setup process; this Windows account cannot read firewall policy without elevation.'
} elseif ($LASTEXITCODE -ne 0) {
    throw 'Firewall verification failed after applying the rule. AIRC setup is incomplete.'
}
