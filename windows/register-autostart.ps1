# Shared by native installation and the Bash installer used by `airc update`.
# Only registration/repair can elevate; builds keep the invoking user's token.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$AircPath,
    [string]$UserSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
    [string]$UserHome = $env:USERPROFILE,
    [switch]$Elevated,
    [switch]$ExistingOnly,
    [switch]$RestartRequired
)
$ErrorActionPreference = 'Stop'
function Get-AircStartupTask {
    try { Get-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -ErrorAction Stop }
    catch { if ($_.CategoryInfo.Category -eq 'ObjectNotFound') { return $null }; throw }
}
function Get-AircStartupInstances {
    $service = New-Object -ComObject Schedule.Service
    $service.Connect()
    $instances = $service.GetFolder('\').GetTask('airc-join').GetInstances(0)
    for ($index = 1; $index -le $instances.Count; $index++) { $instances.Item($index).InstanceGuid }
}
function Assert-NoAircDaemon {
    # Console detach does not break Windows Job membership. A missing parent PID
    # cannot prove independence, so maintenance must own the daemon's absence.
    $processes = @(Get-CimInstance Win32_Process -Filter "Name='airc.exe'" -ErrorAction Stop)
    foreach ($process in $processes) {
        if (-not $process.CommandLine -or $process.CommandLine -match '(?:^|\s)daemon(?:\s|$)') {
            throw 'A live or uninspectable AIRC daemon prevents safe task restart. The running task was preserved; the new action applies at the next login. To apply it sooner, rerun the installer during a daemon maintenance window (airc stop before installation; airc join afterward).'
        }
    }
}
$restart = [bool]$RestartRequired
try {
$AircPath = (Resolve-Path -LiteralPath $AircPath -ErrorAction Stop).ProviderPath
$runner = Join-Path (Split-Path $AircPath) 'airc-join-hidden.ps1'
$source = Join-Path $PSScriptRoot 'run-join-hidden.ps1'
$runnerChanged = -not (Test-Path -LiteralPath $runner) -or
    (Get-FileHash -LiteralPath $source).Hash -ne (Get-FileHash -LiteralPath $runner).Hash
$pending = $runner + '.restart-required'
$logs = Join-Path $UserHome '.airc\logs'
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$existing = Get-AircStartupTask
if ($ExistingOnly -and -not $existing) { return }
if ($existing) {
    $owner = $existing.Principal.UserId
    if (-not $owner) { throw 'Cannot verify the existing AIRC startup task owner; no task changes were made.' }
    $ownerSid = if ($owner -match '^S-1-') { $owner } else {
        (New-Object Security.Principal.NTAccount($owner)).Translate([Security.Principal.SecurityIdentifier]).Value
    }
    if ($ownerSid -ne $UserSid) { throw 'The existing airc-join task belongs to another Windows account; no task changes were made.' }
}
$workingDirectory = $UserHome
if ($existing -and $existing.Actions[0].WorkingDirectory) {
    $workingDirectory = $existing.Actions[0].WorkingDirectory
}
$action = New-ScheduledTaskAction -Execute $shell -WorkingDirectory $workingDirectory `
    -Argument ('-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy RemoteSigned -File "' +
        $runner + '" -AircPath "' + $AircPath + '" -LogDirectory "' + $logs + '"')
$unchanged = $existing -and @($existing.Actions).Count -eq 1 -and
    $existing.Actions[0].Execute -eq $action.Execute -and
    $existing.Actions[0].Arguments -ceq $action.Arguments -and
    $existing.Actions[0].WorkingDirectory -eq $action.WorkingDirectory
$restart = $restart -or ($existing -and $existing.State -eq 'Running' -and
    (-not $unchanged -or $runnerChanged -or (Test-Path -LiteralPath $pending)))
if ($unchanged -and -not $runnerChanged -and -not $restart) { return }
if ($runnerChanged) { Copy-Item -LiteralPath $source -Destination $runner -Force }
if ($restart) { Set-Content -LiteralPath $pending -Value 'running supervisor needs updated action' }
if (-not $unchanged -and $existing) {
    # Retain the user's principal, triggers, restart policy and other settings.
    Set-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -Action $action -ErrorAction Stop | Out-Null
} elseif (-not $existing) {
    $sid = New-Object Security.Principal.SecurityIdentifier($UserSid)
    $userName = $sid.Translate([Security.Principal.NTAccount]).Value
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $userName
    $principal = New-ScheduledTaskPrincipal -UserId $UserSid -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
        -RestartInterval (New-TimeSpan -Minutes 2) -RestartCount 999 `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -Action $action `
        -Trigger $trigger -Principal $principal -Settings $settings -Force -ErrorAction Stop `
        -Description 'Keep this node on the airc mesh: start the scope daemon at logon, restart it if it dies.' | Out-Null
}
$verified = Get-AircStartupTask
if (-not $verified -or @($verified.Actions).Count -ne 1 -or
    $verified.Actions[0].Execute -ne $action.Execute -or $verified.Actions[0].Arguments -cne $action.Arguments -or
    $verified.Actions[0].WorkingDirectory -ne $action.WorkingDirectory) { throw 'AIRC task action verification failed; the running task was not stopped.' }
if ($restart) {
    if (-not $verified.Settings.Enabled) { throw 'Updated task is disabled; preserving its stopped/disabled policy instead of restarting it.' }
    # Suppress Task Scheduler's crash-restart race during the maintenance check.
    # The join feed itself subscribes; it does not spawn replacement daemons.
    Disable-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -ErrorAction Stop | Out-Null
    try {
        Assert-NoAircDaemon
        Start-Sleep -Milliseconds 200
        Assert-NoAircDaemon
        Stop-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -ErrorAction Stop
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        # Disabling a task can report State=Disabled while its instance lives.
        # IgnoreNew requires the old instance to disappear before starting again.
        while (@(Get-AircStartupInstances).Count -ne 0) {
            if ([DateTime]::UtcNow -ge $deadline) { throw 'Updated task registered, but the previous supervisor did not stop.' }
            Start-Sleep -Milliseconds 100
        }
    } finally { Enable-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -ErrorAction Stop | Out-Null }
    Start-ScheduledTask -TaskName 'airc-join' -TaskPath '\' -ErrorAction Stop
}
Remove-Item -LiteralPath $pending -ErrorAction SilentlyContinue
} catch {
    $accessDenied = $_.CategoryInfo.Category -eq 'PermissionDenied' -or
        $_.Exception -is [UnauthorizedAccessException] -or $_.Exception.HResult -eq -2147024891 -or
        $_.Exception.StatusCode -eq 'AccessDenied'
    if (-not $accessDenied -or $Elevated) { throw }
    Write-Host 'Windows requires elevation to repair the existing AIRC startup task. Only startup repair will run elevated.'
    $arguments = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'RemoteSigned', '-File',
        ('"' + $PSCommandPath + '"'), '-AircPath', ('"' + $AircPath + '"'),
        '-UserSid', $UserSid, '-UserHome', ('"' + $UserHome + '"'), '-Elevated')
    if ($restart) { $arguments += '-RestartRequired' }
    if ($ExistingOnly) { $arguments += '-ExistingOnly' }
    try {
        $repair = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') `
            -Verb RunAs -WindowStyle Hidden -ArgumentList $arguments -PassThru -ErrorAction Stop
    } catch { throw 'AIRC startup repair elevation was cancelled or rejected; rerun install.ps1 to retry startup registration. An already-current airc update skips installation.' }
    $null = $repair.Handle
    $repair.WaitForExit()
    $code = $repair.ExitCode
    $repair.Dispose()
    if ($code -ne 0) { throw 'Elevated AIRC startup repair failed; rerun install.ps1 to retry startup registration. An already-current airc update skips installation.' }
}
