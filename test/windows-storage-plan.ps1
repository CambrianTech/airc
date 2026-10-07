# Exercise the actual installer planner with controlled volume observations.
# No package execution, registry mutation, or actual drive-pressure changes.
$ErrorActionPreference = 'Stop'
$repository = Split-Path $PSScriptRoot -Parent
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('airc-storage-' + [guid]::NewGuid().ToString('N'))
$saved = @{}
foreach ($name in @('CARGO_HOME','RUSTUP_HOME','CARGO_TARGET_DIR','TEMP','TMP')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name)
}
try {
    $tokens=$null; $errorsFound=$null
    $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $repository 'windows/install-prereqs.ps1'),[ref]$tokens,[ref]$errorsFound)
    if ($errorsFound.Count) { throw $errorsFound[0] }
    $planner = $ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Initialize-AircBuildStorage'},$true)
    . ([scriptblock]::Create($planner.Extent.Text))
    function Write-Step($message) { }
    function Write-Warn2($message) { }
    function Test-MsvcToolchain { return $script:msvcInstalled }
    function Get-Command { param($Name,$ErrorAction) if ($Name -eq 'cargo') { return @{ Name='fixture-cargo' } } }
    function Get-AircTargetDirectory {
        param($SourceDirectory)
        if ($script:metadataUnavailable) { throw 'fixture: no default Rust toolchain' }
        return $script:configuredTarget
    }
    function Get-AircDriveInfo([string]$Root) {
        $script:probeCount++
        if ($script:probeCount -eq 1 -or -not (Test-Path (Join-Path $DEFAULT_AIRC_ROOT 'build-storage.json'))) {
            return [pscustomobject]@{Name=$Root;AvailableFreeSpace=$script:systemFree}
        }
        return [pscustomobject]@{Name='fixture-build';AvailableFreeSpace=$script:buildFree}
    }
    function Get-AircFixedVolumes {
        [pscustomobject]@{IsReady=$true;DriveType='Fixed';RootDirectory=[pscustomobject]@{FullName=$fixture};AvailableFreeSpace=$script:buildFree}
    }
    New-Item -ItemType Directory -Path $fixture | Out-Null
    $DEFAULT_AIRC_ROOT = Join-Path $fixture 'state'
    $SourceDirectory = Join-Path $fixture 'source'
    $script:configuredTarget = Join-Path $SourceDirectory 'target'
    # Existing explicit homes avoid touching real persisted user settings.
    $env:CARGO_HOME = Join-Path $fixture 'cargo'
    $env:RUSTUP_HOME = Join-Path $fixture 'rustup'
    $env:CARGO_TARGET_DIR = $null
    $script:probeCount=0
    $script:msvcInstalled=$false; $script:systemFree=3GB; $script:buildFree=25GB
    Initialize-AircBuildStorage
    $selected = (Get-Content (Join-Path $DEFAULT_AIRC_ROOT 'build-storage.json') -Raw | ConvertFrom-Json).root
    if (-not $selected) { throw 'Fresh install did not persist its secondary-volume choice' }
    if ($env:TEMP -ne (Join-Path $selected 'temp')) { throw 'Package temporary directory was not selected by setup' }

    # The first successful install consumed part of its reserve. A repeat must
    # account for completed work rather than demand another fresh-install budget.
    $target = Join-Path $selected 'target/release'
    New-Item -ItemType Directory -Path $target -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $target 'airc.exe'),'fixture-build-output')
    $script:msvcInstalled=$true; $script:systemFree=1500MB; $script:buildFree=15GB
    $script:probeCount=0
    Initialize-AircBuildStorage

    $script:msvcInstalled=$false
    $script:probeCount=0
    $rejected=$false
    try { Initialize-AircBuildStorage } catch { $rejected=$_.Exception.Message -match 'system volume' }
    if (-not $rejected) { throw 'Missing system prerequisite was allowed without its reserve' }
    $script:msvcInstalled=$true; $script:buildFree=1GB
    $script:probeCount=0
    $rejected=$false
    try { Initialize-AircBuildStorage } catch { $rejected=$_.Exception.Message -match 'remaining source-build work' }
    if (-not $rejected) { throw 'Exhausted build volume was allowed' }
    # A first install on a roomy system volume writes no relocation marker.
    # Its successful rerun must recognize the existing build after consumption.
    $DEFAULT_AIRC_ROOT = Join-Path $fixture 'system-only-state'
    $script:systemFree=25GB; $script:msvcInstalled=$false; $script:probeCount=0
    Initialize-AircBuildStorage
    if (Test-Path (Join-Path $DEFAULT_AIRC_ROOT 'build-storage.json')) { throw 'Roomy system drive was unnecessarily relocated' }
    New-Item -ItemType Directory -Path (Join-Path $script:configuredTarget 'release') -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $script:configuredTarget 'release/airc.exe'),'fixture')
    $script:systemFree=8GB; $script:msvcInstalled=$true; $script:probeCount=0
    Initialize-AircBuildStorage
    # Cargo metadata can resolve a configured cache rather than source/target.
    $script:configuredTarget=Join-Path $fixture 'configured-cache'
    New-Item -ItemType Directory -Path (Join-Path $script:configuredTarget 'release') -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $script:configuredTarget 'release/airc.exe'),'fixture')
    $script:probeCount=0
    Initialize-AircBuildStorage
    $script:systemFree=1GB; $script:probeCount=0
    $rejected=$false
    try { Initialize-AircBuildStorage } catch { $rejected=$_.Exception.Message -match 'remaining source-build work' }
    if (-not $rejected) { throw 'Configured cache volume capacity was ignored' }
    # A rustup shim can exist before its default toolchain is provisioned.
    # Early planning must allow repair; final planning must fail closed.
    $script:metadataUnavailable=$true; $script:systemFree=25GB; $script:probeCount=0
    Initialize-AircBuildStorage
    $rejected=$false; $script:probeCount=0
    try { Initialize-AircBuildStorage -RequireCargoMetadata } catch { $rejected=$_.Exception.Message -match 'no default Rust toolchain' }
    if (-not $rejected) { throw 'Post-provision planning ignored unresolved Cargo configuration' }
    $script:metadataUnavailable=$false; $script:probeCount=0
    Initialize-AircBuildStorage -RequireCargoMetadata
    Write-Host 'PASS: fresh storage selection, remaining-work rerun, system-volume and build-volume rejection'
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    $full=[IO.Path]::GetFullPath($fixture)
    $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $full.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped TEMP' }
    if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }
}
