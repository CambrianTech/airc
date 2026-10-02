# Project the checksum-pinned shared loader into the standalone remote entry.
# No Continuum application is required and no second launcher is authored here.
param([switch]$Check)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$lock = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'setup-artifacts.lock.json') -Raw).Trim()
$loader = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'shared-setup.ps1') -Raw).Replace("`r`n", "`n")
$assignment = '$aircSetupLock = Get-Content -LiteralPath (Join-Path $PSScriptRoot ''setup-artifacts.lock.json'') -Raw | ConvertFrom-Json'
if (-not $loader.Contains($assignment)) { throw 'Shared loader lock boundary changed; update the bootstrap projection.' }
$loader = $loader.Replace($assignment, ('$aircSetupLock = @' + "'`n" + $lock.Replace("`r`n", "`n") + "`n'@ | ConvertFrom-Json"))
$begin = '# BEGIN GENERATED SHARED SETUP BOOTSTRAP'
$end = '# END GENERATED SHARED SETUP BOOTSTRAP'
$entry = Join-Path $root 'install.ps1'
$text = [IO.File]::ReadAllText($entry).Replace("`r`n", "`n")
$first = $text.IndexOf($begin, [StringComparison]::Ordinal)
$last = $text.IndexOf($end, [StringComparison]::Ordinal)
if ($first -lt 0 -or $last -le $first -or $text.LastIndexOf($begin) -ne $first -or $text.LastIndexOf($end) -ne $last) { throw 'Expected exactly one bootstrap region.' }
$next = $text.Substring(0, $first) + $begin + "`n" + $loader.TrimEnd() + "`n" + $end + $text.Substring($last + $end.Length)
if ($Check) {
    if ($text -cne $next) { throw 'Bootstrap drift: run windows/sync-bootstrap.ps1 and commit install.ps1.' }
} else { [IO.File]::WriteAllText($entry, $next, (New-Object Text.UTF8Encoding($false))) }
