# bootstrap-airc.ps1 -- cold install + first-time setup + room join in one command
#
# Usage:
#   .\bootstrap-airc.ps1 [mnemonic-or-gist-id]
#   iwr https://raw.githubusercontent.com/CambrianTech/airc/canary/bootstrap-airc.ps1 | iex
#   (with mnemonic: download first, then .\bootstrap-airc.ps1 oregon-uncle-bravo-eleven)
#
# What it does:
#   1. Runs install.ps1 if airc isn't already on PATH (handles prereqs
#      via winget + adds airc to PATH).
#   2. Runs `airc doctor --connect` to verify the env can pair (catches
#      Tailscale-down / gh-missing / network-out before they silently fail).
#   3. Walks gh auth if not already done.
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
    [string]$Mnemonic = ''
)

$ErrorActionPreference = 'Stop'

function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Blue }
function OK($msg)   { Write-Host "  -> $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  ! $msg"  -ForegroundColor Yellow }
function FailOut($msg) { Write-Host "`nERROR: $msg" -ForegroundColor Red; exit 1 }

# 0. PowerShell 7+ check — re-launch under pwsh if running on Windows PS 5.1
# (the default Windows shell). airc.ps1 requires PS 7+; if we don't
# re-launch here, every subsequent `airc` invocation in this script
# fails with version-mismatch errors. Issue #91 (Toby's case 2026-04-25).
if ($PSVersionTable.PSVersion.Major -lt 7) {
    $pwshCandidates = @(
        "$env:ProgramFiles\PowerShell\7\pwsh.exe"
        "${env:ProgramFiles(x86)}\PowerShell\7\pwsh.exe"
        "$env:LOCALAPPDATA\Microsoft\WindowsApps\pwsh.exe"
    )
    $pwshPath = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
    if (-not $pwshPath) {
        foreach ($p in $pwshCandidates) {
            if ($p -and (Test-Path $p)) { $pwshPath = $p; break }
        }
    }
    if (-not $pwshPath) {
        Step 'PowerShell 7+ not found -- installing via winget (airc.ps1 requires it)'
        $winget = Get-Command winget -ErrorAction SilentlyContinue
        if (-not $winget) {
            FailOut 'winget not available. Install PowerShell 7 manually from https://github.com/PowerShell/PowerShell/releases, then re-run this script.'
        }
        & winget install --id Microsoft.PowerShell --silent --accept-source-agreements --accept-package-agreements
        # Re-scan for pwsh
        $pwshPath = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
        if (-not $pwshPath) {
            foreach ($p in $pwshCandidates) {
                if ($p -and (Test-Path $p)) { $pwshPath = $p; break }
            }
        }
        if (-not $pwshPath) {
            FailOut 'PowerShell 7 install completed but pwsh.exe still not found. Restart your shell + re-run this script.'
        }
        OK "Installed: $pwshPath"
    }
    Step "Re-launching under PowerShell 7 ($pwshPath)..."
    $relaunchArgs = @('-NoProfile', '-File', $PSCommandPath)
    if ($Mnemonic) { $relaunchArgs += @('-Mnemonic', $Mnemonic) }
    & $pwshPath @relaunchArgs
    exit $LASTEXITCODE
}

# 1. install if not present
$airc = Get-Command airc -ErrorAction SilentlyContinue
if (-not $airc) {
    Step 'airc not on PATH -- running installer (canary channel)'
    iwr 'https://raw.githubusercontent.com/CambrianTech/airc/canary/install.ps1' -UseBasicParsing | iex
    # Refresh PATH for this session
    $env:PATH = [Environment]::GetEnvironmentVariable('PATH','User') + [IO.Path]::PathSeparator + $env:PATH
    $airc = Get-Command airc -ErrorAction SilentlyContinue
    if (-not $airc) {
        FailOut 'airc still not on PATH after install. Restart your shell and re-run this script.'
    }
    OK "airc installed: $($airc.Source)"
} else {
    OK "airc already on PATH: $($airc.Source)"
}

# 2. pre-flight (catches Tailscale-down, gh-missing, network-out, etc.)
Step 'Pre-flight: airc doctor --connect'
& airc doctor --connect
if ($LASTEXITCODE -ne 0) {
    FailOut 'Pre-flight failed. Fix the items above, then re-run this script.'
}

# 3. gh auth if needed
& gh auth status 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) {
    Step "Authenticating gh (need 'gist' scope for room substrate)"
    & gh auth login -s gist
    if ($LASTEXITCODE -ne 0) {
        FailOut 'gh auth failed. Re-run this script after logging in manually.'
    }
}

# 4. join the room
if ($Mnemonic) {
    Step "Joining room via mnemonic / gist-id: $Mnemonic"
    & airc join $Mnemonic
} else {
    Step 'Joining auto-scoped room (no mnemonic given -- using git remote org or #general)'
    & airc join
}

# Give the pair handshake a moment to settle before identity check.
Start-Sleep -Seconds 1

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
    airc list                       # see all rooms on your gh account
    airc help                       # full command list
'@ | Write-Host
Write-Host ''
