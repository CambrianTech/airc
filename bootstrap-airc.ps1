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

# 2. pre-flight (live route/process state before join). The rust-rewrite
# `airc doctor` exposes `--health`; the old `--connect` flag no longer
# exists and made this pre-flight hard-fail on every fresh rust install.
# Mirrors bootstrap-airc.sh. Fixed 2026-06-13.
Step 'Pre-flight: airc doctor --health'
& airc doctor --health
if ($LASTEXITCODE -ne 0) {
    FailOut 'Pre-flight failed. Fix the items above, then re-run this script.'
}

# 3. gh auth if needed. Pin -h github.com (matches install.sh / .ps1, skips
# the interactive host picker) and -s gist for the substrate scope. After
# a successful login, wire gh's token into git's credential helper so gist
# fetch/push (the rendezvous hot path) doesn't pop a password prompt.
& gh auth status 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) {
    Step "Authenticating gh (need 'gist' scope for room substrate)"
    & gh auth login -h github.com -s gist
    if ($LASTEXITCODE -ne 0) {
        FailOut 'gh auth failed. Re-run this script after logging in manually.'
    }
}
# Idempotent: wire the credential helper if gh isn't already registered.
$ghHelper = & git config --global --get-all credential.https://github.com.helper 2>$null
# Join to a scalar before -notmatch: --get-all can return multiple helper
# values (array), and PS -notmatch on an array filters rather than returning
# a strict boolean. Worst case without this is a redundant (idempotent)
# setup-git, but the scalar form is correct.
if ((@($ghHelper) -join "`n") -notmatch 'gh auth git-credential') {
    & gh auth setup-git 2>$null
    if ($LASTEXITCODE -eq 0) { OK 'gh token wired into git credential helper' }
}

# 3b. Git author identity. Agents commit + open PRs; a fresh box has no
# global user.name/user.email and the first commit dies with "Author
# identity unknown". Derive from the authenticated gh account when unset;
# never clobber an identity the user already set. Mirrors install.sh.
$gitName  = (& git config --global user.name)  2>$null
$gitEmail = (& git config --global user.email) 2>$null
if (-not $gitName -or -not $gitEmail) {
    $ghLogin = (& gh api user --jq '.login') 2>$null
    $ghName  = (& gh api user --jq '.name // .login') 2>$null
    $ghId    = (& gh api user --jq '.id') 2>$null
    $ghEmail = (& gh api user --jq '.email // empty') 2>$null
    if (-not $ghEmail -and $ghId -and $ghLogin) {
        $ghEmail = "$ghId+$ghLogin@users.noreply.github.com"
    }
    if (-not $gitName -and $ghName) {
        & git config --global user.name $ghName
        OK "git user.name set from gh: $ghName (override: git config --global user.name ...)"
    }
    if (-not $gitEmail -and $ghEmail) {
        & git config --global user.email $ghEmail
        OK "git user.email set from gh: $ghEmail (override: git config --global user.email ...)"
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
