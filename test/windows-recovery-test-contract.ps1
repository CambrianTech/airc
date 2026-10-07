# Match the hosted PS5 dot-source/LASTEXITCODE wrapper, including a failing assertion.
$ErrorActionPreference='Stop'
$scratch=Join-Path ([IO.Path]::GetTempPath()) ('airc-recovery-contract-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    $test=Join-Path $PSScriptRoot 'windows-elevated-recovery.ps1'
    $negative=Join-Path $scratch 'negative.ps1'
    $source=[IO.File]::ReadAllText($test)
    $adapter=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../windows/adopt-installed.ps1'))
    $source=$source.Replace('$PSScriptRoot/../windows/adopt-installed.ps1',$adapter)
    $marker="    Write-Host 'PASS: public adoption"
    if(-not $source.Contains($marker)){throw 'Negative-control assertion marker missing'}
    $source=$source.Replace($marker,"    throw 'negative-control assertion'`n"+$marker)
    [IO.File]::WriteAllText($negative,$source)
    foreach($case in @(@($test,0),@($negative,1))) {
        $wrapper=Join-Path $scratch 'wrapper.ps1'
        [IO.File]::WriteAllText($wrapper,('$ErrorActionPreference=''Stop'''+"`ntry { . '"+$case[0].Replace("'","''")+"' } catch { [Console]::Error.WriteLine(`$_.ToString()); exit 1 }`nif (Test-Path variable:LASTEXITCODE) { exit `$LASTEXITCODE }`n"))
        $start=New-Object Diagnostics.ProcessStartInfo
        $start.FileName=Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
        $start.Arguments='-NoProfile -ExecutionPolicy RemoteSigned -File "'+$wrapper+'"'
        $start.UseShellExecute=$false;$start.CreateNoWindow=$true
        $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
        $child=[Diagnostics.Process]::Start($start)
        try {
            $stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
            if(-not $child.WaitForExit(30000)){ $child.Kill();throw 'Owned synthetic test child timed out' }
            if($child.ExitCode -ne $case[1]) {throw "Wrapper exit $($child.ExitCode), expected $($case[1]): $($stdout.Result) $($stderr.Result)"}
            if($case[1] -eq 1 -and $stderr.Result -notlike '*negative-control assertion*'){throw 'Negative control failed for an unrelated reason'}
        } finally { $child.Dispose() }
    }
    Write-Host 'PASS: hosted PS5 wrapper returns zero only for completed assertions; real assertion failure remains nonzero.'
} finally {
    $resolved=[IO.Path]::GetFullPath($scratch)
    if(-not $resolved.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()),[StringComparison]::OrdinalIgnoreCase)){throw 'Scratch cleanup outside temporary root'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
exit 0
