# bootstrap-airc.ps1 -- cold install + first-time setup + room join in one command
#
# Usage:
#   .\bootstrap-airc.ps1 [mnemonic-or-gist-id]
#   iwr https://raw.githubusercontent.com/CambrianTech/airc/canary/bootstrap-airc.ps1 | iex
#   (with mnemonic: download first, then .\bootstrap-airc.ps1 oregon-uncle-bravo-eleven)
#
# What it does:
#   1. Reconciles install.ps1 on every run (handles prereqs
#      via winget + adds airc to PATH).
#   2. Walks gh auth if not already done, waiting for browser authorization.
#   3. Checks health AFTER join has created the identity and daemon.
#   4. Joins a room: with the mnemonic-or-gist-id argument if given,
#      otherwise auto-scope from the current git repo (or #general).
#   5. Sets a default identity if pronouns are still unset.
#   6. Prints a final whois + next-step hints.
#
# Designed for first-time users (especially first-EXTERNAL users like
# Toby) so the path from "got the SMS with a 4-word phrase" to "in the
# room" is a single command, not seven.
#
# Issue #81. Pairs with bootstrap-airc.sh for Mac/Linux/Git-Bash.

[CmdletBinding()]
param(
    [string]$Mnemonic = '',
    [string]$GitHubUser = ''
)

$ErrorActionPreference = 'Stop'

function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Blue }
function OK($msg)   { Write-Host "  -> $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  ! $msg"  -ForegroundColor Yellow }
function FailOut($msg) { Write-Host "`nERROR: $msg" -ForegroundColor Red; exit 1 }

# AIRC is a native executable; the bootstrap works on stock PowerShell 5.1.
# Refresh newly installed tools without asking users to open another terminal.
function Refresh-Path {
    $env:PATH = (@(
        [Environment]::GetEnvironmentVariable('PATH', 'User'),
        [Environment]::GetEnvironmentVariable('PATH', 'Machine'),
        $env:PATH
    ) | Where-Object { $_ }) -join [IO.Path]::PathSeparator
}
Refresh-Path

# 1. Reconcile installation on every run. An existing executable must not skip
# repair of failed prerequisite, firewall, PATH or integration stages.
$airc = Get-Command airc -ErrorAction SilentlyContinue
& {
    Step 'Running the repeatable AIRC installer'
    $installer = if ($PSScriptRoot) { Join-Path $PSScriptRoot 'install.ps1' } else { $null }
    $downloadedInstaller = $false
    if (-not $installer -or -not (Test-Path -LiteralPath $installer)) {
        $installer = Join-Path ([IO.Path]::GetTempPath()) ('airc-install-' + [guid]::NewGuid().ToString('N') + '.ps1')
        Invoke-WebRequest 'https://raw.githubusercontent.com/CambrianTech/airc/canary/install.ps1' -UseBasicParsing -OutFile $installer
        $downloadedInstaller = $true
    }
    try {
        # install.ps1 intentionally exits. Isolate that exit in a child process
        # so a successful cold install continues to authentication and join.
        $previousExpectedUser = $env:AIRC_GITHUB_USER
        if ($GitHubUser) { $env:AIRC_GITHUB_USER = $GitHubUser }
        & (Get-Process -Id $PID).Path -NoProfile -ExecutionPolicy RemoteSigned -File $installer
        if ($LASTEXITCODE -ne 0) { FailOut "Installation failed (exit $LASTEXITCODE). Re-run this bootstrap to resume." }
    } finally {
        $env:AIRC_GITHUB_USER = $previousExpectedUser
        if ($downloadedInstaller) { Remove-Item -LiteralPath $installer -Force }
    }
    Refresh-Path
    $airc = Get-Command airc -ErrorAction SilentlyContinue
    if (-not $airc) {
        FailOut 'Installer returned success but airc is unavailable. No connection was attempted.'
    }
    OK "airc installed: $($airc.Source)"
}

