# Public startup regression: no task registration or real AIRC process changes.
param([switch]$BoundaryParent, [switch]$BoundaryChild)
$ErrorActionPreference = 'Stop'
function Get-FixtureEntryFailure {
    param([scriptblock]$Action)
    $previous = [Console]::Error
    $capture = New-Object IO.StringWriter
    try {
        [Console]::SetError($capture)
        $global:LASTEXITCODE = 0
        & $Action | Out-Null
        if ($global:LASTEXITCODE -ne 1) { throw "Expected public entry exit 1, got $global:LASTEXITCODE" }
        return $capture.ToString()
    } finally { [Console]::SetError($previous); $capture.Dispose() }
}
$repo = Split-Path $PSScriptRoot
if ($BoundaryParent) {
    if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'Boundary parent must run in PowerShell 7' }
    # Reproduce PS7 -> Git Bash -> PS5 inheritance in this disposable process.
    $env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
    $inheritedModulePath = $env:PSModulePath
    $gitDirectory = [IO.DirectoryInfo]((& git --exec-path).Trim())
    if ($LASTEXITCODE -ne 0) { throw 'Cannot locate Git Bash for the boundary regression' }
    while ($gitDirectory -and -not (Test-Path -LiteralPath (Join-Path $gitDirectory.FullName 'bin/bash.exe'))) {
        $gitDirectory = $gitDirectory.Parent
    }
    if (-not $gitDirectory) { throw 'Git Bash is required for the public installer boundary regression' }
    $bash = Join-Path $gitDirectory.FullName 'bin/bash.exe'
    $launcher = Join-Path $repo 'windows/run-powershell.sh'
    & $bash $launcher -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File $PSCommandPath -BoundaryChild
    if ($LASTEXITCODE -ne 0) { throw 'Registrar fixture failed through the actual Bash to PS5 launcher' }
    if ($env:PSModulePath -cne $inheritedModulePath) { throw 'Bash launcher changed its parent module path' }
    & $bash $launcher -NoProfile -NonInteractive -Command 'exit 37'
    if ($LASTEXITCODE -ne 37) { throw 'Bash launcher lost the PowerShell exit status' }
    if ($env:PSModulePath -cne $inheritedModulePath) { throw 'Failed child changed its parent module path' }
    # Load the real pinned shared artifacts into a disposable cache, then prove
    # ownership through AIRC's exact PS7 -> Bash adapter -> PS5 path. Only context
    # handling runs: no package install, cache acquisition, task or firewall call.
    $boundaryCache = Join-Path ([IO.Path]::GetTempPath()) ('airc-elevation-boundary-' + [guid]::NewGuid().ToString('N'))
    $savedLocalAppData = $env:LOCALAPPDATA
    $savedElevationContext = $env:CAMBRIAN_INSTALL_ELEVATION
    $loadedElevation = $false
    try {
        $env:LOCALAPPDATA = $boundaryCache
        $env:CAMBRIAN_INSTALL_ELEVATION = $null
        . (Join-Path $repo 'windows/shared-setup.ps1')
        $loadedElevation = $true
        function Test-IsAdmin { $true } # prohibit real gsudo probes/cleanup
        Initialize-ElevationSession
        $loader = (Join-Path $repo 'windows/shared-setup.ps1').Replace("'", "''")
        $probe = @"
`$ErrorActionPreference = 'Stop'
try {
    . '$loader'
    Initialize-ElevationSession
    if (-not `$script:InstallElevationSession.Borrowed -or `$script:InstallElevationSession.OwnerPid -ne $PID) { throw 'AIRC adapter lost the outer installer owner.' }
    Clear-Elevation
    if (-not `$env:CAMBRIAN_INSTALL_ELEVATION) { throw 'AIRC child removed the outer context.' }
    Write-Output 'fixture AIRC borrowed owner verified'
} catch { Write-Output `$_.Exception.Message; exit 1 }
"@
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
        $output = @(& $bash $launcher -NoProfile -NonInteractive -EncodedCommand $encoded) -join "`n"
        if ($LASTEXITCODE -ne 0 -or $output -notmatch 'fixture AIRC borrowed owner verified') { throw "AIRC shared owner boundary failed: $output" }
        Write-Output 'PASS: pinned shared helper borrows the same owner through the actual AIRC shell adapter'
    } finally {
        try { if ($loadedElevation) { Clear-Elevation } }
        finally {
            $env:LOCALAPPDATA = $savedLocalAppData
            $env:CAMBRIAN_INSTALL_ELEVATION = $savedElevationContext
            $resolved = [IO.Path]::GetFullPath($boundaryCache)
            $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
            if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or (Split-Path $resolved -Leaf) -notlike 'airc-elevation-boundary-*') { throw 'Unsafe boundary cache cleanup' }
            if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
        }
    }
    Write-Output 'PASS: PS7 -> Git Bash -> PS5 registrar, unchanged parent environment, and exit 37'
    exit 0
}
if ($BoundaryChild) {
    if ($PSVersionTable.PSVersion.Major -ne 5) { throw 'Boundary child must run in Windows PowerShell 5' }
    # Assert before the fixture's task/CIM mocks can hide module-load failures.
    Get-Command Get-FileHash, Get-CimInstance, Get-ScheduledTask -ErrorAction Stop | Out-Null
}
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
    # Exercise the exact windowless task entry and its installed sibling path.
    Copy-Item -LiteralPath $runner -Destination (Join-Path $scratch 'airc-join-hidden.ps1')
    $entry = Join-Path $scratch 'airc-join-hidden.vbs'
    Copy-Item -LiteralPath (Join-Path $repo 'windows/run-join-hidden.vbs') -Destination $entry
    $hostProcess = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/wscript.exe') -WindowStyle Hidden -PassThru `
        -ArgumentList @('//B', '//Nologo', ('"' + $entry + '"'), ('"' + $binary + '"'), ('"' + $logs + '"'))
    $null = $hostProcess.Handle
    $hostProcess.WaitForExit()
    if ($hostProcess.ExitCode -ne 7 -or $elapsed.Elapsed.TotalSeconds -ge 5) { throw 'Join supervisor lost exit code or waited for detached daemon' }
    $hostProcess.Dispose()
    $descendant = Get-Process -Id ([int](Get-Content -LiteralPath ($binary + '.pid')))
    if ($descendant.HasExited) { throw 'Join supervisor killed its detached descendant' }
    if ((Get-Content -LiteralPath ($binary + '.console') -Raw) -ne 'visible=False;tty=False') { throw 'Join child was visible or retained a console-bound output stream' }
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
    std::process::exit(if context.runtime_label() == "supervisor" && context.should_stream_join() { 7 } else { 99 });
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

    if (-not $BoundaryChild) {
        $parentModulePath = $env:PSModulePath
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        & $pwsh -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File $PSCommandPath -BoundaryParent
        if ($LASTEXITCODE -ne 0) { throw 'PowerShell 7 installer boundary regression failed' }
        if ($env:PSModulePath -cne $parentModulePath) { throw 'Boundary regression changed the caller environment' }
    }

    # Existing tasks retain identity/settings/triggers; legacy runtime tokens migrate.
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
    function Set-ScheduledTask { param($TaskName, $TaskPath, $Action, $Principal, $ErrorAction)
        if ($global:aircStartupFixture.deny) { throw [UnauthorizedAccessException]::new('fixture access denied') }
        if ($global:aircStartupFixture.failRegistration) { throw 'fixture registration failure' }
        $global:aircStartupFixture.updated=$Action
        $global:aircStartupFixture.existing.Actions=@($Action)
        if (-not $global:aircStartupFixture.ignorePrincipal) { $global:aircStartupFixture.existing.Principal=$Principal }
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
        $global:aircStartupFixture.existing = [pscustomobject]@{ Actions=@($Action); State='Ready'; Settings=[pscustomobject]@{Enabled=$true}; Principal=$Principal }
    }
    function New-ScheduledTaskTrigger { param([switch]$AtLogOn, $User) $User }
    function New-ScheduledTaskPrincipal { param($UserId, $LogonType, $RunLevel) [pscustomobject]@{UserId=$UserId; LogonType=$LogonType; RunLevel=$RunLevel} }
    function New-ScheduledTaskSettingsSet { param([switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries, [switch]$StartWhenAvailable, $RestartInterval, $RestartCount, $ExecutionTimeLimit, $MultipleInstances)
        if ($RestartCount -ne 999 -or $RestartInterval.TotalMinutes -ne 2 -or $ExecutionTimeLimit -ne [TimeSpan]::Zero -or $MultipleInstances -ne 'IgnoreNew') { throw 'New-task recovery policy changed' }
        'expected-settings'
    }
    $registrarSource = Join-Path $scratch 'startup source'
    New-Item -ItemType Directory -Path $registrarSource | Out-Null
    foreach ($name in @('setup-entrypoint.ps1','register-autostart.ps1','run-join-hidden.ps1','run-join-hidden.vbs')) {
        Copy-Item -LiteralPath (Join-Path $repo "windows\$name") -Destination $registrarSource
    }
    [IO.File]::WriteAllText((Join-Path $registrarSource 'shared-setup.ps1'), 'function Initialize-ElevationSession { }; function Clear-Elevation { }')
    $registrar = Join-Path $registrarSource 'register-autostart.ps1'
    & $registrar -AircPath $binary
    if (-not $global:aircStartupFixture.updated -or $global:aircStartupFixture.registered -or $global:aircStartupFixture.updated.WorkingDirectory -ne $scratch) { throw 'Existing task was replaced or its working directory changed' }
    if ($global:aircStartupFixture.updated.Execute -ne (Join-Path $env:SystemRoot 'System32\wscript.exe') -or $global:aircStartupFixture.updated.Arguments -notlike '//B //Nologo *' -or $global:aircStartupFixture.updated.Arguments -notlike ('*"' + $binary + '"*')) { throw 'Task action lost windowless runner or exact binary path' }
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if ($global:aircStartupFixture.events.Count -ne 0) { throw 'Unchanged task was touched' }
    # Regression: a matching action previously preserved Highest/S4U forever.
    # Registration must normalize token policy without replacing settings or
    # stopping an existing daemon. Rerunning the corrected task is a no-op.
    $settingsBefore = $global:aircStartupFixture.existing.Settings
    foreach ($legacy in @(@('Interactive','Highest'), @('S4U','Limited'), @('S4U','Highest'))) {
        $global:aircStartupFixture.existing.Principal.LogonType=$legacy[0]
        $global:aircStartupFixture.existing.Principal.RunLevel=$legacy[1]
        $global:aircStartupFixture.events=@()
        & $registrar -AircPath $binary
        $principal=$global:aircStartupFixture.existing.Principal
        if (($global:aircStartupFixture.events -join ',') -ne 'register' -or $principal.LogonType -ne 'Interactive' -or $principal.RunLevel -ne 'Limited' -or -not [object]::ReferenceEquals($settingsBefore,$global:aircStartupFixture.existing.Settings)) { throw 'Legacy principal migration changed settings or retained an unsafe token' }
        $global:aircStartupFixture.events=@()
        & $registrar -AircPath $binary
        if ($global:aircStartupFixture.events.Count) { throw 'Corrected principal was not idempotent' }
    }
    $global:aircStartupFixture.existing.Principal.RunLevel='Highest'
    $global:aircStartupFixture.ignorePrincipal=$true
    $failed=$false
    $failed=(Get-FixtureEntryFailure { & $registrar -AircPath $binary }) -match 'principal verification failed'
    if (-not $failed -or ($global:aircStartupFixture.events -join ',') -ne 'register') { throw 'Unapplied principal change reported success or stopped the task' }
    $global:aircStartupFixture.ignorePrincipal=$false
    & $registrar -AircPath $binary
    $global:aircStartupFixture.events=@()
    $global:aircStartupFixture.existing.Principal.UserId='S-1-5-18'
    $failed=$false
    $failed=(Get-FixtureEntryFailure { & $registrar -AircPath $binary }) -match 'another Windows account'
    if (-not $failed -or $global:aircStartupFixture.events.Count -ne 0) { throw 'Another account startup task was modified' }
    $global:aircStartupFixture.existing.Principal.UserId=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $global:aircStartupFixture.existing.State='Running'
    $global:aircStartupFixture.existing.Actions[0].Arguments='old'
    & $registrar -AircPath $binary
    if (($global:aircStartupFixture.events -join ',') -ne 'register,disable,stop,instances,enable,start') { throw 'Task restart did not follow verified registration and maintenance check' }
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if ($global:aircStartupFixture.events.Count -ne 0) { throw 'Unchanged running task restarted' }
    $global:aircStartupFixture.existing.Principal.LogonType='S4U'
    $global:aircStartupFixture.existing.Principal.RunLevel='Highest'
    $global:aircStartupFixture.daemons=@([pscustomobject]@{CommandLine=$null})
    $failed=$false
    $failed=(Get-FixtureEntryFailure { & $registrar -AircPath $binary }) -match 'maintenance window'
    if (-not $failed -or ($global:aircStartupFixture.events -join ',') -ne 'register,disable,enable') { throw 'Legacy principal repair stopped an uninspectable daemon or claimed runtime recovery' }
    $global:aircStartupFixture.daemons=@()
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if (($global:aircStartupFixture.events -join ',') -ne 'disable,stop,instances,enable,start') { throw 'Principal-only migration forgot its pending restart' }
    $global:aircStartupFixture.events=@()
    $global:aircStartupFixture.existing.Actions[0].Arguments='old'
    $global:aircStartupFixture.failRegistration=$true
    $failed=$false
    $failed=(Get-FixtureEntryFailure { & $registrar -AircPath $binary }) -match 'fixture registration failure'
    if (-not $failed -or $global:aircStartupFixture.events.Count -ne 0) { throw 'Failed registration touched running task' }
    $global:aircStartupFixture.failRegistration=$false
    $global:aircStartupFixture.daemons=@([pscustomobject]@{CommandLine='airc.exe daemon'})
    $failed=$false
    $failed=(Get-FixtureEntryFailure { & $registrar -AircPath $binary }) -match 'maintenance window'
    if (-not $failed -or ($global:aircStartupFixture.events -join ',') -ne 'register,disable,enable') { throw 'Live daemon was endangered by task restart' }
    $global:aircStartupFixture.daemons=@()
    $global:aircStartupFixture.events=@()
    & $registrar -AircPath $binary
    if (($global:aircStartupFixture.events -join ',') -ne 'disable,stop,instances,enable,start') { throw 'Pending changed-action restart was forgotten on retry' }

    # Elevation is mocked: no UAC/task operation occurs in this regression.
    function Invoke-Elevated { param($Reason, $CommandLine)
        if ($Reason -notmatch 'original user') { throw 'Unexpected elevation request' }
        $global:aircStartupFixture.elevation=$CommandLine
        if ($global:aircStartupFixture.rejectElevation) { throw 'fixture UAC cancelled' }
        $global:LASTEXITCODE=0
    }
    $global:aircStartupFixture.existing.Actions[0].Arguments='old'
    $global:aircStartupFixture.deny=$true
    & $registrar -AircPath $binary -UserHome $scratch -ExistingOnly
    $elevation=$global:aircStartupFixture.elevation -join ' '
    if ($elevation -notlike ('*-UserSid ' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value + '*') -or $global:aircStartupFixture.elevation -notcontains $scratch -or $elevation -notlike '*-Elevated*' -or $elevation -notlike '*-ExistingOnly*') { throw 'Elevation lost original identity/home or widened installer scope' }
    $global:aircStartupFixture.rejectElevation=$true
    $failed=$false
    $failed=(Get-FixtureEntryFailure { & $registrar -AircPath $binary }) -match 'fixture UAC cancelled'
    if (-not $failed) { throw 'Rejected UAC was not reported clearly' }
    $global:aircStartupFixture.deny=$false
    $global:aircStartupFixture.existing = $null
    $global:aircStartupFixture.registered=$null
    & $registrar -AircPath $binary -ExistingOnly
    if ($global:aircStartupFixture.registered) { throw 'Bash install opted into new autostart' }
    & $registrar -AircPath $binary
    if (-not $global:aircStartupFixture.registered -or $global:aircStartupFixture.registered.TaskPath -ne '\' -or $global:aircStartupFixture.registered.Principal.UserId -ne [Security.Principal.WindowsIdentity]::GetCurrent().User.Value) { throw 'New task did not preserve current user identity/root task path' }
    # A failed registrar used to become a warning in the shared coordinator,
    # allowing setup to report success. Execute that exact Bash stage with a
    # controlled registrar exit; no task or elevation operation is performed.
    $installer = [IO.File]::ReadAllText((Join-Path $repo 'install.sh'))
    $stage = [regex]::Match($installer, '(?ms)^_setup_windows_autostart\(\) \{.*?^\}')
    if (-not $stage.Success) { throw 'Shared autostart stage was not found' }
    $stageFixture = Join-Path $scratch 'autostart-stage.sh'
    $prefix = @'
CLONE_DIR=fixture-source
BIN_DIR=fixture-bin
_to_win_path() { printf '%s\n' "$1"; }
ok() { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*"; }
fail() { printf '%s\n' "$*"; exit 1; }
'@
    # Override the process boundary only; the installer's branching and flags
    # remain unchanged. Arguments choose success/failure and native/Bash mode.
    $suffix = @'
registrar_exit="$1"
export AIRC_WINDOWS_NATIVE="$2"
_windows_powershell() { printf '%s\n' "$*"; return "$registrar_exit"; }
_setup_windows_autostart
printf 'fixture install continued\n'
'@
    [IO.File]::WriteAllText($stageFixture, ($prefix + "`n" + $stage.Value + "`n" + $suffix).Replace("`r`n", "`n"))
    $gitDirectory = [IO.DirectoryInfo]((& git --exec-path).Trim())
    while ($gitDirectory -and -not (Test-Path -LiteralPath (Join-Path $gitDirectory.FullName 'bin/bash.exe'))) { $gitDirectory = $gitDirectory.Parent }
    if (-not $gitDirectory) { throw 'Git Bash is required for the shared autostart regression' }
    foreach ($nativeMode in @('0', '1')) {
        foreach ($registrarExit in @('0', '73')) {
            $output = @(& (Join-Path $gitDirectory.FullName 'bin/bash.exe') --noprofile --norc $stageFixture $registrarExit $nativeMode) -join "`n"
            $code = $LASTEXITCODE
            if ($nativeMode -eq '0' -and $output -notmatch '-ExistingOnly') { throw 'Bash repair widened startup scope' }
            if ($nativeMode -eq '1' -and $output -match '-ExistingOnly') { throw 'Native setup lost startup registration' }
            if ($registrarExit -eq '0') {
                if ($code -ne 0 -or $output -notmatch 'fixture install continued') { throw 'Successful startup stage stopped setup' }
            } elseif ($code -eq 0 -or $output -match 'fixture install continued' -or $output -notmatch 'Setup is incomplete') {
                throw 'Shared installer suppressed startup failure'
            }
        }
    }
    Write-Output 'PASS: shared installer stops on startup failure in native and existing-only modes'
    Write-Output 'PASS: hidden join, logs, exit 7, surviving descendant, missing executable, and shared task registration'
} finally {
    if ($descendant -and -not $descendant.HasExited) { $descendant.Kill(); $descendant.WaitForExit(); $descendant.Dispose() }
    $resolved = [IO.Path]::GetFullPath($scratch)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or (Split-Path $resolved -Leaf) -notlike 'airc-autostart-test-*') { throw 'Unsafe fixture cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
exit 0
