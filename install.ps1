# install.ps1 -- Windows-native installer for airc.
#
# Mirrors install.sh for POSIX (bash) -- same shape, same skills wiring,
# same channel persistence -- but bootstraps the Windows-native PS port
# (airc.ps1) with auto-install of every prereq via winget.
#
# Designed for FIRST-TIME Windows users with NOTHING pre-installed.
# Runs on the default Windows PowerShell 5.1 (ships with Windows 10+).
# Installs:
#   - Git              (clone + update)
#   - Python 3         (envelope crypto, formatter, helpers)
#   - GitHub CLI (gh)  (gist transport -- the room IS a private gist)
#   - jq               (JSON wrangling for the bash scripts)
#
# Post-Phase-3c: no OpenSSH server, no Tailscale, no sshd, no pwsh.
# The substrate is gh + envelope encryption (X25519 + ChaCha20-Poly1305).
# Post-PR #864: airc IS the Rust binary (`target/release/airc.exe`);
# this installer builds it via cargo and drops it on PATH. The old
# bash-shim/airc.ps1 era is gone.
#
# Single command setup, from any PowerShell prompt (incl. the default 5.1):
#
#   iwr https://raw.githubusercontent.com/CambrianTech/airc/canary/install.ps1 | iex
#
# Or clone + run:
#
#   git clone https://github.com/CambrianTech/airc.git $HOME\.airc\src
#   powershell -ExecutionPolicy Bypass -File $HOME\.airc\src\install.ps1
#
# After install: open a NEW shell so PATH refreshes, then `airc join`.

# We deliberately do NOT require -Version 7 -- this script must run from
# the default Windows PowerShell 5.1 (the always-present default).
$ErrorActionPreference = 'Stop'

# Paths. AIRC_DIR controls where the source lives; BIN_TARGET is where
# airc.exe lands (added to user PATH); SKILLS_TARGET is where
# Claude Code looks for slash-command skills. All three honor env-var
# overrides for tests + isolated installs (parity with install.sh).
$DEFAULT_AIRC_ROOT = Join-Path $env:USERPROFILE '.airc'
$DEFAULT_CLONE_DIR = Join-Path $DEFAULT_AIRC_ROOT 'src'
$CLONE_DIR     = if ($env:AIRC_DIR)      { $env:AIRC_DIR }      else { $DEFAULT_CLONE_DIR }
$BIN_TARGET    = if ($env:BIN_TARGET)    { $env:BIN_TARGET }    else { Join-Path $env:USERPROFILE 'AppData\Local\Programs\airc' }
$SKILLS_TARGET = if ($env:SKILLS_TARGET) { $env:SKILLS_TARGET } else { Join-Path $env:USERPROFILE '.claude\skills' }
$REPO_URL      = 'https://github.com/CambrianTech/airc.git'

# Channel persistence: same scheme as install.sh -- $CLONE_DIR/.channel
# holds the user's release-channel preference (main / canary). Honored
# by `airc update`.
$DEFAULT_CHANNEL = if ($env:AIRC_CHANNEL) { $env:AIRC_CHANNEL } else { 'canary' }

function Write-Step($msg)  { Write-Host "  -> $msg" }
function Write-Ok($msg)    { Write-Host "  + $msg" -ForegroundColor Green }
function Write-Warn2($msg) { Write-Host "  ! $msg" -ForegroundColor Yellow }
function Write-Fail($msg)  { Write-Host "  x $msg" -ForegroundColor Red }

# -- Refresh PATH from registry ------------------------------------------
# winget updates the User PATH in the registry but the current session
# inherits the old PATH from when this script started. Without a refresh,
# any tool we just installed won't be found by Get-Command in the same
# session. Pulling Machine + User PATH and re-merging mirrors what a
# brand-new shell would inherit.
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
    $user    = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $env:PATH = "$machine;$user"
}

