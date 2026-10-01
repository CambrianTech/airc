# Real archive/checksum/executable checks from the installer's acquisition stage,
# with local release responses. No network, real PATH writes, or GitHub login.
$ErrorActionPreference='Stop'
$repository=Split-Path $PSScriptRoot -Parent
$fixture=Join-Path ([IO.Path]::GetTempPath()) ('airc-gh-test-' + [guid]::NewGuid().ToString('N'))
$saved=@{}
foreach ($name in @('LOCALAPPDATA','TEMP','AIRC_FIXTURE_GH_LOG')) { $saved[$name]=[Environment]::GetEnvironmentVariable($name) }
try {
    New-Item -ItemType Directory -Force -Path (Join-Path $fixture 'package/bin'),(Join-Path $fixture 'source/windows'),(Join-Path $fixture 'temp') | Out-Null
    $env:LOCALAPPDATA=Join-Path $fixture 'local'
    $env:TEMP=Join-Path $fixture 'temp'
    $env:AIRC_FIXTURE_GH_LOG=Join-Path $fixture 'gh-runs'
    Add-Type -OutputAssembly (Join-Path $fixture 'package/bin/gh.exe') -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
public static class GhAcquisitionFixture {
  public static void Main(string[] args) {
    File.AppendAllText(Environment.GetEnvironmentVariable("AIRC_FIXTURE_GH_LOG"), "verified-executable\n");
    Console.WriteLine("gh fixture");
  }
}
'@
    $script:fixtureArchive=Join-Path $fixture 'release.zip'
    Compress-Archive -Path (Join-Path $fixture 'package/bin') -DestinationPath $script:fixtureArchive
    $arch=if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'amd64' }
    $assetName="gh_1.2.3_windows_$arch.zip"
    $script:fixtureChecksum=Join-Path $fixture 'checksums.txt'
    [IO.File]::WriteAllText($script:fixtureChecksum,((Get-FileHash $script:fixtureArchive -Algorithm SHA256).Hash + '  ' + $assetName))
    [IO.File]::WriteAllText((Join-Path $fixture 'source/windows/register-bin-path.ps1'), 'param([string]$BinDirectory) if (-not (Test-Path (Join-Path $BinDirectory "gh.exe"))) { throw "PATH registration preceded archive extraction" }')
    $tokens=$null; $errorsFound=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile((Join-Path $repository 'windows/install-prereqs.ps1'),[ref]$tokens,[ref]$errorsFound)
    if ($errorsFound.Count) { throw $errorsFound[0] }
    $definition=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Install-GitHubCli'},$true)
    . ([scriptblock]::Create($definition.Extent.Text))
    function Write-Step($message) { }
    function Write-Ok($message) { }
    function Update-SessionPath { }
    $script:existing=$false; $script:corrupt=$false; $script:downloads=0
    function Get-Command {
        param([string]$Name,$ErrorAction)
        if ($Name -eq 'gh') {
            if ($script:existing) { [pscustomobject]@{Source=(Join-Path $env:LOCALAPPDATA 'Programs/GitHub CLI/bin/gh.exe')} }
            return
        }
        Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
    }
    function Invoke-RestMethod {
        param([string]$Uri)
        if ($Uri -ne 'https://api.github.com/repos/cli/cli/releases/latest') { throw 'Unexpected release authority' }
        [pscustomobject]@{assets=@(
            [pscustomobject]@{name=$assetName;browser_download_url='fixture:archive'},
            [pscustomobject]@{name='gh_1.2.3_checksums.txt';browser_download_url='fixture:checksum'}
        )}
    }
    function Invoke-WebRequest {
        param([string]$Uri,[switch]$UseBasicParsing,[string]$OutFile)
        $script:downloads++
        if ($Uri -eq 'fixture:archive') {
            if ($script:corrupt) { [IO.File]::WriteAllText($OutFile,'corrupted bytes') }
            else { Copy-Item -LiteralPath $script:fixtureArchive -Destination $OutFile }
        } elseif ($Uri -eq 'fixture:checksum') { Copy-Item -LiteralPath $script:fixtureChecksum -Destination $OutFile }
        else { throw 'Unexpected release asset' }
    }
    Install-GitHubCli -SourceDirectory (Join-Path $fixture 'source')
    if (-not (Test-Path $env:AIRC_FIXTURE_GH_LOG)) { throw 'Downloaded executable was not validated' }
    if ($script:downloads -ne 2) { throw 'Archive and checksum were not both acquired' }
    $script:existing=$true
    Install-GitHubCli -SourceDirectory (Join-Path $fixture 'source')
    if ($script:downloads -ne 2) { throw 'Rerun unnecessarily downloaded existing usable gh' }
    $script:existing=$false; $script:corrupt=$true
    $executions=@(Get-Content $env:AIRC_FIXTURE_GH_LOG).Count
    $rejected=$false
    try { Install-GitHubCli -SourceDirectory (Join-Path $fixture 'source') } catch { $rejected=$_.Exception.Message -match 'checksum mismatch' }
    if (-not $rejected) { throw 'Corrupted release archive was accepted' }
    if (@(Get-Content $env:AIRC_FIXTURE_GH_LOG).Count -ne $executions) { throw 'Corrupted archive reached executable validation' }
    Write-Host 'PASS: missing-gh acquisition, real checksum/executable validation, reuse, corruption rejection'
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    $full=[IO.Path]::GetFullPath($fixture)
    $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $full.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped TEMP' }
    if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
}
