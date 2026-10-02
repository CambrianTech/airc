# Hosted-only real medium-token public installation. No token mocks or runtime policy overrides.
param([ValidateSet('powershell','pwsh')][string]$Engine='powershell',[switch]$Child,[string]$Gate,[string]$ReadyPipe,[string]$OriginalSid)
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true'){throw 'Token-changing installation acceptance runs only on a disposable hosted runner.'}
$root=Split-Path $PSScriptRoot -Parent
. (Join-Path $root 'windows/shared-setup.ps1')
Add-Type -Path (Join-Path $root 'windows/daemon-diagnostics.cs')
function Read-ChildLogs {
    param($OutputTask,$ErrorTask)
    if(-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($OutputTask,$ErrorTask),10000)){throw 'Owned child exited but its output pipes did not close within 10 seconds'}
    if($OutputTask.Result){[Console]::Out.Write($OutputTask.Result)}
    if($ErrorTask.Result){[Console]::Error.Write($ErrorTask.Result)}
}
function Read-OwnToken {
    $name='airc-ci-token-'+[guid]::NewGuid().ToString('N')
    $server=New-Object IO.Pipes.NamedPipeServerStream($name,[IO.Pipes.PipeDirection]::InOut,1,[IO.Pipes.PipeTransmissionMode]::Byte,[IO.Pipes.PipeOptions]::Asynchronous)
    try { [AircDaemonDiagnostics]::Inspect(('\\.\pipe\'+$name)) } finally { $server.Dispose() }
}
if($Child){
    $token=Read-OwnToken
    if((Test-IsAdmin) -or $token.tokenElevated -ne $false -or $token.tokenIntegritySid -ne 'S-1-16-8192' -or $token.tokenUserSid -ne $OriginalSid){throw ('Runner did not produce the required actual normal token: '+($token|ConvertTo-Json -Compress))}
    # A native pipe observation supplies authoritative PID/token evidence. The
    # child birth time additionally prevents a PID-reuse race when retaining it.
    $self=Get-Process -Id $PID
    try {[IO.File]::WriteAllText(($Gate+'.identity'),(@{pid=$PID;startTicks=$self.StartTime.ToUniversalTime().Ticks}|ConvertTo-Json -Compress))}finally{$self.Dispose()}
    $name=$ReadyPipe.Substring('\\.\pipe\'.Length)
    $ready=New-Object IO.Pipes.NamedPipeServerStream($name,[IO.Pipes.PipeDirection]::InOut,1,[IO.Pipes.PipeTransmissionMode]::Byte,[IO.Pipes.PipeOptions]::Asynchronous)
    try {
        $deadline=[DateTime]::UtcNow.AddMinutes(2)
        while(-not (Test-Path -LiteralPath $Gate)) {if([DateTime]::UtcNow -gt $deadline){throw 'Timed out awaiting scoped CI consent'};Start-Sleep -Milliseconds 100}
    } finally { $ready.Dispose() }
    $env:CAMBRIAN_INSTALL_ELEVATION=$null
    $env:AIRC_UPDATE_SESSION_OWNER=$null
    $env:AIRC_DIR=Join-Path $env:USERPROFILE ('.airc/src-normal-'+$Engine)
    $env:AIRC_INSTALL_NO_PULL='1'
    New-Item -ItemType Directory -Force -Path $env:AIRC_DIR | Out-Null
    Get-ChildItem -LiteralPath $root -Force | Copy-Item -Destination $env:AIRC_DIR -Recurse -Force
    & (Join-Path $env:AIRC_DIR 'install.ps1')
    if($LASTEXITCODE -ne 0){throw "Public installation failed with $LASTEXITCODE"}
    $installed=Join-Path $env:LOCALAPPDATA 'Programs/airc/airc.exe'
    try {
        Invoke-InstallerProcess -OwnProcessTree $installed @('doctor')
        if($LASTEXITCODE -ne 0){throw 'Installed normal-token doctor failed'}
        $endpoint=@(Invoke-InstallerProcess -OwnProcessTree $installed @('ipc-endpoint','--native'))
        if($LASTEXITCODE -ne 0 -or $endpoint.Count -ne 1){throw 'Installed endpoint resolution failed'}
        $daemon=[AircDaemonDiagnostics]::Inspect([string]$endpoint[0])
        if($daemon.tokenElevated -ne $false -or $daemon.tokenIntegritySid -ne 'S-1-16-8192' -or $daemon.tokenUserSid -ne $OriginalSid){throw ('Installed daemon token is not normal: '+($daemon|ConvertTo-Json -Compress))}
        Write-Host ('PASS: real public install and doctor under normal token; daemon receipt '+($daemon|ConvertTo-Json -Compress))
    } finally {
        if(Test-Path -LiteralPath $installed){Invoke-InstallerProcess -OwnProcessTree $installed @('stop');if($LASTEXITCODE -ne 0){throw 'Owned installed test daemon cleanup failed'}}
    }
    exit 0
}
if(-not (Test-IsAdmin)){throw 'Hosted supervisor must start elevated to grant explicit process-scoped test consent.'}
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$scratch=Join-Path $env:RUNNER_TEMP ('airc-medium-install-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$gate=Join-Path $scratch 'consent.ready'
$pipe='\\.\pipe\airc-medium-ready-'+[guid]::NewGuid().ToString('N')
$process=$null;$captured=$null;$cache=$false
try {
    Ensure-Gsudo
    $gsudo=Find-GsudoExecutable
    Invoke-InstallerProcess -OwnProcessTree $gsudo @('--version') | Out-Host
    if($LASTEXITCODE -ne 0){throw 'Cannot inspect managed gsudo'}
    $enginePath=if($Engine -eq 'powershell'){Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'}else{(Get-Command pwsh.exe -CommandType Application).Source}
    $arguments=@('--integrity','Medium','--direct','--wait',$enginePath,'-NoProfile','-ExecutionPolicy','RemoteSigned','-File',$PSCommandPath,'-Child','-Engine',$Engine,'-Gate',$gate,'-ReadyPipe',$pipe,'-OriginalSid',$sid)
    $quoted=foreach($value in $arguments){if($value -notmatch '[\s"]'){$value}else{'"'+$value+'"'}}
    $start=New-Object Diagnostics.ProcessStartInfo
    $start.FileName=$gsudo;$start.Arguments=$quoted -join ' ';$start.WorkingDirectory=$root
    $start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $process=[Continuum.Setup.OwnedProcessV2]::Start($start)
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    $deadline=[DateTime]::UtcNow.AddMinutes(2)
    do {
        if($process.HasExited){Read-ChildLogs $stdout $stderr;throw 'Medium child failed before consent'}
        $receipt=[AircDaemonDiagnostics]::Inspect($pipe)
        if($receipt.serverPid -ne 'UNKNOWN'){break}
        if([DateTime]::UtcNow -gt $deadline){throw 'No authoritative ready endpoint from medium child'}
        Start-Sleep -Milliseconds 100
    } while($true)
    if($receipt.tokenUserSid -ne $sid -or $receipt.tokenElevated -ne $false -or $receipt.tokenIntegritySid -ne 'S-1-16-8192' -or -not [string]::Equals($receipt.imagePath,$enginePath,[StringComparison]::OrdinalIgnoreCase)){throw ('Unverified medium child: '+($receipt|ConvertTo-Json -Compress))}
    $captured=Get-Process -Id ([int]$receipt.serverPid) -ErrorAction Stop
    $heldHandle=$captured.Handle # Retain the observed process object across consent/cleanup.
    $identity=Get-Content -LiteralPath ($gate+'.identity') -Raw | ConvertFrom-Json
    if($identity.pid -ne $receipt.serverPid -or [long]$identity.startTicks -ne $captured.StartTime.ToUniversalTime().Ticks){throw 'Observed ready owner changed before process capture'}
    $ancestor=[int]$receipt.serverPid;$seen=[Collections.Generic.HashSet[int]]::new()
    while($ancestor -ne $PID){if(-not $seen.Add($ancestor)){throw 'Invalid child ancestry'};$parent=Get-CimInstance Win32_Process -Filter "ProcessId=$ancestor" -ErrorAction Stop;if(-not $parent){throw 'Missing child ancestry'};$ancestor=[int]$parent.ParentProcessId}
    if($captured.HasExited){throw 'Observed child exited before scoped consent'}
    Invoke-InstallerProcess $gsudo @('cache','on','-p',[string]$receipt.serverPid,'-d','-1')
    if($LASTEXITCODE -ne 0){throw 'Explicit hosted process-scoped consent failed'}
    $cache=$true
    if($captured.HasExited){throw 'Observed child exited during consent'}
    [IO.File]::WriteAllText($gate,'approved')
    while(-not $process.WaitForExit(500)){if([DateTime]::UtcNow -gt $deadline.AddHours(1)){throw 'Installer coordinator exit was not observed'}}
    Read-ChildLogs $stdout $stderr
    if($process.ExitCode -ne 0){throw "Normal-token public install failed with exit $($process.ExitCode)"}
} finally {
    try {if($cache){Invoke-InstallerProcess $gsudo @('cache','off','-p',[string]$receipt.serverPid);if($LASTEXITCODE -ne 0){throw 'Scoped CI cache cleanup failed'}}}
    finally {if($process){$process.Dispose()};if($captured){$captured.Dispose()}}
}
exit 0
