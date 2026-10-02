# Project the checksum-pinned shared loader into the standalone remote entry.
# No Continuum application is required and no second launcher is authored here.
param([switch]$Check)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$lock = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'setup-artifacts.lock.json') -Raw).Trim()
$loader = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'shared-setup.ps1') -Raw).Replace("`r`n", "`n")
# Reuse the checksum-verified artifact acquisition path, then project its small
# runtime initializer before the loader itself needs any autoloaded cmdlets.
. (Join-Path $PSScriptRoot 'shared-setup.ps1')
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($aircSetupElevation, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Pinned setup helper could not be parsed.' }
$initializers = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -in @('Initialize-InstallerPowerShell', 'Invoke-InstallerEntryPoint')
}, $false))
if ($initializers.Count -ne 2) { throw 'Pinned setup helper must supply the runtime initializer and entry serializer.' }
$runtimeBegin = '# BEGIN GENERATED RUNTIME MODULES'
$runtimeEnd = '# END GENERATED RUNTIME MODULES'
$runtimeStart = $loader.IndexOf($runtimeBegin, [StringComparison]::Ordinal)
$runtimeFinish = $loader.IndexOf($runtimeEnd, [StringComparison]::Ordinal)
if ($runtimeStart -lt 0 -or $runtimeFinish -le $runtimeStart -or $loader.LastIndexOf($runtimeBegin) -ne $runtimeStart -or $loader.LastIndexOf($runtimeEnd) -ne $runtimeFinish) { throw 'Expected one generated runtime module region.' }
$runtime = ($initializers | ForEach-Object { $_.Extent.Text.Replace("`r`n", "`n") }) -join "`n`n"
$entryDefinitions = "# Generated from verified shared setup artifacts by windows/sync-bootstrap.ps1.`n" + $runtime + "`n"
$entryDefinitionsPath = Join-Path $PSScriptRoot 'setup-entrypoint.ps1'
if ($Check) {
    if (-not (Test-Path -LiteralPath $entryDefinitionsPath) -or [IO.File]::ReadAllText($entryDefinitionsPath).Replace("`r`n", "`n") -cne $entryDefinitions) { throw 'Entry serializer drift: regenerate from the pinned shared helper.' }
} else { [IO.File]::WriteAllText($entryDefinitionsPath, $entryDefinitions, (New-Object Text.UTF8Encoding($false))) }
$projected = $loader.Substring(0, $runtimeStart) + $runtimeBegin + "`n" + $runtime + "`nInitialize-InstallerPowerShell`n" + $runtimeEnd + $loader.Substring($runtimeFinish + $runtimeEnd.Length)
if ($Check) {
    if ($loader -cne $projected) { throw 'Runtime module drift: regenerate from the pinned shared helper.' }
} else {
    [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'shared-setup.ps1'), $projected, (New-Object Text.UTF8Encoding($false)))
}
$loader = $projected
# Public entry owns serialization around both artifact acquisition and setup.
# Dot-sourced shared-setup remains a library with ordinary exception semantics.
$loader = $loader.Replace("`nInitialize-InstallerPowerShell`n", "`nInvoke-InstallerEntryPoint {`nInitialize-InstallerPowerShell`n")
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
