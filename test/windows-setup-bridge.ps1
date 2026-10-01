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
    foreach ($relative in @('Cargo.toml','install.sh','setup/github-auth.sh','windows/install-prereqs.ps1','windows/run-powershell.sh','windows/register-bin-path.ps1','windows/configure-firewall.ps1')) {
        $path = Join-Path $directory $relative
        New-Item -ItemType Directory -Force -Path (Split-Path $path -Parent) | Out-Null
        [IO.File]::WriteAllText($path,'fixture')
    }
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
    $fakeGit = Join-Path $gitRoot 'cmd/git.ps1'
    [IO.File]::WriteAllText($fakeGit,'$global:LASTEXITCODE = 0; Write-Output $env:AIRC_FIXTURE_GIT_EXEC')
    [IO.File]::WriteAllText((Join-Path $gitRoot 'exec/git-remote-https.exe'),'fixture')
    # A tiny process fixture checks actual Windows argv/path/env handoff. It is
    # deliberately not a shell and cannot execute installer or package commands.
    Add-Type -OutputAssembly (Join-Path $gitRoot 'bin/bash.exe') -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
public static class SetupBridgeFixture {
  public static void Main(string[] args) {
    File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_FIXTURE_LOG"),
      "bash|" + string.Join("|", args) + "|source=" + Environment.GetEnvironmentVariable("AIRC_DIR") + "\n");
  }
}
'@
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
        Add-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Value 'winget'
        [IO.File]::WriteAllText((Join-Path $gitRoot 'exec/git-remote-https.exe'),'fixture')
        $global:LASTEXITCODE = 0
    }

    & (Join-Path $entry 'install.ps1')
    $calls = Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw
    Assert-True ($calls -match 'git\|clone\|--quiet\|--branch\|canary\|') 'Fresh public source did not use matching channel'
    Assert-True ($calls -match "O'Brien") 'Path containing spaces/apostrophe was lost'
    Assert-True ($calls -match 'bash\|--noprofile\|--norc\|') 'Shared coordinator was not invoked'
    Assert-True ($calls -notmatch 'winget') 'Working Git was unnecessarily reinstalled'

    # Simulate an older installed source. It must never run its old coordinator.
    Remove-Item -LiteralPath (Join-Path $env:USERPROFILE '.airc/src/setup/github-auth.sh')
    [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
    & (Join-Path $entry 'install.ps1')
    $calls = Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw
    Assert-True ($calls -match 'setup-source-') 'Old default source bypassed compatible acquisition'
    Assert-True (Test-Path (Join-Path $env:USERPROFILE '.airc/src/Cargo.toml')) 'Old checkout was destroyed'

    # Explicit developer sources are never silently replaced or switched.
    $env:AIRC_DIR = Join-Path $env:USERPROFILE '.airc/src'
    [IO.File]::WriteAllText($env:AIRC_FIXTURE_LOG,'')
    $rejected = $false
    try { & (Join-Path $entry 'install.ps1') } catch { $rejected = $_.Exception.Message -match 'explicitly selected source' }
    Assert-True $rejected 'Incompatible developer source was not rejected'
    Assert-True (-not (Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw)) 'Rejected developer source launched a process'

    # Git present without HTTPS transport is repaired through the same entry.
    $env:AIRC_DIR = $null
    Remove-Item -LiteralPath (Join-Path $gitRoot 'exec/git-remote-https.exe')
    & (Join-Path $entry 'install.ps1')
    Assert-True ((Get-Content -LiteralPath $env:AIRC_FIXTURE_LOG -Raw) -match 'winget') 'Broken Git transport bypassed acquisition'
    Write-Host 'PASS: fresh source, old-source upgrade, developer-tree preservation, broken Git, Windows path handoff'
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    # The only recursive removal is this uniquely-created fixture below TEMP.
    $full = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $full.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped TEMP' }
    if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
}
