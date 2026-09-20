# Task Scheduler owns this foreground supervisor. Keep the console-subsystem
# join client hidden, and report its real exit code for RestartOnFailure.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$AircPath,
    [Parameter(Mandatory = $true)][string]$LogDirectory
)
$ErrorActionPreference = 'Stop'
try {
    New-Item -ItemType Directory -Force -Path $LogDirectory | Out-Null
    $process = Start-Process -FilePath $AircPath -ArgumentList 'join' `
        -WindowStyle Hidden -PassThru `
        -RedirectStandardError (Join-Path $LogDirectory 'join.err.log')
    # Keep stdout on the child's hidden console. AIRC uses IsTerminal to retain
    # the live join feed; redirecting stdout makes plain logon join exit early.
    # The console buffer is bounded; messages already live in AIRC's store.
    # -Wait also waits descendants, including the intentionally detached daemon.
    # Pin the handle before waiting so Windows PowerShell 5.1 keeps ExitCode.
    $null = $process.Handle
    $process.WaitForExit()
    exit $process.ExitCode
} catch {
    Write-Error $_
    exit 1
}