# -- winget bootstrap ----------------------------------------------------
# winget ships with Windows 10 (1809+) and Windows 11 by default via the
# App Installer package. If a user is on a much older Windows OR has
# stripped App Installer, we can't auto-install -- flag it loud with the
# exact Microsoft Store / GitHub Releases URL to recover.
function Test-WingetAvailable {
    if (Get-Command winget -ErrorAction SilentlyContinue) { return }

    # Issue #95: detect Windows Server — Microsoft Store path is a
    # dead-end there (no Store, no App Installer). Surface chocolatey
    # / scoop fallbacks instead.
    $isServer = $false
    try {
        $os = Get-CimInstance Win32_OperatingSystem -ErrorAction SilentlyContinue
        # ProductType: 1=Workstation, 2=Domain Controller, 3=Server
        if ($os -and ($os.ProductType -eq 2 -or $os.ProductType -eq 3)) {
            $isServer = $true
        }
    } catch { }

    Write-Fail 'winget not found.'
    Write-Host ''
    if ($isServer) {
        Write-Host '  This is Windows Server. The Microsoft Store path does not apply here.'
        Write-Host '  Use chocolatey OR scoop, then re-run this installer:'
        Write-Host ''
        Write-Host '    # chocolatey (recommended for Server):'
        Write-Host '    Set-ExecutionPolicy Bypass -Scope Process -Force'
        Write-Host "    iex ((New-Object System.Net.WebClient).DownloadString('https://chocolatey.org/install.ps1'))"
        Write-Host '    choco install -y python git gh jq'
        Write-Host ''
        Write-Host '    # OR scoop (user-scope, no admin needed):'
        Write-Host "    iwr -useb https://get.scoop.sh | iex"
        Write-Host '    scoop install python git gh jq'
        Write-Host ''
        Write-Host '  After installing python, git, gh, jq manually, re-run this script;'
        Write-Host '  it will detect them and skip winget.'
    } else {
        Write-Host '  winget ships with App Installer (Microsoft Store). Install or update it:'
        Write-Host '    1. Open the Microsoft Store, search "App Installer", click Install/Update'
        Write-Host '       (or: https://www.microsoft.com/store/productId/9NBLGGH4NNS1)'
        Write-Host '    2. Reopen PowerShell and run this installer again.'
        Write-Host ''
        Write-Host '  If the Store is unavailable, install manually from'
        Write-Host '  https://github.com/microsoft/winget-cli/releases (latest .msixbundle).'
    }
    exit 1
}

# -- Install one winget package, idempotent ------------------------------
# Test-Cmd: a callable that returns $true when the package is already
# usable (e.g. {Get-Command python} or a custom probe). Skips winget on
# hits -- saves the 30s+ download/install round trip.
function Install-IfMissing {
    param(
        [string]$Name,        # human label for messages
        [string]$WingetId,    # winget package id (e.g. Python.Python.3.12)
        [scriptblock]$TestCmd # returns truthy when already installed
    )
    if (& $TestCmd) {
        Write-Ok "$Name already installed"
        return
    }
    Write-Step "Installing $Name (winget: $WingetId) ..."
    # --silent: no UI prompts. --accept-*: prevents one-time first-run
    # interactive accepts that would block CI / first-time install.
    # --disable-interactivity: belt-and-suspenders against any winget
    # prompt that would hang a non-interactive bootstrap.
    $wingetArgs = @(
        'install', '--id', $WingetId,
        '--exact',
        '--silent',
        '--accept-package-agreements',
        '--accept-source-agreements',
        '--disable-interactivity'
    )
    & winget @wingetArgs
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
        # -1978335189 (0x8A15002B) = APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE
        # = "already installed, no update needed". Treat as success.
        Write-Warn2 "winget exit $LASTEXITCODE for $Name -- continuing; the post-install probe below decides if we recover."
    }
    Update-SessionPath
    if (& $TestCmd) {
        Write-Ok "$Name installed"
    } else {
        Write-Fail "$Name install completed but probe still fails. PATH may need a fresh shell. Re-run after opening a new PowerShell window."
    }
}

# -- Banner --------------------------------------------------------------
Write-Host ''
Write-Host '  AIRC installer (Windows native)'
Write-Host '  --------------------------------'
Write-Host ''

Test-WingetAvailable

# -- Install prereqs -----------------------------------------------------
# Order matters lightly: git first so we can clone, then python for the
# runtime helpers, then gh + jq for the substrate (gist transport).
# pwsh (PowerShell 7+) is intentionally NOT installed -- airc.ps1 is a
# thin bash shim that runs fine on the built-in PS 5.1, and dropping it
# removes a 30+ second prereq install (and the visible UAC prompt).

