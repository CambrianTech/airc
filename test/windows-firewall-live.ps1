# Real NetSecurity provider acceptance on an elevated disposable Windows runner.
# Exercise the installer helper against a unique harmless executable; never
# alter rules for PowerShell, Git, or the user's installed AIRC.
$ErrorActionPreference = 'Stop'
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'This acceptance test requires an elevated disposable Windows runner.'
}
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('airc firewall live ' + [guid]::NewGuid().ToString('N'))
$binary = Join-Path $fixture 'airc-fixture.exe'
$helper = Join-Path (Split-Path $PSScriptRoot -Parent) 'windows/firewall-allow.ps1'
try {
    New-Item -ItemType Directory -Path $fixture | Out-Null
    Add-Type -TypeDefinition 'public static class FirewallFixture { public static void Main() {} }' -OutputAssembly $binary -OutputType ConsoleApplication
    & powershell -NoProfile -ExecutionPolicy RemoteSigned -File $helper -AircPath $binary
    if ($LASTEXITCODE -ne 0) { throw 'Actual firewall application/verification failed' }
    & powershell -NoProfile -ExecutionPolicy RemoteSigned -File $helper -AircPath $binary -CheckOnly
    if ($LASTEXITCODE -ne 0) { throw 'Actual firewall repeat check failed' }
    Write-Host 'PASS: actual Windows firewall provider accepted and verified installer policy'
} finally {
    Get-NetFirewallApplicationFilter -PolicyStore PersistentStore -ErrorAction Stop |
        Where-Object { $_.Program -eq $binary } |
        Get-NetFirewallRule -PolicyStore PersistentStore -ErrorAction Stop |
        Remove-NetFirewallRule -ErrorAction Stop
    $full = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $full.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped TEMP' }
    if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
}
