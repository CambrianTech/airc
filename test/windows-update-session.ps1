# Actual public session entry with synthetic gsudo/updater. No live elevation.
param([string]$SharedHelper, [switch]$CheckBash)
$ErrorActionPreference='Stop'
if ($CheckBash -and $env:GITHUB_ACTIONS -ne 'true') { throw 'Unadapted Bash descendant visibility runs only on the hosted CI desktop.' }
if (-not $SharedHelper) { . "$PSScriptRoot/../windows/shared-setup.ps1"; $SharedHelper=$aircSetupElevation }
. $SharedHelper
$scratch=Join-Path ([IO.Path]::GetTempPath()) ('airc-owner-test-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$saved=@{}
foreach($name in @('CAMBRIAN_INSTALL_ELEVATION','AIRC_UPDATE_SESSION_OWNER','AIRC_SESSION_TEST_LOG','AIRC_SESSION_TEST_PHASE','AIRC_SESSION_TEST_CODE','AIRC_SESSION_TEST_BASH','AIRC_SESSION_TEST_BASH_SCRIPT','AIRC_SESSION_TEST_BRIDGE','AIRC_SESSION_TEST_WINDOWS')) { $saved[$name]=[Environment]::GetEnvironmentVariable($name) }
try {
    $env:CAMBRIAN_INSTALL_ELEVATION=$null; $env:AIRC_UPDATE_SESSION_OWNER=$null
    $windows=Join-Path $scratch 'windows'
    New-Item -ItemType Directory -Path $windows | Out-Null
    Copy-Item "$PSScriptRoot/../windows/install-session.ps1" $windows
    Copy-Item "$PSScriptRoot/../windows/setup-entrypoint.ps1" $windows
    Copy-Item $SharedHelper (Join-Path $windows 'helper.ps1')
    @'
. (Join-Path $PSScriptRoot 'helper.ps1')
function Test-IsAdmin { $false }
function Find-GsudoExecutable { Join-Path $PSScriptRoot 'fake-gsudo.exe' }
'@ | Set-Content (Join-Path $windows 'shared-setup.ps1')
    @'
using System; using System.IO;
public static class FakeGsudo {
 public static int Main(string[] args) {
  File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_LOG"),String.Join(" ",args)+"\n");
  if(args[0]=="status") { Console.WriteLine("false"); return 1; }
  return 0;
 }
}
'@ | Set-Content (Join-Path $windows 'fake-gsudo.cs')
    Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\Microsoft.NET\Framework64\v4.0.30319\csc.exe" @('/nologo','/target:exe',('/out:'+(Join-Path $windows 'fake-gsudo.exe')),(Join-Path $windows 'fake-gsudo.cs'))
    if ($LASTEXITCODE -ne 0) { throw 'Synthetic gsudo compile failed.' }
    $phase=Join-Path $scratch 'phase.ps1'
    @'
param($Windows,[string]$AircPath,[string]$ScopeHome)
if($AircPath){$Windows=$env:AIRC_SESSION_TEST_WINDOWS}
$ErrorActionPreference='Stop'
. (Join-Path $Windows 'shared-setup.ps1')
Invoke-InstallerEntryPoint {
 Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class Visibility { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); }'
 if ([Visibility]::IsWindowVisible([Visibility]::GetConsoleWindow())) { throw 'Visible updater descendant.' }
 try { Initialize-ElevationSession; Ensure-Elevated 'synthetic phase' }
 finally { Clear-Elevation }
}
'@ | Set-Content $phase
    $env:AIRC_SESSION_TEST_PHASE=$phase
    $env:AIRC_SESSION_TEST_BASH=$null
    if ($CheckBash) {
        $gitPath=@(Invoke-InstallerProcess -OwnProcessTree 'git' @('--exec-path'))
        if($LASTEXITCODE -ne 0){throw 'Cannot resolve registered Git Bash for the hosted boundary test.'}
        $gitDirectory=[IO.DirectoryInfo](($gitPath -join '').Trim())
        while($gitDirectory -and -not (Test-Path -LiteralPath (Join-Path $gitDirectory.FullName 'bin/bash.exe'))) { $gitDirectory=$gitDirectory.Parent }
        if(-not $gitDirectory){throw 'Git Bash is missing; refusing a WSL substitute.'}
        $env:AIRC_SESSION_TEST_BASH=Join-Path $gitDirectory.FullName 'bin/bash.exe'
        $env:AIRC_SESSION_TEST_BRIDGE=[IO.Path]::GetFullPath("$PSScriptRoot/../windows/run-powershell.sh").Replace('\','/')
        $env:AIRC_SESSION_TEST_WINDOWS=$windows.Replace('\','/')
        $env:AIRC_SESSION_TEST_PHASE=$phase.Replace('\','/')
        $env:AIRC_SESSION_TEST_BASH_SCRIPT=Join-Path $scratch 'borrow.sh'
        # Exercise the actual public adoption callsite, not a hand-written
        # substitute which could hide a direct-shebang ancestry regression.
        Copy-Item $env:AIRC_SESSION_TEST_BRIDGE (Join-Path $windows 'run-powershell.sh')
        Copy-Item $phase (Join-Path $windows 'adopt-installed.ps1')
        $public=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '../install.sh')).Replace("`r`n","`n")
        $function=[regex]::Match($public,'(?ms)^_windows_powershell\(\) \{.*?^\}')
        $adoption=[regex]::Match($public.Substring($public.IndexOf('# Direct installation starts/verifies')), '(?ms)^  case "\$\(uname -s\)" in\n    MINGW.*?^  esac')
        if(-not $function.Success -or -not $adoption.Success){throw 'Public adoption shell boundary is missing'}
        $script='set -e'+"`n"+'CLONE_DIR="$AIRC_SESSION_TEST_WINDOWS/.."'+"`n"+'installed_airc="$AIRC_SESSION_TEST_WINDOWS/fake-gsudo.exe"'+"`n"+'fail() { echo "$*" >&2; exit 1; }'+"`n"+$function.Value+"`n"+$adoption.Value
        [IO.File]::WriteAllText($env:AIRC_SESSION_TEST_BASH_SCRIPT,$script)

    }
    @'
using System; using System.IO; using System.Diagnostics;
public static class FakeUpdater {
 public static int Main(string[] args) {
  File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_LOG"),"args:"+String.Join("|",args)+"\n");
  var windows=Path.Combine(Path.GetDirectoryName(Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_PHASE")),"windows");
  for(int i=0;i<2;i++) {
   var p=new ProcessStartInfo(Path.Combine(Environment.GetEnvironmentVariable("SystemRoot"),"System32","WindowsPowerShell","v1.0","powershell.exe"));
   p.Arguments="-NoProfile -ExecutionPolicy RemoteSigned -File \""+Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_PHASE")+"\" \""+windows+"\"";
   var bash=Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_BASH");
   if(!String.IsNullOrEmpty(bash)) { p.FileName=bash; p.Arguments="--noprofile --norc \""+Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_BASH_SCRIPT").Replace('\\','/')+"\""; }
   p.UseShellExecute=false; p.CreateNoWindow=true; p.RedirectStandardOutput=true; p.RedirectStandardError=true;
   using(var c=Process.Start(p)) { var o=c.StandardOutput.ReadToEndAsync(); var e=c.StandardError.ReadToEndAsync(); c.WaitForExit(); Console.Write(o.Result); Console.Error.Write(e.Result); if(c.ExitCode!=0)return c.ExitCode; }
  }
  return Int32.Parse(Environment.GetEnvironmentVariable("AIRC_SESSION_TEST_CODE"));
 }
}
'@ | Set-Content (Join-Path $scratch 'fake-updater.cs')
    Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\Microsoft.NET\Framework64\v4.0.30319\csc.exe" @('/nologo','/target:exe',('/out:'+(Join-Path $scratch 'fake-updater.exe')),(Join-Path $scratch 'fake-updater.cs'))
    if ($LASTEXITCODE -ne 0) { throw 'Synthetic updater compile failed.' }
    foreach($case in @(@{Auto=$false;Code=0},@{Auto=$true;Code=200},@{Auto=$false;Code=23})) {
        $env:AIRC_SESSION_TEST_LOG=Join-Path $scratch ([guid]::NewGuid().ToString('N')+'.log')
        $env:AIRC_SESSION_TEST_CODE=[string]$case.Code
        $arguments=@('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',(Join-Path $windows 'install-session.ps1'),'-SourceDirectory',$scratch,'-UpdaterPath',(Join-Path $scratch 'fake-updater.exe'),'-HomePath',(Join-Path $scratch 'home with spaces'))
        if($case.Auto) { $arguments+='-AutoUpdate' }
        $output=@(Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" $arguments 2>&1)
        $expected=if($case.Code -eq 200){1}else{$case.Code}
        if($LASTEXITCODE -ne $expected) {throw "Public status changed: $LASTEXITCODE expected $expected; $output"}
        $lines=@(Get-Content $env:AIRC_SESSION_TEST_LOG)
        $on=@($lines | Where-Object {$_ -match '^cache on -p (\d+) -d -1$'})
        $off=@($lines | Where-Object {$_ -match '^cache off -p (\d+)$'})
        if($on.Count -ne 2 -or $off.Count -ne 1) {throw "Expected two borrowed phases and one cleanup: $lines"}
        $owners=@($on | ForEach-Object {($_ -split ' ')[3]} | Select-Object -Unique)
        if($owners.Count -ne 1 -or ($off[0] -split ' ')[3] -ne $owners[0]) {throw "Elevation owner changed: $lines"}
        $wanted='args:--home|'+(Join-Path $scratch 'home with spaces')+'|update'
        if($case.Auto){$wanted+='|--auto'}
        if($lines -notcontains $wanted){throw "Update arguments changed: $lines"}
    }
    Write-Host 'PASS: public update session shares one owner across two phases, cleans once, preserves args and maps verified recovery to public failure.'
    $env:CAMBRIAN_INSTALL_ELEVATION=@{version=1;ownerPid=$PID;ownerStarted=(Get-Process -Id $PID).StartTime.ToUniversalTime().Ticks.ToString();existingCache=$false} | ConvertTo-Json -Compress
    foreach($marker in @([string]$PID,'1')) {
        $env:AIRC_UPDATE_SESSION_OWNER=$marker
        $validation=@(Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" @('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',(Join-Path $windows 'install-session.ps1'),'-SourceDirectory',$scratch,'-ValidateOnly') 2>&1)
        if($marker -eq [string]$PID) {
            if($LASTEXITCODE -ne 0){throw "Valid borrowed owner refused: $validation"}
        } elseif($LASTEXITCODE -eq 0 -or ($validation -join ' ') -notmatch 'marker does not match') {throw "Mismatched owner marker accepted: $validation"}
    }
    $env:AIRC_UPDATE_SESSION_OWNER=$null
    $env:CAMBRIAN_INSTALL_ELEVATION='{"version":1,"ownerPid":1,"ownerStarted":"0","existingCache":false}'
    $rejection=@(Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" @('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',(Join-Path $windows 'install-session.ps1'),'-SourceDirectory',$scratch,'-ValidateOnly') 2>&1)
    if($LASTEXITCODE -eq 0 -or ($rejection -join ' ') -notmatch 'Cannot borrow installer elevation'){throw "Stale owner accepted: $rejection"}
    Write-Host 'PASS: inherited stale context refuses before updater execution.'
} finally {
    foreach($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name]) }
    $resolved=[IO.Path]::GetFullPath($scratch)
    $parent=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')+'\'
    if(-not $resolved.StartsWith($parent,[StringComparison]::OrdinalIgnoreCase)){throw 'Unsafe test scratch path.'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
exit 0