Install-IfMissing -Name 'Git for Windows'    -WingetId 'Git.Git'             -TestCmd { Get-Command git -ErrorAction SilentlyContinue }
Install-IfMissing -Name 'Python 3'           -WingetId 'Python.Python.3.12'  -TestCmd {
    # Probe both the launcher (`py -3`) and direct `python`. Either is fine
    # for airc.ps1's Python invocations. Skip the App Execution Alias stub
    # at $env:LOCALAPPDATA\Microsoft\WindowsApps\python.exe which prints
    # "Python was not found; run without arguments to install ..." on call.
    $py = Get-Command python -ErrorAction SilentlyContinue
    if ($py -and $py.Source -notlike '*\WindowsApps\*') { return $true }
    return [bool](Get-Command py -ErrorAction SilentlyContinue)
}
Install-IfMissing -Name 'GitHub CLI (gh)'    -WingetId 'GitHub.cli'          -TestCmd { Get-Command gh -ErrorAction SilentlyContinue }
Install-IfMissing -Name 'jq'                 -WingetId 'jqlang.jq'           -TestCmd { Get-Command jq -ErrorAction SilentlyContinue }


Write-Host ''

# -- Clone or update the airc source -------------------------------------
# Pulls $DEFAULT_CHANNEL on first install. install.sh has a more elaborate
# self-recovery path for non-channel branches; mirror the basic shape here
# (git fetch, ff-pull, surface failures cleanly).
if (Test-Path (Join-Path $CLONE_DIR '.git')) {
    Write-Step "Updating existing checkout at $CLONE_DIR"
    try {
        & git -C $CLONE_DIR fetch --quiet origin
        # If we're on the channel branch, ff-pull. Otherwise, leave the
        # branch alone (user may be on a feature branch deliberately) and
        # just print state.
        $current = (& git -C $CLONE_DIR rev-parse --abbrev-ref HEAD).Trim()
        if ($current -eq $DEFAULT_CHANNEL) {
            & git -C $CLONE_DIR pull --ff-only --quiet
        } else {
            Write-Warn2 "Not on '$DEFAULT_CHANNEL' (currently on '$current') -- skipping pull. Run 'airc update' to switch."
        }
    } catch {
        Write-Warn2 "git pull skipped: $_"
    }
} else {
    Write-Step "Cloning airc source to $CLONE_DIR"
    New-Item -ItemType Directory -Force -Path (Split-Path $CLONE_DIR) | Out-Null
    & git clone --quiet --branch $DEFAULT_CHANNEL $REPO_URL $CLONE_DIR
    if ($LASTEXITCODE -ne 0) {
        # Branch may not exist yet (e.g. user pulled from main) -- fall back
        # to default branch + warn.
        Write-Warn2 "Channel '$DEFAULT_CHANNEL' not found on origin; falling back to default branch."
        & git clone --quiet $REPO_URL $CLONE_DIR
        if ($LASTEXITCODE -ne 0) {
            Write-Fail "git clone failed. Check network + that $REPO_URL is reachable."
            exit 1
        }
    }
}

# Persist channel preference (parity with install.sh's .channel file)
$channelFile = Join-Path $CLONE_DIR '.channel'
if (-not (Test-Path $channelFile) -or (Get-Content $channelFile -Raw -ErrorAction SilentlyContinue).Trim() -ne $DEFAULT_CHANNEL) {
    Set-Content -Path $channelFile -Value $DEFAULT_CHANNEL -NoNewline
}

# -- Build + install the Rust airc binary -------------------------------
# Mirrors install.sh's `_install_airc_binary`: PR #864 demolished the bash
# wrapper, airc IS the Rust binary now. On Windows that's
# `target/release/airc.exe`. The legacy `airc.ps1` / `airc.cmd` shim
# machinery is dead; if a prior install dropped them in BIN_TARGET we
# reap them below so the new binary is the only thing on PATH.
Write-Step "Wiring airc binary into $BIN_TARGET"
New-Item -ItemType Directory -Force -Path $BIN_TARGET | Out-Null

if ($env:AIRC_SKIP_RUST_BUILD -eq '1') {
    Write-Step "AIRC_SKIP_RUST_BUILD=1 -- skipping airc build"
} else {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Fail "cargo is required to build airc. Install Rust (https://rustup.rs), then re-run install.ps1."
        exit 1
    }
    Write-Step "Building Rust CLI: airc"
    Push-Location $CLONE_DIR
    try {
        & cargo build --release -p airc-cli
        if ($LASTEXITCODE -ne 0) {
            Write-Fail "cargo build failed (exit $LASTEXITCODE)"
            exit 1
        }
    } finally {
        Pop-Location
    }

    $builtExe = Join-Path $CLONE_DIR 'target\release\airc.exe'
    if (-not (Test-Path $builtExe)) {
        Write-Fail "airc build completed but binary is missing: $builtExe"
        exit 1
    }
    $dstExe = Join-Path $BIN_TARGET 'airc.exe'
    Copy-Item -Path $builtExe -Destination $dstExe -Force
    Write-Ok "Installed airc: $dstExe"
}

