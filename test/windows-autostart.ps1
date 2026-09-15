# Public startup regression: no task registration or real AIRC process changes.
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('airc-autostart-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$descendant = $null
try {
    $binary = Join-Path $scratch "airc with spaces and ' quote.exe"
    Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
public class JoinFixture {
    [DllImport("kernel32.dll")] static extern IntPtr GetConsoleWindow();
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int kind);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    public static int Main(string[] args) {
        if (args.Length == 1 && args[0] == "descendant") {
            System.Threading.Thread.Sleep(15000); return 0;
        }
        if (args.Length != 1 || args[0] != "join") return 99;
        Console.WriteLine("join visible=" + IsWindowVisible(GetConsoleWindow()));
        Console.Error.WriteLine("failure receipt");
        var own = System.Reflection.Assembly.GetExecutingAssembly().Location;
        uint mode;
        File.WriteAllText(own + ".console", "visible=" + IsWindowVisible(GetConsoleWindow()) + ";tty=" + GetConsoleMode(GetStdHandle(-11), out mode));
        var child = Process.Start(new ProcessStartInfo(own, "descendant") {
            UseShellExecute = false, CreateNoWindow = true
        });
        File.WriteAllText(own + ".pid", child.Id.ToString());
        return 7;
    }
}
'@ -OutputAssembly $binary -OutputType ConsoleApplication
    $shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $logs = Join-Path $scratch 'logs'
    $runner = Join-Path $repo 'windows\run-join-hidden.ps1'
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    # Match Task Scheduler: the PowerShell host gets its own hidden console.
    # Calling it directly from this test's redirected CI stdout is a different
    # runtime and would make a child inherit the CI pipe instead of a console.
    $hostProcess = Start-Process -FilePath $shell -WindowStyle Hidden -PassThru `
        -ArgumentList @('-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden', '-ExecutionPolicy', 'RemoteSigned', '-File', ('"' + $runner + '"'), '-AircPath', ('"' + $binary + '"'), '-LogDirectory', ('"' + $logs + '"'))
    $null = $hostProcess.Handle
    $hostProcess.WaitForExit()
    if ($hostProcess.ExitCode -ne 7 -or $elapsed.Elapsed.TotalSeconds -ge 5) { throw 'Join supervisor lost exit code or waited for detached daemon' }
    $hostProcess.Dispose()
    $descendant = Get-Process -Id ([int](Get-Content -LiteralPath ($binary + '.pid')))
    if ($descendant.HasExited) { throw 'Join supervisor killed its detached descendant' }
    if ((Get-Content -LiteralPath ($binary + '.console') -Raw) -ne 'visible=False;tty=True') { throw 'Join child was visible or lost its terminal/streaming runtime' }
    if ((Get-Content (Join-Path $logs 'join.err.log') -Raw).Trim() -ne 'failure receipt') { throw 'Join stderr was lost' }
    # Compile the real runtime classifier without linking the daemon. Clear
    # agent/harness markers so this proves plain user-logon behavior, not Codex.
    $runtimeSource = Join-Path $scratch 'runtime_fixture.rs'
    $runtimeBinary = Join-Path $scratch 'runtime_fixture.exe'
    $classifier = Join-Path $repo 'crates\airc-cli\src\runtime_context.rs'
    $rust = @'
#![allow(dead_code)]
mod client_id {
    pub fn current_client_id() -> Result<Option<String>, std::io::Error> { Ok(None) }
}
#[path = r"@@CLASSIFIER@@"] mod runtime_context;
fn main() {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CARGO_") || name.starts_with("CODEX_") || name.starts_with("CLAUDE")
            || name == "AIRC_NO_ATTACH" || name == "AIRC_CODEX_START_CHILD" || name == "AI_AGENT" {
            std::env::remove_var(&key);
        }
    }
    let context = runtime_context::RuntimeContext::current();
    std::process::exit(if context.runtime_label() == "interactive" && context.should_stream_join() { 7 } else { 99 });
}
'@
    [IO.File]::WriteAllText($runtimeSource, $rust.Replace('@@CLASSIFIER@@', $classifier))
    & rustc --edition 2021 --crate-name runtime_fixture $runtimeSource -o $runtimeBinary
    if ($LASTEXITCODE -ne 0) { throw 'Runtime classifier fixture did not compile' }
    $runtimeHost = Start-Process -FilePath $shell -WindowStyle Hidden -PassThru `
        -ArgumentList @('-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden', '-ExecutionPolicy', 'RemoteSigned', '-File', ('"' + $runner + '"'), '-AircPath', ('"' + $runtimeBinary + '"'), '-LogDirectory', ('"' + $logs + '"'))
    $null = $runtimeHost.Handle
    $runtimeHost.WaitForExit()
    if ($runtimeHost.ExitCode -ne 7) { throw 'Actual AIRC runtime classifier would not retain the unattended join stream' }
    $runtimeHost.Dispose()
    $missing = Start-Process -FilePath $shell -WindowStyle Hidden -PassThru `
        -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'RemoteSigned', '-File', ('"' + $runner + '"'), '-AircPath', ('"' + (Join-Path $scratch 'missing.exe') + '"'), '-LogDirectory', ('"' + $logs + '"')) `
        -RedirectStandardError (Join-Path $scratch 'missing.err.log')
    $null = $missing.Handle
    $missing.WaitForExit()
    if ($missing.ExitCode -eq 0) { throw 'Missing join executable reported success' }
    $missing.Dispose()

    # Existing tasks retain their identity/settings/trigger; only action changes.
    $global:aircStartupFixture = @{ updated=$null; registered=$null; existing=$null; events=@(); daemons=@(); failRegistration=$false; deny=$false; rejectElevation=$false; elevation=$null }
    $global:aircStartupFixture.registered = $null
    $global:aircStartupFixture.existing = [pscustomobject]@{ Actions = @([pscustomobject]@{ WorkingDirectory = $scratch }); State='Ready'; Settings=[pscustomobject]@{Enabled=$true}; Principal=[pscustomobject]@{UserId=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value} }
    function Get-ScheduledTask { param($TaskName, $TaskPath, $ErrorAction)
        if ($TaskName -ne 'airc-join' -or $TaskPath -ne '\') { throw 'Wrong task selected' }
        $global:aircStartupFixture.existing
    }
    function New-ScheduledTaskAction { param($Execute, $Argument, $WorkingDirectory)
        [pscustomobject]@{ Execute=$Execute; Arguments=$Argument; WorkingDirectory=$WorkingDirectory }
    }
    function Set-ScheduledTask { param($TaskName, $TaskPath, $Action, $ErrorAction)
        if ($global:aircStartupFixture.deny) { throw [UnauthorizedAccessException]::new('fixture access denied') }
        if ($global:aircStartupFixture.failRegistration) { throw 'fixture registration failure' }
        $global:aircStartupFixture.updated=$Action
        $global:aircStartupFixture.existing.Actions=@($Action)
        $global:aircStartupFixture.events += 'register'
    }
    function Get-CimInstance { param($ClassName, $Filter, $ErrorAction) $global:aircStartupFixture.daemons }
    function Disable-ScheduledTask { param($TaskName, $TaskPath, $ErrorAction) $global:aircStartupFixture.events += 'disable'; $global:aircStartupFixture.existing.Settings.Enabled=$false }
    function Enable-ScheduledTask { param($TaskName, $TaskPath, $ErrorAction) $global:aircStartupFixture.events += 'enable'; $global:aircStartupFixture.existing.Settings.Enabled=$true }
    function Stop-ScheduledTask { param($TaskName, $TaskPath, $ErrorAction) $global:aircStartupFixture.events += 'stop'; $global:aircStartupFixture.existing.State='Ready' }
    function Start-ScheduledTask { param($TaskName, $TaskPath, $ErrorAction) $global:aircStartupFixture.events += 'start'; $global:aircStartupFixture.existing.State='Running' }
    function New-Object { param([Parameter(Position=0)]$TypeName, [Parameter(Position=1)]$ArgumentList, [string]$ComObject)
        if ($ComObject -eq 'Schedule.Service') {
            $service=[pscustomobject]@{}
            $service | Add-Member ScriptMethod Connect {}
            $service | Add-Member ScriptMethod GetFolder { param($Path) $this }
            $service | Add-Member ScriptMethod GetTask { param($Name) $this }
            $service | Add-Member ScriptMethod GetInstances { param($Flags)
                $global:aircStartupFixture.events += 'instances'
                [pscustomobject]@{Count=0}
            }
            return $service
        }
        Microsoft.PowerShell.Utility\New-Object @PSBoundParameters
    }
    function Register-ScheduledTask { param($TaskName, $TaskPath, $Action, $Trigger, $Principal, $Settings, [switch]$Force, $Description, $ErrorAction)
        $global:aircStartupFixture.registered = [pscustomobject]@{ Action=$Action; Trigger=$Trigger; Principal=$Principal; Settings=$Settings; TaskPath=$TaskPath }
        $global:aircStartupFixture.existing = [pscustomobject]@{ Actions=@($Action); State='Ready'; Settings=[pscustomobject]@{Enabled=$true}; Principal=[pscustomobject]@{UserId=$Principal} }
    }
    function New-ScheduledTaskTrigger { param([switch]$AtLogOn, $User) $User }
    function New-ScheduledTaskPrincipal { param($UserId, $LogonType, $RunLevel) $UserId }
    function New-ScheduledTaskSettingsSet { param([switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries, [switch]$StartWhenAvailable, $RestartInterval, $RestartCount, $ExecutionTimeLimit, $MultipleInstances)
        if ($RestartCount -ne 999 -or $RestartInterval.TotalMinutes -ne 2 -or $ExecutionTimeLimit -ne [TimeSpan]::Zero -or $MultipleInstances -ne 'IgnoreNew') { throw 'New-task recovery policy changed' }
        'expected-settings'
    }
    $registrar = Join-Path $repo 'windows\register-autostart.ps1'
    & $registrar -AircPath $binary
    if (-not $global:aircStartupFixture.updated -or $global:aircStartupFixture.registered -or $global:aircStartupFixture.updated.WorkingDirectory -ne $scratch) { throw 'Existing task was replaced or its working directory changed' }
    if ($global:aircStartupFixture.updated.Arguments -notlike '*-WindowStyle Hidden -ExecutionPolicy RemoteSigned -File*' -or $global:aircStartupFixture.updated.Arguments -notlike ('*-AircPath "' + $binary + '"*')) { throw 'Task action lost hidden runner or exact binary path' }
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if ($global:aircStartupFixture.events.Count -ne 0) { throw 'Unchanged task was touched' }
    $global:aircStartupFixture.existing.Principal.UserId='S-1-5-18'
    $failed=$false
    try { & $registrar -AircPath $binary } catch { $failed=$_ -match 'another Windows account' }
    if (-not $failed -or $global:aircStartupFixture.events.Count -ne 0) { throw 'Another account startup task was modified' }
    $global:aircStartupFixture.existing.Principal.UserId=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $global:aircStartupFixture.existing.State='Running'
    $global:aircStartupFixture.existing.Actions[0].Arguments='old'
    & $registrar -AircPath $binary
    if (($global:aircStartupFixture.events -join ',') -ne 'register,disable,stop,instances,enable,start') { throw 'Task restart did not follow verified registration and maintenance check' }
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if ($global:aircStartupFixture.events.Count -ne 0) { throw 'Unchanged running task restarted' }
    $global:aircStartupFixture.existing.Actions[0].Arguments='old'
    $global:aircStartupFixture.failRegistration=$true
    $failed=$false
    try { & $registrar -AircPath $binary } catch { $failed=$_ -match 'fixture registration failure' }
    if (-not $failed -or $global:aircStartupFixture.events.Count -ne 0) { throw 'Failed registration touched running task' }
    $global:aircStartupFixture.failRegistration=$false
    $global:aircStartupFixture.daemons=@([pscustomobject]@{CommandLine='airc.exe daemon'})
    $failed=$false
    try { & $registrar -AircPath $binary } catch { $failed=$_ -match 'maintenance window' }
    if (-not $failed -or ($global:aircStartupFixture.events -join ',') -ne 'register,disable,enable') { throw 'Live daemon was endangered by task restart' }
    $global:aircStartupFixture.daemons=@()
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if (($global:aircStartupFixture.events -join ',') -ne 'disable,stop,instances,enable,start') { throw 'Pending changed-action restart was forgotten on retry' }

    # Elevation is mocked: no UAC/task operation occurs in this regression.
    function Start-Process { param($FilePath, $Verb, $WindowStyle, $ArgumentList, [switch]$PassThru, $ErrorAction)
        if ($Verb -ne 'RunAs' -or $WindowStyle -ne 'Hidden') { throw 'Unexpected elevation request' }
        $global:aircStartupFixture.elevation=$ArgumentList
        if ($global:aircStartupFixture.rejectElevation) { throw 'fixture UAC cancelled' }
        $result=[pscustomobject]@{Handle=1;ExitCode=0}
        $result | Add-Member ScriptMethod WaitForExit {}
        $result | Add-Member ScriptMethod Dispose {}
        $result
    }
    $global:aircStartupFixture.existing.Actions[0].Arguments='old'
    $global:aircStartupFixture.deny=$true
    & $registrar -AircPath $binary -UserHome $scratch -ExistingOnly
    $elevation=$global:aircStartupFixture.elevation -join ' '
    if ($elevation -notlike ('*-UserSid ' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value + '*') -or $elevation -notlike ('*-UserHome "' + $scratch + '"*') -or $elevation -notlike '*-Elevated*' -or $elevation -notlike '*-ExistingOnly*') { throw 'Elevation lost original identity/home or widened installer scope' }
    $global:aircStartupFixture.rejectElevation=$true
    $failed=$false
    try { & $registrar -AircPath $binary } catch { $failed=$_ -match 'cancelled or rejected' }
    if (-not $failed) { throw 'Rejected UAC was not reported clearly' }
    $global:aircStartupFixture.deny=$false
    $global:aircStartupFixture.existing = $null
    $global:aircStartupFixture.registered=$null
    & $registrar -AircPath $binary -ExistingOnly
    if ($global:aircStartupFixture.registered) { throw 'Bash install opted into new autostart' }
    & $registrar -AircPath $binary
    if (-not $global:aircStartupFixture.registered -or $global:aircStartupFixture.registered.TaskPath -ne '\' -or $global:aircStartupFixture.registered.Principal -ne [Security.Principal.WindowsIdentity]::GetCurrent().User.Value) { throw 'New task did not preserve current user identity/root task path' }
    Write-Output 'PASS: hidden join, logs, exit 7, surviving descendant, missing executable, and shared task registration'
} finally {
    if ($descendant -and -not $descendant.HasExited) { $descendant.Kill(); $descendant.WaitForExit(); $descendant.Dispose() }
    $resolved = [IO.Path]::GetFullPath($scratch)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or (Split-Path $resolved -Leaf) -notlike 'airc-autostart-test-*') { throw 'Unsafe fixture cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
exit 0
