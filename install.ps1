# Native Windows entry. install.sh owns the shared install lifecycle; this
# bridge acquires Git/Bash and a source tree before handing it that lifecycle.
# Runs on stock Windows PowerShell 5.1. No PowerShell 7 or WSL required.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
function Refresh-Path {
    $env:PATH = [Environment]::GetEnvironmentVariable('PATH','User') + ';' +
        [Environment]::GetEnvironmentVariable('PATH','Machine') + ';' + $env:PATH
}
function Find-GitBash {
    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if ($git) {
        $execPath = & $git.Source --exec-path
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $execPath 'git-remote-https.exe'))) { return $null }
        $root = Split-Path (Split-Path $git.Source -Parent) -Parent
        foreach ($relative in @('bin\bash.exe','usr\bin\bash.exe')) {
            $candidate = Join-Path $root $relative
            if (Test-Path -LiteralPath $candidate) { return $candidate }
        }
    }
    return $null
}
Refresh-Path
$bash = Find-GitBash
if (-not $bash) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw 'Git for Windows and winget are unavailable. Install Windows App Installer, then rerun AIRC setup.'
    }
    Write-Host 'Installing Git for Windows. If Windows requests consent, setup waits for you to approve it.'
    & winget install --id Git.Git --source winget --exact --silent --accept-package-agreements --accept-source-agreements --disable-interactivity
    $installExit = $LASTEXITCODE
    Refresh-Path
    $bash = Find-GitBash
    if (-not $bash) { throw "Git for Windows is unavailable after installation (exit $installExit). Rerun AIRC setup to resume." }
}
$managedSource = -not $env:AIRC_DIR -and -not ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml')))
$source = if ($env:AIRC_DIR) { $env:AIRC_DIR } elseif ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml'))) {
    $PSScriptRoot
} else { Join-Path $env:USERPROFILE '.airc\src' }
$channel = if ($env:AIRC_CHANNEL) { $env:AIRC_CHANNEL } else { 'canary' }
function Test-SetupLayout([string]$Directory) {
    foreach ($relative in @('install.sh','setup\github-auth.sh','windows\install-prereqs.ps1','windows\run-powershell.sh','windows\register-bin-path.ps1','windows\configure-firewall.ps1')) {
        if (-not (Test-Path -LiteralPath (Join-Path $Directory $relative))) { return $false }
    }
    return $true
}
if ((Test-Path (Join-Path $source 'Cargo.toml')) -and -not (Test-SetupLayout $source)) {
    if (-not $managedSource) {
        throw 'The explicitly selected source tree has an older setup layout. Update that checkout before invoking this installer; it will not switch or overwrite developer work.'
    }
    # Keep an older installer-owned checkout intact (it may contain user edits).
    # Acquire a compatible channel checkout beside it, using a bounded safe name.
    $hash = [Security.Cryptography.SHA256]::Create()
    try { $suffix = ([BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($channel))) -replace '-','').Substring(0,12).ToLowerInvariant() }
    finally { $hash.Dispose() }
    $source = Join-Path $env:USERPROFILE ('.airc\setup-source-' + $suffix)
    Write-Host "Acquiring compatible $channel setup in $source; preserving the older checkout."
}
if (-not (Test-Path (Join-Path $source 'Cargo.toml'))) {
    $cloneArgs = @('clone','--quiet')
    # The published setup entry is canary; acquire matching helpers rather than
    # accidentally handing new setup to an older remote-default checkout.
    $cloneArgs += @('--branch',$channel)
    $cloneArgs += @('https://github.com/CambrianTech/airc.git',$source)
    & git @cloneArgs
    if ($LASTEXITCODE -ne 0) { throw 'AIRC source acquisition failed. Rerun setup to resume.' }
}
if (-not (Test-SetupLayout $source)) { throw 'Selected source does not contain this setup version. Installation stopped before invoking an incompatible coordinator.' }
$source = (Resolve-Path -LiteralPath $source).Path
# Source update, prerequisites, auth, build, verification and integrations all
# run in the same coordinator used on Linux/macOS and Git Bash.
$names = @('AIRC_DIR','BIN_DIR','AIRC_WINDOWS_NATIVE','PSModulePath')
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
try {
    $env:AIRC_DIR = $source
    if ($env:BIN_TARGET) { $env:BIN_DIR = $env:BIN_TARGET }
    elseif (-not $env:BIN_DIR) { $env:BIN_DIR = Join-Path $env:LOCALAPPDATA 'Programs\airc' }
    $env:AIRC_WINDOWS_NATIVE = '1'
    $env:PSModulePath = $null
    & $bash --noprofile --norc ((Join-Path $source 'install.sh') -replace '\\','/')
    $result = $LASTEXITCODE
} finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    Refresh-Path
}
if ($result -ne 0) { throw "AIRC setup failed (exit $result). Rerun the same setup to resume." }
$global:LASTEXITCODE = 0