# Shared GitHub consent stage, also used by install.sh and install.ps1.
$sourceDirectory = $PSScriptRoot
if (-not $sourceDirectory -or -not (Test-Path (Join-Path $sourceDirectory 'setup\github-auth.sh'))) {
    $sourceMarker = Join-Path $env:USERPROFILE '.airc\install-source'
    if (Test-Path -LiteralPath $sourceMarker) { $sourceDirectory = (Get-Content -LiteralPath $sourceMarker -Raw).Trim() }
}
$downloadedSetup = $null
if (-not $sourceDirectory -or -not (Test-Path (Join-Path $sourceDirectory 'windows\invoke-github-auth.ps1')) -or
    -not (Test-Path (Join-Path $sourceDirectory 'setup\github-auth.sh'))) {
    # Existing binaries predate these helpers. Acquire this bootstrap's setup
    # stage automatically; do not switch the user's installed source branch.
    $downloadedSetup = Join-Path ([IO.Path]::GetTempPath()) ('airc-onboarding-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path (Join-Path $downloadedSetup 'setup'),(Join-Path $downloadedSetup 'windows') -Force | Out-Null
    $setupRef = if ($env:AIRC_CHANNEL) { $env:AIRC_CHANNEL } else { 'canary' }
    foreach ($relative in @('setup/github-auth.sh','windows/invoke-github-auth.ps1')) {
        Invoke-WebRequest "https://raw.githubusercontent.com/CambrianTech/airc/$setupRef/$relative" -UseBasicParsing -OutFile (Join-Path $downloadedSetup $relative)
    }
    $sourceDirectory = $downloadedSetup
}
$previousExpectedUser = $env:AIRC_GITHUB_USER
try {
    if ($GitHubUser) { $env:AIRC_GITHUB_USER = $GitHubUser }
    & (Join-Path $sourceDirectory 'windows\invoke-github-auth.ps1') -SourceDirectory $sourceDirectory
} finally {
    $env:AIRC_GITHUB_USER = $previousExpectedUser
    if ($downloadedSetup) {
        Remove-Item -LiteralPath (Join-Path $downloadedSetup 'setup/github-auth.sh'),(Join-Path $downloadedSetup 'windows/invoke-github-auth.ps1') -Force
        Remove-Item -LiteralPath (Join-Path $downloadedSetup 'setup'),(Join-Path $downloadedSetup 'windows'),$downloadedSetup
    }
}
# 4. Provision through the public join command, then validate. The default
# interactive join streams forever; suppress only that feed while bootstrapping.
$previousNoAttach = $env:AIRC_NO_ATTACH
$env:AIRC_NO_ATTACH = '1'
try {
if ($Mnemonic) {
    Step "Joining room via mnemonic / gist-id: $Mnemonic"
    & airc join $Mnemonic
} else {
    Step 'Joining auto-scoped room (no mnemonic given -- using git remote org or #general)'
    & airc join
}
if ($LASTEXITCODE -ne 0) { FailOut 'Mesh join failed. Re-run this bootstrap to repair and resume.' }
} finally { $env:AIRC_NO_ATTACH = $previousNoAttach }

Step 'Verifying the joined mesh: airc doctor --health'
& airc doctor --health
if ($LASTEXITCODE -ne 0) { FailOut 'Mesh health verification failed; bootstrap is not complete.' }

# 5. set default identity if unset
$identityOut = & airc identity show 2>$null
if ($identityOut -match 'pronouns:\s*\(unset\)') {
    Step 'Setting default identity (override later with: airc identity set ...)'
    & airc identity set `
        --pronouns it `
        --role onboarded-via-bootstrap `
        --bio 'Joined via bootstrap-airc.ps1'
}

# 6. final summary
Write-Host ''
OK 'Bootstrap complete. Your airc identity:'
Write-Host ''
$whois = & airc whois 2>&1
foreach ($line in $whois) { Write-Host "    $line" }
Write-Host ''
OK 'Next steps:'
@'
    airc msg "hello room"           # broadcast to your room
    airc msg @<peer> "hi"           # DM a peer
    airc peers                      # list paired peers
    airc whois <peer>               # see another peer's identity
    airc room                       # inspect current room
    airc help                       # full command list
'@ | Write-Host
Write-Host ''
