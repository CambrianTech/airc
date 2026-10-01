# Exercise the real elevation adapter's argument boundary with a harmless helper.
# Start-Process is redirected to a normal child; no firewall/UAC changes occur.
$ErrorActionPreference='Stop'
$repository=Split-Path $PSScriptRoot -Parent
$fixture=Join-Path ([IO.Path]::GetTempPath()) ("airc firewall O'Brien " + [guid]::NewGuid().ToString('N'))
$savedExpected=$env:AIRC_FIXTURE_EXPECTED_PATH
$savedReadDenied=$env:AIRC_FIXTURE_FIREWALL_READ_DENIED
try {
    New-Item -ItemType Directory -Path $fixture | Out-Null
    Copy-Item (Join-Path $repository 'windows/configure-firewall.ps1') (Join-Path $fixture 'configure-firewall.ps1')
    $binary=Join-Path $fixture 'installed airc.exe'
    [IO.File]::WriteAllText($binary,'fixture')
    $env:AIRC_FIXTURE_EXPECTED_PATH=$binary
    [IO.File]::WriteAllText((Join-Path $fixture 'firewall-allow.ps1'), @'
param([string]$AircPath,[switch]$CheckOnly,[string]$LogPath)
if ($AircPath -ne $env:AIRC_FIXTURE_EXPECTED_PATH) { Write-Error 'Argument boundaries changed the binary path'; exit 91 }
$state=Join-Path $PSScriptRoot 'applied'
if ($CheckOnly) {
    if ($env:AIRC_FIXTURE_FIREWALL_READ_DENIED -eq '1') { exit 4 }
    if (Test-Path $state) { exit 0 } else { exit 1 }
}
[IO.File]::WriteAllText($state,$AircPath)
exit 0
'@)
    $testState=@{requests=0;deny=$false}
    function Start-Process {
        param([string]$FilePath,[string]$Verb,[string]$WindowStyle,[string]$ArgumentList,[switch]$PassThru)
        if ($Verb -ne 'RunAs' -or $WindowStyle -ne 'Hidden') { throw 'Incorrect consent process configuration' }
        $testState.requests++
        if ($testState.deny) { throw 'fixture: consent denied' }
        Microsoft.PowerShell.Management\Start-Process -FilePath $FilePath -WindowStyle Hidden -ArgumentList $ArgumentList -PassThru
    }
    & (Join-Path $fixture 'configure-firewall.ps1') -AircPath $binary
    if (-not (Test-Path (Join-Path $fixture 'applied'))) { throw 'Apply child did not receive the literal path' }
    & (Join-Path $fixture 'configure-firewall.ps1') -AircPath $binary
    if ($testState.requests -ne 1) { throw 'Correct existing rule requested consent again' }
    $env:AIRC_FIXTURE_FIREWALL_READ_DENIED='1'
    & (Join-Path $fixture 'configure-firewall.ps1') -AircPath $binary
    if ($testState.requests -ne 2) { throw 'Restricted policy read did not use administrator setup verification' }
    $env:AIRC_FIXTURE_FIREWALL_READ_DENIED=$null
    Remove-Item -LiteralPath (Join-Path $fixture 'applied')
    $testState.deny=$true
    $rejected=$false
    try { & (Join-Path $fixture 'configure-firewall.ps1') -AircPath $binary } catch { $rejected=$_.Exception.Message -match 'fixture: consent denied' }
    if (-not $rejected) { throw 'Consent failure was suppressed or replaced with success' }
    Write-Host 'PASS: literal Windows paths, completed-child verification, no repeated consent, original failure retained'
} finally {
    $env:AIRC_FIXTURE_EXPECTED_PATH=$savedExpected
    $env:AIRC_FIXTURE_FIREWALL_READ_DENIED=$savedReadDenied
    $full=[IO.Path]::GetFullPath($fixture)
    $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $full.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped TEMP' }
    if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
}
