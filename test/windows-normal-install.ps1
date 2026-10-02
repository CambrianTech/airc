# Hosted-only real medium-token public installation. No token mocks or runtime policy overrides.
param([ValidateSet('powershell','pwsh')][string]$Engine='powershell',[switch]$Child,[string]$Gate,[string]$ReadyPipe,[string]$OriginalSid,[string]$GsudoDirectory,[switch]$CancellationOnly,[switch]$CancelProbe)
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true'){throw 'Token-changing installation acceptance runs only on a disposable hosted runner.'}
$root=Split-Path $PSScriptRoot -Parent
. (Join-Path $root 'windows/setup-entrypoint.ps1')
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
    # Restore actual credentialed user's profile paths, never the supervisor's.
    $env:USERPROFILE=[Environment]::GetFolderPath('UserProfile')
    $env:LOCALAPPDATA=[Environment]::GetFolderPath('LocalApplicationData')
    $env:APPDATA=[Environment]::GetFolderPath('ApplicationData')
    $env:HOME=$env:USERPROFILE
    $env:TEMP=Join-Path $env:LOCALAPPDATA 'Temp';$env:TMP=$env:TEMP
    New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null
    $env:CARGO_HOME=$null;$env:RUSTUP_HOME=$null
    $env:PATH=$GsudoDirectory+';'+$env:PATH
    . (Join-Path $root 'windows/shared-setup.ps1')
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
    if($CancelProbe){
        Invoke-InstallerProcess -OwnProcessTree (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') @('-NoProfile','-File',(Join-Path (Split-Path $Gate -Parent) 'owned-descendant.ps1'),'-Receipt',($Gate+'.descendant'))
        throw 'Cancellation fixture unexpectedly completed'
    }
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
. (Join-Path $root 'windows/shared-setup.ps1')
if(-not (Test-IsAdmin)){throw 'Hosted supervisor must start elevated to grant explicit process-scoped test consent.'}
if(-not $CancellationOnly){
    Invoke-InstallerProcess -OwnProcessTree (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') @('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',$PSCommandPath,'-Engine',$Engine,'-CancellationOnly')
    if($LASTEXITCODE -ne 0){throw 'Hosted credentialed child cancellation proof failed'}
}
$testUser='aircci'+[guid]::NewGuid().ToString('N').Substring(0,12)
$secret=ConvertTo-SecureString ('aA9!'+[guid]::NewGuid().ToString('N')+[guid]::NewGuid().ToString('N')) -AsPlainText -Force
$account=$null
$scratch=Join-Path $env:RUNNER_TEMP ('airc-medium-install-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$gate=Join-Path $scratch 'consent.ready'
$pipe='\\.\pipe\airc-medium-ready-'+[guid]::NewGuid().ToString('N')
$process=$null;$captured=$null;$descendant=$null;$cache=$false
[IO.File]::WriteAllText((Join-Path $scratch 'owned-descendant.ps1'),@'
param([string]$Receipt)
$owned=Get-Process -Id $PID
[IO.File]::WriteAllText($Receipt,(@{pid=$PID;startTicks=$owned.StartTime.ToUniversalTime().Ticks}|ConvertTo-Json -Compress))
while($true){Start-Sleep -Seconds 1}
'@)
try {
    $account=New-LocalUser -Name $testUser -Password $secret -AccountNeverExpires -PasswordNeverExpires
    $users=Get-LocalGroup -SID 'S-1-5-32-545'
    Add-LocalGroupMember -Group $users -Member $account
    $sid=$account.SID.Value
    $acl=Get-Acl -LiteralPath $scratch
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sid,'Modify','ContainerInherit,ObjectInherit','None','Allow')))
    Set-Acl -LiteralPath $scratch -AclObject $acl
    Ensure-Gsudo
    $gsudo=Find-GsudoExecutable
    $gsudoDirectory=Join-Path $scratch 'gsudo'
    New-Item -ItemType Directory -Path $gsudoDirectory | Out-Null
    Get-ChildItem -LiteralPath (Split-Path $gsudo -Parent) | Copy-Item -Destination $gsudoDirectory -Recurse -Force
    $gsudo=Join-Path $gsudoDirectory ([IO.Path]::GetFileName($gsudo))
    Invoke-InstallerProcess -OwnProcessTree $gsudo @('--version') | Out-Host
    if($LASTEXITCODE -ne 0){throw 'Cannot inspect managed gsudo'}
    $enginePath=if($Engine -eq 'powershell'){Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'}else{(Get-Command pwsh.exe -CommandType Application).Source}
    $arguments=@('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',$PSCommandPath,'-Child','-Engine',$Engine,'-Gate',$gate,'-ReadyPipe',$pipe,'-OriginalSid',$sid,'-GsudoDirectory',$gsudoDirectory)
    if($CancellationOnly){$arguments+='-CancelProbe'}
    $quoted=foreach($value in $arguments){if($value -notmatch '[\s"]'){$value}else{'"'+$value+'"'}}
    $start=New-Object Diagnostics.ProcessStartInfo
    $start.FileName=$enginePath;$start.Arguments=$quoted -join ' ';$start.WorkingDirectory=$root
    $start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $start.UserName=$testUser;$start.Domain=$env:COMPUTERNAME;$start.Password=$secret;$start.LoadUserProfile=$true
    $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    $process=[Diagnostics.Process]::Start($start)
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
    if($CancellationOnly){
        [IO.File]::WriteAllText($gate,'cancellation fixture gate')
        $deadline=[DateTime]::UtcNow.AddSeconds(30)
        while(-not (Test-Path -LiteralPath ($gate+'.descendant'))){if($process.HasExited -or [DateTime]::UtcNow -gt $deadline){throw 'No owned cancellation descendant'};Start-Sleep -Milliseconds 100}
        $birth=Get-Content -LiteralPath ($gate+'.descendant') -Raw | ConvertFrom-Json
        $descendant=Get-Process -Id ([int]$birth.pid)
        $descendantHandle=$descendant.Handle
        if($descendant.StartTime.ToUniversalTime().Ticks -ne [long]$birth.startTicks -or (Get-CimInstance Win32_Process -Filter "ProcessId=$($birth.pid)").ParentProcessId -ne $captured.Id){throw 'Cancellation descendant ownership changed'}
        $process.Kill()
        if(-not $process.WaitForExit(10000) -or -not $descendant.WaitForExit(10000)){throw 'Cancellation left an owned descendant alive'}
        Read-ChildLogs $stdout $stderr
        Write-Host 'PASS: actual standard-account cancellation closes inner owned descendant'
    } else {
    Invoke-InstallerProcess $gsudo @('cache','on','-p',[string]$receipt.serverPid,'-d','-1')
    if($LASTEXITCODE -ne 0){throw 'Explicit hosted process-scoped consent failed'}
    $cache=$true
    if($captured.HasExited){throw 'Observed child exited during consent'}
    [IO.File]::WriteAllText($gate,'approved')
    while(-not $process.WaitForExit(500)){if([DateTime]::UtcNow -gt $deadline.AddHours(1)){throw 'Installer coordinator exit was not observed'}}
    Read-ChildLogs $stdout $stderr
    if($process.ExitCode -ne 0){throw "Normal-token public install failed with exit $($process.ExitCode)"}
    }
} finally {
    try {if($cache){Invoke-InstallerProcess $gsudo @('cache','off','-p',[string]$receipt.serverPid);if($LASTEXITCODE -ne 0){throw 'Scoped CI cache cleanup failed'}}}
    finally {
        try {
            if($process -and -not $process.HasExited){$process.Kill();if(-not $process.WaitForExit(10000)){throw 'Owned fixture child did not exit; account retained'}}
            if($captured -and -not $captured.HasExited){throw 'Captured fixture child remains alive; account retained'}
            if($account){
                $cleanupDeadline=[DateTime]::UtcNow.AddSeconds(15)
                do {
                    $owned=@(Get-CimInstance Win32_Process | Where-Object {try{(Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid -ErrorAction Stop).Sid -eq $sid}catch{$false}})
                    if(-not $owned.Count){break}
                    if([DateTime]::UtcNow -gt $cleanupDeadline){throw 'Fixture user processes remain alive; account/profile retained'}
                    Start-Sleep -Milliseconds 200
                }while($true)
                $profile=Get-CimInstance Win32_UserProfile -Filter "SID='$($account.SID.Value)'"
                if($profile){
                    $expected=[IO.Path]::GetFullPath((Join-Path $env:SystemDrive ('Users/'+$testUser)))
                    if($profile.Loaded -or [IO.Path]::GetFullPath($profile.LocalPath) -ne $expected){throw 'Fixture profile still loaded or unexpected; retaining account/profile'}
                    $profile | Remove-CimInstance
                }
                Remove-LocalUser -SID $account.SID
            }
        } finally {if($process){$process.Dispose()};if($captured){$captured.Dispose()};if($descendant){$descendant.Dispose()};$secret.Dispose()}
    }
}
exit 0
