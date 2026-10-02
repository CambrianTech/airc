# Executes the public native entry with isolated source/package fixtures. Never
# installs packages, edits the real user profile, or changes GitHub credentials.
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
$repository = Split-Path $PSScriptRoot -Parent
# Exercise only the registrar's pure ordering function, never real user PATH.
$tokens=$null; $errors=$null
$pathAst=[Management.Automation.Language.Parser]::ParseFile((Join-Path $repository 'windows/register-bin-path.ps1'),[ref]$tokens,[ref]$errors)
$pathFunction=@($pathAst.FindAll({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-AircRegisteredPath'},$true))
if ($errors.Count -or $pathFunction.Count -ne 1) { throw 'Missing PATH reconciliation function' }
. ([scriptblock]::Create($pathFunction[0].Extent.Text))
$selected="C:\Fixture O'Brien\Selected bin"
$other='C:\Fixture\Other bin'
foreach($current in @("$other;$selected", "$selected;$other", "$other;$($selected.ToUpperInvariant())\;$selected")) {
    $reconciled=Get-AircRegisteredPath $current $selected
    if ($reconciled -cne "$selected;$other") { throw "Selected PATH directory did not win uniquely: $reconciled" }
    if ((Get-AircRegisteredPath $reconciled $selected) -cne $reconciled) { throw 'PATH reconciliation is not idempotent' }
}
$fixture = Join-Path ([IO.Path]::GetTempPath()) ("airc setup O'Brien " + [guid]::NewGuid().ToString('N'))
$saved = @{}
foreach ($name in @('USERPROFILE','LOCALAPPDATA','PATH','BIN_DIR','BIN_TARGET','AIRC_DIR','AIRC_CHANNEL','AIRC_FIXTURE_LOG','AIRC_FIXTURE_GIT_EXEC','AIRC_FIXTURE_SOURCE')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name)
}
function Assert-True($condition,$message) { if (-not $condition) { throw $message } }
function New-Source($directory) {
    foreach ($relative in @('Cargo.toml','install.sh','setup/github-auth.sh','windows/install-prereqs.ps1','windows/run-powershell.sh','windows/register-bin-path.ps1','windows/configure-firewall.ps1','windows/setup-artifacts.lock.json','windows/install-session.ps1','windows/sync-bootstrap.ps1','windows/setup-entrypoint.ps1')) {
        $path = Join-Path $directory $relative
        New-Item -ItemType Directory -Force -Path (Split-Path $path -Parent) | Out-Null
        [IO.File]::WriteAllText($path,'fixture')
    }
    [IO.File]::WriteAllText((Join-Path $directory 'windows/shared-setup.ps1'), 'function Initialize-ElevationSession { }; function Clear-Elevation { }')
    [IO.File]::WriteAllText((Join-Path $directory 'windows/configure-firewall.ps1'), 'param([string]$AircPath) Add-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Value (''firewall|'' + $AircPath); if ($env:AIRC_FIXTURE_FIREWALL_FAIL) { exit 73 }; exit 0')
}
try {
    $gitRoot = Join-Path $fixture 'git'
    $entry = Join-Path $fixture 'entry'
    New-Item -ItemType Directory -Force -Path (Join-Path $gitRoot 'cmd'),(Join-Path $gitRoot 'bin'),(Join-Path $gitRoot 'exec'),$entry | Out-Null
    Copy-Item (Join-Path $repository 'install.ps1') (Join-Path $entry 'install.ps1')
    # Exercise Windows checkout line endings even when the author uses LF.
    $fixtureEntry = Join-Path $entry 'install.ps1'
    [IO.File]::WriteAllText($fixtureEntry, [IO.File]::ReadAllText($fixtureEntry).Replace("`r`n", "`n").Replace("`n", "`r`n"))
    $env:USERPROFILE = Join-Path $fixture 'profile'
    $env:LOCALAPPDATA = Join-Path $fixture 'local'
    $env:AIRC_DIR = $null
    $env:AIRC_CHANNEL = $null
    $env:BIN_DIR = $null
    $env:BIN_TARGET = $null
    $env:AIRC_FIXTURE_LOG = Join-Path $fixture 'calls.txt'
    $env:AIRC_FIXTURE_GIT_EXEC = Join-Path $gitRoot 'exec'
    $env:AIRC_FIXTURE_SOURCE = Join-Path $fixture 'source template'
    New-Source $env:AIRC_FIXTURE_SOURCE
    $fakeGit = Join-Path $gitRoot 'cmd/git.exe'
    [IO.File]::WriteAllText((Join-Path $gitRoot 'exec/git-remote-https.exe'),'fixture')
    # A tiny process fixture checks actual Windows argv/path/env handoff. It is
    # deliberately not a shell and cannot execute installer or package commands.
    Add-Type -OutputAssembly (Join-Path $gitRoot 'bin/bash.exe') -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
public static class SetupBridgeFixture {
  [DllImport("kernel32.dll")] static extern IntPtr GetConsoleWindow();
  static void CopyTree(string source, string target) {
    Directory.CreateDirectory(target);
    foreach (var file in Directory.GetFiles(source)) File.Copy(file, Path.Combine(target, Path.GetFileName(file)));
    foreach (var directory in Directory.GetDirectories(source)) CopyTree(directory, Path.Combine(target, Path.GetFileName(directory)));
  }
  public static void Main(string[] args) {
    if (GetConsoleWindow()!=IntPtr.Zero) { Environment.Exit(91); }
    if (Array.IndexOf(args,"diagnostic-fixture")>=0) {
      Console.Out.WriteLine("FIXTURE DATA"); Console.Error.WriteLine("FIXTURE COMPILER ERROR"); Environment.Exit(23);
    }
    if (args.Length>0 && args[0]=="clone") {
      File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_FIXTURE_LOG"),"git|"+string.Join("|",args)+"\n");
      CopyTree(Environment.GetEnvironmentVariable("AIRC_FIXTURE_SOURCE"),args[args.Length-1]); return;
    }
    if (args.Length>0 && args[0]=="install") {
      if (Array.IndexOf(args,"user")<0) Environment.Exit(92);
      File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_FIXTURE_LOG"),"winget\n");
      File.WriteAllText(Path.Combine(Environment.GetEnvironmentVariable("AIRC_FIXTURE_GIT_EXEC"),"git-remote-https.exe"),"fixture"); return;
    }
    if (args.Length == 1 && args[0] == "--exec-path") {
      Console.WriteLine(Environment.GetEnvironmentVariable("AIRC_FIXTURE_GIT_EXEC"));
      return;
    }
    File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_FIXTURE_LOG"),
      "bash|" + string.Join("|", args) + "|source=" + Environment.GetEnvironmentVariable("AIRC_DIR") + "|bin=" + Environment.GetEnvironmentVariable("BIN_DIR") + "|target=" + Environment.GetEnvironmentVariable("BIN_TARGET") + "\n");
  }
}
'@
    # A script setting global:LASTEXITCODE cannot emulate a native command when
    # an enclosing scope has its own value (e.g. the deliberate exit-73 case).
    Copy-Item -LiteralPath (Join-Path $gitRoot 'bin/bash.exe') -Destination $fakeGit
    $fakeWinget = Join-Path $gitRoot 'cmd/winget.exe'
    Copy-Item -LiteralPath $fakeGit -Destination $fakeWinget
    # Seed only this test's isolated, checksum-verified setup cache. This loads
    # the real shared launcher; native fixture programs cannot install anything.
    . (Join-Path $repository 'windows/shared-setup.ps1')
    # Real nested native coordinator, with only its Bash executable replaced.
    # Capture raw OS pipes: merging errors inside the child would hide the PS5 bug.
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $start.Arguments = '-NoProfile -ExecutionPolicy RemoteSigned -File "' + (Join-Path $repository 'windows/install-session.ps1') + '" -SourceDirectory "' + $repository + '" -BashPath "' + (Join-Path $gitRoot 'bin/bash.exe') + '" -ExpectedBuild diagnostic-fixture'
    $start.UseShellExecute=$false; $start.CreateNoWindow=$true
    $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
    $native = [Diagnostics.Process]::Start($start)
    try {
        $out = $native.StandardOutput.ReadToEndAsync(); $err = $native.StandardError.ReadToEndAsync()
        if (-not $native.WaitForExit(30000)) { $native.Kill(); throw 'Nested coordinator diagnostic test timed out.' }
        Assert-True ($native.ExitCode -eq 23) "Nested coordinator lost native failure: $($native.ExitCode); $($err.Result)"
        Assert-True ($out.Result.Trim() -eq 'FIXTURE DATA') "Nested coordinator contaminated data: $($out.Result)"
        Assert-True ([regex]::Matches($err.Result,'(?m)^FIXTURE COMPILER ERROR\s*$').Count -eq 1) "Missing or duplicated nested diagnostic: $($err.Result)"
    } finally { $native.Dispose() }
    Write-Host 'PASS: actual nested PS5 coordinator preserves diagnostic, data and exit status'
    function Get-Command {
        param([string]$Name, $ErrorAction, $CommandType)
        if ($Name -in @('git.exe','git')) { return [pscustomobject]@{Source=$fakeGit} }
        if ($Name -eq 'winget') { return [pscustomobject]@{Source=$fakeWinget} }
        Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
    }
    function git {
        Add-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Value ('git|' + ($args -join '|'))
        if ($args[0] -ne 'clone') { throw 'Unexpected Git mutation in bridge test' }
        New-Source $args[-1]
        $global:LASTEXITCODE = 0
    }
    function winget {
        if (($args -join ' ') -notmatch '--scope user') { throw 'Bootstrap Git acquisition must use user scope' }
        Add-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Value 'winget'
        [IO.File]::WriteAllText((Join-Path $gitRoot 'exec/git-remote-https.exe'),'fixture')
        $global:LASTEXITCODE = 0
    }

    # Native processes update the global automatic status. A caller-local value
    # must neither invent failure nor hide a subsequent real child failure.
    $LASTEXITCODE = 73
    # Repeated setup must not grow a long inherited PATH past Windows' limit.
    $env:PATH = 'C:\airc PATH fixture;' * 1200
    & (Join-Path $entry 'install.ps1')
    $calls = Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw
    Assert-True ($calls -match '\|bin=\|target=\r?\n') 'Native entry overrode shared install destination selection'
    Assert-True ($calls -match 'git\|clone\|--quiet\|--branch\|canary\|') 'Fresh public source did not use matching channel'
    Assert-True ($calls -match "O'Brien") 'Path containing spaces/apostrophe was lost'
    Assert-True ($calls -match 'bash\|--noprofile\|--norc\|') 'Shared coordinator was not invoked'
    Assert-True ($calls -notmatch 'winget') 'Working Git was unnecessarily reinstalled'

    $installedBinary = Join-Path $fixture "installed airc O'Brien.exe"
    [IO.File]::WriteAllText($installedBinary,'fixture')
    [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
    & (Join-Path $entry 'install.ps1') -FirewallOnly -AircPath $installedBinary
    $calls = Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw
    Assert-True ($calls.Trim() -eq ('firewall|' + $installedBinary)) 'Firewall-only public entry rebuilt or lost the installed path'
    # A fresh PS5 public-entry process must choose its own Utility module even
    # when the desktop host passes a foreign module path. No caller cleanup.
    $foreign = Join-Path $fixture 'foreign-modules'
    $utility = Join-Path $foreign 'Microsoft.PowerShell.Utility'
    New-Item -ItemType Directory -Path $utility -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $utility 'Microsoft.PowerShell.Utility.psm1'), 'function ConvertFrom-Json { throw "FOREIGN UTILITY MODULE" }; Export-ModuleMember -Function ConvertFrom-Json')
    [IO.File]::WriteAllText((Join-Path $utility 'Microsoft.PowerShell.Utility.psd1'), "@{RootModule='Microsoft.PowerShell.Utility.psm1';ModuleVersion='99.0';FunctionsToExport=@('ConvertFrom-Json')}")
    $modulePathBefore = $env:PSModulePath; $pathBefore = $env:PATH; $sourceBefore = $env:AIRC_DIR
    $controlEntry = Join-Path $entry 'control.ps1'
    $entryText = [IO.File]::ReadAllText((Join-Path $entry 'install.ps1')).Replace("`r`n", "`n")
    $first = $entryText.IndexOf('# BEGIN GENERATED RUNTIME MODULES')
    $last = $entryText.IndexOf('# END GENERATED RUNTIME MODULES')
    Assert-True ($first -ge 0 -and $last -gt $first) 'Public runtime initialization region missing'
    $controlText = $entryText.Replace("`nInitialize-InstallerPowerShell`n", "`n")
    Assert-True ($controlText -cne $entryText) 'Negative control did not remove runtime initialization'
    [IO.File]::WriteAllText($controlEntry, $controlText)
    $probe = Join-Path $fixture 'public-module-probe.ps1'
    @'
param($Entry, $Binary, $ForeignModules)
[Console]::WriteLine('module probe entered')
# Windows PowerShell may reorder inherited module paths during startup; establish
# the deliberately incompatible fixture before either public entry executes.
$env:PSModulePath = $ForeignModules + ';' + $env:PSModulePath
# ConvertFrom-Json is the first module-provided command in the generated loader.
$before = $env:PSModulePath
try {
    & $Entry -FirewallOnly -AircPath $Binary
    if ($global:LASTEXITCODE -ne 0) { exit $global:LASTEXITCODE }; if ($env:PSModulePath -cne $before) { throw 'Public entry rewrote inherited module paths.' }
} catch { [Console]::Error.WriteLine($_.Exception.ToString()); exit 1 }
'@ | Set-Content -LiteralPath $probe -Encoding UTF8
    try {
        $env:PSModulePath = $foreign + ';' + $env:PSModulePath
        $env:PATH = (Join-Path $gitRoot 'cmd') + ';' + $saved['PATH']
        $env:AIRC_DIR = $env:AIRC_FIXTURE_SOURCE
        [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
        $control = @(Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" @('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',$probe,$controlEntry,$installedBinary,$foreign) 2>&1)
        Assert-True ($global:LASTEXITCODE -ne 0 -and ($control -join "`n") -match 'FOREIGN UTILITY MODULE') "Unfixed public-entry control did not select the hostile module (exit $global:LASTEXITCODE): $($control -join [Environment]::NewLine)"
        Invoke-InstallerProcess -OwnProcessTree "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" @('-NoProfile','-ExecutionPolicy','RemoteSigned','-File',$probe,(Join-Path $entry 'install.ps1'),$installedBinary,$foreign)
        Assert-True ($global:LASTEXITCODE -eq 0) 'Fresh PS5 public entry selected an incompatible inherited module'
        Assert-True ((Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw).Trim() -eq ('firewall|' + $installedBinary)) 'Foreign-module regression did not reach the public firewall boundary'
    } finally { $env:PSModulePath = $modulePathBefore; $env:PATH = $pathBefore; $env:AIRC_DIR = $sourceBefore }
    Write-Host 'PASS: fresh PS5 public entry retains foreign module paths and selects native built-ins'
    $env:AIRC_FIXTURE_FIREWALL_FAIL = '1'
    $LASTEXITCODE = 0
    try {
        $rejected = $false
        $rejected = (Get-FixtureEntryFailure { & (Join-Path $entry 'install.ps1') -FirewallOnly -AircPath $installedBinary }) -match 'exit 73'
        Assert-True $rejected 'Firewall-only public entry hid failed policy verification'
    } finally { $env:AIRC_FIXTURE_FIREWALL_FAIL = $null; $LASTEXITCODE = 73 }

    # Simulate an older installed source. It must never run its old coordinator.
    Remove-Item -LiteralPath (Join-Path $env:USERPROFILE '.airc/src/windows/install-session.ps1')
    [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
    & (Join-Path $entry 'install.ps1')
    $calls = Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw
    Assert-True ($calls -match 'setup-source-') 'Old default source bypassed compatible acquisition'
    Assert-True (Test-Path (Join-Path $env:USERPROFILE '.airc/src/Cargo.toml')) 'Old checkout was destroyed'

    $fallback = Get-ChildItem (Join-Path $env:USERPROFILE '.airc') -Directory -Filter 'setup-source-*' | Select-Object -First 1
    Remove-Item -LiteralPath (Join-Path $fallback.FullName 'windows/install-session.ps1')
    [IO.File]::WriteAllText((Join-Path $fallback.FullName 'local-work.txt'),'preserve me')
    [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
    & (Join-Path $entry 'install.ps1')
    $calls = Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw
    Assert-True ($calls -match 'git\|clone' -and $calls -match 'setup-source-[a-f0-9]+-1') 'Stale managed fallback was not replaced by compatible acquisition'
    Assert-True ((Get-Content (Join-Path $fallback.FullName 'local-work.txt')) -eq 'preserve me') 'Managed fallback user work was changed'

    # Explicit developer sources are never silently replaced or switched.
    $env:AIRC_DIR = Join-Path $env:USERPROFILE '.airc/src'
    [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
    $rejected = $false
    $refusalDetail = 'Installer returned successfully'
    $refusalDetail = Get-FixtureEntryFailure { & (Join-Path $entry 'install.ps1') }
    $rejected = $refusalDetail -match 'explicitly selected source'
    Assert-True $rejected ("Incompatible developer source was not rejected: $refusalDetail")
    Assert-True (-not (Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw)) 'Rejected developer source launched a process'

    # Git present without HTTPS transport is repaired through the same entry.
    $env:AIRC_DIR = $null
    Remove-Item -LiteralPath (Join-Path $gitRoot 'exec/git-remote-https.exe')
    & (Join-Path $entry 'install.ps1')
    Assert-True ((Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw) -match 'winget') 'Broken Git transport bypassed acquisition'

    # Exercise the actual pinned loader with controlled artifact bytes. Cached
    # or downloaded content must match before it can be dot-sourced.
    & {
        $artifactSource = Join-Path $fixture 'artifact source'
        New-Item -ItemType Directory -Path $artifactSource | Out-Null
        Copy-Item -LiteralPath (Join-Path $repository 'windows/shared-setup.ps1') -Destination $artifactSource
        $manifestText = '$script:ContinuumManifest = @{gsudo=@{source=@{type=''winget'';id=''fixture-package'';scope=''user''}}}'
        $helperText = 'param($GsudoSource) if ($GsudoSource.id -ne ''fixture-package'') { throw ''Manifest descriptor missing'' }; function Initialize-ElevationSession { }; function Clear-Elevation { }'
        $utf8 = New-Object Text.UTF8Encoding($false)
        $hasher = [Security.Cryptography.SHA256]::Create()
        try {
            $manifestHash = [BitConverter]::ToString($hasher.ComputeHash($utf8.GetBytes($manifestText))).Replace('-','').ToLowerInvariant()
            $helperHash = [BitConverter]::ToString($hasher.ComputeHash($utf8.GetBytes($helperText))).Replace('-','').ToLowerInvariant()
        } finally { $hasher.Dispose() }
        $revision = 'a' * 40
        @{schemaVersion=1;continuumRevision=$revision;elevationSha256=$helperHash;manifestSha256=$manifestHash} |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $artifactSource 'setup-artifacts.lock.json')
        $downloadState = @{count=0;corrupt=$false}
        $loaderPath = Join-Path $artifactSource 'shared-setup.ps1'
        $tokens=$null; $parseErrors=$null
        $loaderAst = [Management.Automation.Language.Parser]::ParseFile($loaderPath,[ref]$tokens,[ref]$parseErrors)
        $downloadFunction = $loaderAst.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Save-AircSetupArtifact'},$true)
        if ($parseErrors.Count -or -not $downloadFunction) { throw 'Shared artifact download boundary missing.' }
        $downloadFixture = @'
        function Save-AircSetupArtifact {
            param($Uri,$OutFile)
            if ($downloadState.fail) { throw 'fixture: download timed out' }
            if ($Uri -notlike ('https://raw.githubusercontent.com/CambrianTech/continuum/' + $revision + '/*')) { throw 'Artifact URL is not pinned' }
            $downloadState.count++
            $body = if ($downloadState.corrupt) { 'throw "unverified artifact executed"' }
                elseif ($Uri.EndsWith('/manifest.windows.ps1')) { $manifestText } else { $helperText }
            [IO.File]::WriteAllText($OutFile,$body,$utf8)
        }
'@
        [IO.File]::WriteAllText($loaderPath, [IO.File]::ReadAllText($loaderPath).Replace($downloadFunction.Extent.Text,$downloadFixture))
        . (Join-Path $artifactSource 'shared-setup.ps1')
        Assert-True ($downloadState.count -eq 2) 'Fresh loader did not acquire both verified artifacts'
        . (Join-Path $artifactSource 'shared-setup.ps1')
        Assert-True ($downloadState.count -eq 2) 'Verified cached artifacts were downloaded again'
        $cachedManifest = Join-Path $env:LOCALAPPDATA ("airc/setup-artifacts/$revision/manifest.windows.ps1")
        [IO.File]::WriteAllText($cachedManifest,'throw "unverified cache executed"')
        . (Join-Path $artifactSource 'shared-setup.ps1')
        Assert-True ($downloadState.count -eq 3) 'Corrupted cache was reused instead of repaired'
        [IO.File]::WriteAllText($cachedManifest,'throw "unverified cache executed"')
        $downloadState.corrupt = $true
        $rejected = $false
        try { . (Join-Path $artifactSource 'shared-setup.ps1') } catch { $rejected = $_.Exception.Message -match 'checksum mismatch' }
        Assert-True $rejected 'Unverified artifact was executed or silently accepted'
        Assert-True (-not (Get-ChildItem -LiteralPath (Split-Path $cachedManifest) -Filter '*.download')) 'Failed download left staging files'
        $downloadState.fail = $true
        $rejected = $false
        try { . (Join-Path $artifactSource 'shared-setup.ps1') } catch { $rejected = $_.Exception.Message -match 'download timed out' }
        Assert-True $rejected 'Download failure was hidden or treated as a valid cache hit'
        Assert-True (-not (Get-ChildItem -LiteralPath (Split-Path $cachedManifest) -Filter '*.download')) 'Interrupted download left staging files'
        Write-Host 'PASS: immutable artifact acquisition, verified cache reuse/repair, mismatch refusal, shared manifest descriptor'
    }
    Write-Host 'PASS: fresh source, old-source upgrade, developer-tree preservation, broken Git, Windows path handoff'
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    # The only recursive removal is this uniquely-created fixture below TEMP.
    $full = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $full.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped TEMP' }
    if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
}
