# Public adoption adapter with synthetic process/elevation boundaries only.
$ErrorActionPreference='Stop'
$scratch=Join-Path ([IO.Path]::GetTempPath()) ('airc-recovery-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    Copy-Item "$PSScriptRoot/../windows/adopt-installed.ps1" $scratch
    @'
function Invoke-InstallerEntryPoint { param([scriptblock]$Action) & $Action }
function Initialize-InstallerPowerShell {}
'@ | Set-Content (Join-Path $scratch 'setup-entrypoint.ps1')
    @'
function Test-IsAdmin { $false }
function Initialize-ElevationSession { $global:AircRecoveryTestEvents.Add('owner') }
function Clear-Elevation { $global:AircRecoveryTestEvents.Add('cleanup') }
function Invoke-InstallerProcess {
 param([switch]$OwnProcessTree,[switch]$PreserveChildrenOnSuccess,[Parameter(Position=0)]$Program,[Parameter(Position=1)]$Arguments)
 if($Arguments[0] -eq 'ipc-endpoint') { $global:LASTEXITCODE=0; return '\\.\pipe\synthetic' }
 if($Arguments -contains '--probe') { $global:AircRecoveryTestEvents.Add('probe'); $global:LASTEXITCODE=$global:AircRecoveryTestProbe; return }
 if($Arguments[0] -eq 'update') { $global:AircRecoveryTestEvents.Add('adopt'); $global:LASTEXITCODE=0; return }
 throw 'Unexpected process action'
}
function Invoke-Elevated {
 param($Reason,$CommandLine)
 if($CommandLine[1] -ne 'setup-recover-elevated-owner' -or $CommandLine -contains '--probe') {throw 'Wrong elevated operation'}
 $global:AircRecoveryTestEvents.Add('verified-stop'); $global:LASTEXITCODE=$global:AircRecoveryTestStop
}
'@ | Set-Content (Join-Path $scratch 'shared-setup.ps1')
    foreach($case in @(@(0,0,'probe,owner,adopt,cleanup'),@(5,0,'probe,owner,verified-stop,adopt,cleanup'),@(5,1,'probe,owner,verified-stop,cleanup'),@(23,0,'probe'))) {
        $global:AircRecoveryTestProbe=$case[0];$global:AircRecoveryTestStop=$case[1];$global:AircRecoveryTestEvents=[Collections.Generic.List[string]]::new()
        $failed=$false
        try { & (Join-Path $scratch 'adopt-installed.ps1') -AircPath 'C:\fixture\airc.exe' } catch { $failed=$true }
        if(($global:AircRecoveryTestEvents -join ',') -ne $case[2]) {throw "Wrong sequence: $($global:AircRecoveryTestEvents -join ',')"}
        if($failed -ne ($case[0] -eq 23 -or $case[1] -eq 1)) {throw 'Failure propagation mismatch'}
    }
    Write-Host 'PASS: public adoption only elevates typed access-denied, refuses failed recovery, adopts normally and cleans once.'
} finally {
    $resolved=[IO.Path]::GetFullPath($scratch)
    if(-not $resolved.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()),[StringComparison]::OrdinalIgnoreCase)){throw 'Scratch cleanup outside temporary root'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
