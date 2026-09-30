# Task Scheduler owns this long-lived supervisor. Drain the live feed without
# allocating a console, and report the join client's exit code for restart.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$AircPath,
    [Parameter(Mandatory = $true)][string]$LogDirectory
)
$ErrorActionPreference = 'Stop'
$process = $null
$errorLog = $null
try {
    New-Item -ItemType Directory -Force -Path $LogDirectory | Out-Null
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $AircPath
    $start.Arguments = 'join'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables['AIRC_SUPERVISOR'] = '1'
    $process = [System.Diagnostics.Process]::Start($start)
    if (-not $process) { throw 'AIRC join supervisor did not start' }
    # The feed is durable in AIRC's event store. Drain it so a full pipe cannot
    # stall the join process; retain stderr for startup diagnostics.
    $errorLog = [System.IO.File]::Open((Join-Path $LogDirectory 'join.err.log'),
        [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    $stdout = $process.StandardOutput.BaseStream.CopyToAsync([System.IO.Stream]::Null)
    $stderr = $process.StandardError.BaseStream.CopyToAsync($errorLog)
    $process.WaitForExit()
    # A detached descendant may inherit the pipe writers. Its lifetime must not
    # delay reporting join's exit to Task Scheduler. Allow only a bounded flush.
    $null = [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]@($stdout, $stderr), 250)
    exit $process.ExitCode
} catch {
    Write-Error $_
    exit 1
} finally {
    if ($errorLog) { $errorLog.Dispose() }
    if ($process) { $process.Dispose() }
}