# Reap legacy install-shape leftovers (bash wrapper, airc-core binary,
# Windows trampolines from the wrapper era). Reruns of install.ps1 on a
# machine that previously had the wrapper-era install converge cleanly
# to the redesigned Rust-binary layout. Mirrors install.sh:595-600.
foreach ($stale in @('airc.cmd', 'airc.ps1', 'airc-core.exe', 'airc-core')) {
    $stalePath = Join-Path $BIN_TARGET $stale
    if (Test-Path $stalePath) {
        Remove-Item $stalePath -Force -Recurse -ErrorAction SilentlyContinue
        Write-Ok "Removed legacy install artifact: $stalePath"
    }
}

# Add BIN_TARGET to user PATH (idempotent).
$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
if (-not $userPath) { $userPath = '' }
if ($userPath -notlike "*$BIN_TARGET*") {
    $newPath = if ($userPath.Length -gt 0) { "$userPath;$BIN_TARGET" } else { $BIN_TARGET }
    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    Write-Step "Added $BIN_TARGET to user PATH (open a NEW shell to pick up)"
}

# -- Skills wiring -------------------------------------------------------
# Same as install.sh: each subdir under <repo>/skills becomes a slash
# command in Claude Code. Symlink when possible (so `git pull` updates
# pick up live), else copy. Cleanup list mirrors install.sh:
# old skill names from the IRC rename (#59) self-heal across updates.
$skillsSrc = Join-Path $CLONE_DIR 'skills'
if (Test-Path $skillsSrc) {
    Write-Step "Wiring skills into $SKILLS_TARGET"
    New-Item -ItemType Directory -Force -Path $SKILLS_TARGET | Out-Null

    $oldSkillNames = @('connect', 'send', 'rename', 'disconnect', 'monitor', 'setup', 'uninstall')
    foreach ($old in $oldSkillNames) {
        $oldPath = Join-Path $SKILLS_TARGET $old
        if (Test-Path $oldPath) {
            Remove-Item $oldPath -Force -Recurse -ErrorAction SilentlyContinue
        }
    }

    # foreach (statement) over a materialized array, with explicit
    # locals -- avoids the PS 5.1 ForEach-Object pipeline edge case where
    # an inner cmdlet failure surfaces as a misleading "ForEach-Object :
    # parameter Path is null" error against the outer pipeline.
    $skillDirs = @(Get-ChildItem -Path $skillsSrc -Directory -ErrorAction SilentlyContinue)
    foreach ($skill in $skillDirs) {
        if (-not $skill -or -not $skill.Name -or -not $skill.FullName) { continue }
        $skillName = $skill.Name
        $skillPath = $skill.FullName
        $dst = Join-Path $SKILLS_TARGET $skillName
        if (Test-Path $dst) {
            Remove-Item $dst -Force -Recurse -ErrorAction SilentlyContinue
        }
        $linked = $false
        try {
            New-Item -ItemType SymbolicLink -Path $dst -Target $skillPath -ErrorAction Stop | Out-Null
            $linked = $true
        } catch {
            # Symlink requires Developer Mode or admin on Windows;
            # fall back to a recursive copy. Refresh on next install.
            Copy-Item -Recurse -Path $skillPath -Destination $dst -Force
        }
        Write-Host "    /$skillName"
    }
}

# -- Final guidance ------------------------------------------------------
Write-Host ''
Write-Ok 'airc installed.'
Write-Host ''
Write-Host '  Next:'
Write-Host '    1. Open a NEW PowerShell window (so PATH refreshes)'
Write-Host '    2. Authenticate gh once:    gh auth login -s gist'
Write-Host '    3. Join the mesh:           airc join'
Write-Host ''
Write-Host '  Diagnose anytime:    airc doctor'
Write-Host ''

# Explicit successful exit. External probes (winget, gh, etc.) can leak
# a non-zero $LASTEXITCODE through to the script's natural-end exit even
# when the install fully succeeded. Pin it to 0 so CI doesn't see a
# spurious failure.
$global:LASTEXITCODE = 0
exit 0
