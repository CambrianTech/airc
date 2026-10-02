# Pure guard fixtures and actual caller-token proof, stopping before acquisition.
$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
. (Join-Path $root 'windows/setup-entrypoint.ps1')
Initialize-InstallerPowerShell
$entry=[IO.File]::ReadAllText((Join-Path $root 'install.ps1'))
$boundary=$entry.IndexOf('$aircSetupLock =',[StringComparison]::Ordinal)
if($boundary -lt 0){throw 'Missing acquisition boundary'}
$prefix=$entry.Substring(0,$boundary)+"`n[Console]::WriteLine('BEFORE ACQUISITION');`n}"
$tokens=$null;$errors=$null
$ast=[Management.Automation.Language.Parser]::ParseInput($prefix,[ref]$tokens,[ref]$errors)
$predicate=@($ast.FindAll({param($node)$node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Test-IsAdmin'},$true))
if($errors.Count -or $predicate.Count -ne 1){throw 'Projected token predicate must parse exactly once'}
$scratch=Join-Path ([IO.Path]::GetTempPath()) ('airc-entry-token-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    foreach($case in @(@('real',$prefix,[int](Test-IsAdmin),''),@('high',$prefix.Replace($predicate[0].Extent.Text,'function Test-IsAdmin { $true }'),1,''),@('normal',$prefix.Replace($predicate[0].Extent.Text,'function Test-IsAdmin { $false }'),0,''),@('firewall',$prefix.Replace($predicate[0].Extent.Text,'function Test-IsAdmin { $true }'),0,' -FirewallOnly'),@('diagnostic',$prefix.Replace($predicate[0].Extent.Text,'function Test-IsAdmin { $true }'),0,' -DiagnoseDaemon'))) {
        $file=Join-Path $scratch ($case[0]+'.ps1');[IO.File]::WriteAllText($file,$case[1])
        $start=New-Object Diagnostics.ProcessStartInfo
        $start.FileName=Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
        $start.Arguments='-NoProfile -ExecutionPolicy RemoteSigned -File "'+$file+'"'+$case[3]
        $start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
        $child=[Diagnostics.Process]::Start($start)
        try {$stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync();$child.WaitForExit();if($child.ExitCode -ne $case[2]){throw "$($case[0]) status $($child.ExitCode): $($stderr.Result)"};if($case[2] -eq 1 -and ($stdout.Result.Contains('BEFORE ACQUISITION') -or $stderr.Result -notlike '*normal user terminal*')){throw 'Elevated refusal did not happen before acquisition'};if($case[2] -eq 0 -and -not $stdout.Result.Contains('BEFORE ACQUISITION')){throw 'Normal or explicit adapter path did not pass guard'}}finally{$child.Dispose()}
    }
    Write-Host 'PASS: real caller token and synthetic policy cases refuse elevated full setup before acquisition; explicit adapters remain allowed.'
}finally{
    $resolved=[IO.Path]::GetFullPath($scratch)
    if(-not $resolved.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()),[StringComparison]::OrdinalIgnoreCase)){throw 'Scratch cleanup escaped temporary root'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
exit 0
