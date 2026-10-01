# Executes the public native entry with isolated source/package fixtures. Never
# installs packages, edits the real user profile, or changes GitHub credentials.
$ErrorActionPreference = 'Stop'
$repository = Split-Path $PSScriptRoot -Parent
$fixture = Join-Path ([IO.Path]::GetTempPath()) ("airc setup O'Brien " + [guid]::NewGuid().ToString('N'))
$saved = @{}
foreach ($name in @('USERPROFILE','LOCALAPPDATA','PATH','AIRC_DIR','AIRC_CHANNEL','AIRC_FIXTURE_LOG','AIRC_FIXTURE_GIT_EXEC')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name)
}
function Assert-True($condition,$message) { if (-not $condition) { throw $message } }
function New-Source($directory) {
    foreach ($relative in @('Cargo.toml','install.sh','setup/github-auth.sh','windows/install-prereqs.ps1','windows/run-powershell.sh','windows/register-bin-path.ps1','windows/configure-firewall.ps1','windows/setup-artifacts.lock.json','windows/install-session.ps1')) {
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
    $env:USERPROFILE = Join-Path $fixture 'profile'
    $env:LOCALAPPDATA = Join-Path $fixture 'local'
    $env:AIRC_DIR = $null
    $env:AIRC_CHANNEL = $null
    $env:AIRC_FIXTURE_LOG = Join-Path $fixture 'calls.txt'
    $env:AIRC_FIXTURE_GIT_EXEC = Join-Path $gitRoot 'exec'
    $fakeGit = Join-Path $gitRoot 'cmd/git.exe'
    [IO.File]::WriteAllText((Join-Path $gitRoot 'exec/git-remote-https.exe'),'fixture')
    # A tiny process fixture checks actual Windows argv/path/env handoff. It is
    # deliberately not a shell and cannot execute installer or package commands.
    Add-Type -OutputAssembly (Join-Path $gitRoot 'bin/bash.exe') -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
public static class SetupBridgeFixture {
  public static void Main(string[] args) {
    if (args.Length == 1 && args[0] == "--exec-path") {
      Console.WriteLine(Environment.GetEnvironmentVariable("AIRC_FIXTURE_GIT_EXEC"));
      return;
    }
    File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_FIXTURE_LOG"),
      "bash|" + string.Join("|", args) + "|source=" + Environment.GetEnvironmentVariable("AIRC_DIR") + "\n");
  }
}
'@
    # A script setting global:LASTEXITCODE cannot emulate a native command when
    # an enclosing scope has its own value (e.g. the deliberate exit-73 case).
    Copy-Item -LiteralPath (Join-Path $gitRoot 'bin/bash.exe') -Destination $fakeGit
    function Get-Command {
        param([string]$Name, $ErrorAction)
        if ($Name -eq 'git.exe') { return [pscustomobject]@{Source=$fakeGit} }
        if ($Name -eq 'winget') { return [pscustomobject]@{Source='fixture-winget'} }
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
    $env:AIRC_FIXTURE_FIREWALL_FAIL = '1'
    $LASTEXITCODE = 0
    try {
        $rejected = $false
        try { & (Join-Path $entry 'install.ps1') -FirewallOnly -AircPath $installedBinary } catch { $rejected = $_.Exception.Message -match 'exit 73' }
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
    try { & (Join-Path $entry 'install.ps1') } catch {
        $refusalDetail = $_.ToString() + ' at ' + $_.ScriptStackTrace
        $rejected = $_.Exception.Message -match 'explicitly selected source'
    }
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
        function Invoke-WebRequest {
            param($Uri,[switch]$UseBasicParsing,$OutFile)
            if ($Uri -notlike ('https://raw.githubusercontent.com/CambrianTech/continuum/' + $revision + '/*')) { throw 'Artifact URL is not pinned' }
            $downloadState.count++
            $body = if ($downloadState.corrupt) { 'throw "unverified artifact executed"' }
                elseif ($Uri.EndsWith('/manifest.windows.ps1')) { $manifestText } else { $helperText }
            [IO.File]::WriteAllText($OutFile,$body,$utf8)
        }
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
