# install.ps1 -- Windows-native installer for airc.
#
# Mirrors install.sh for POSIX (bash) -- same shape, same skills wiring,
# same channel persistence -- but bootstraps the Windows-native PS port
# (airc.ps1) with auto-install of every prereq via winget.
#
# DEV PATH ONLY (zero-friction doctrine, docs/ZERO-FRICTION-PATH.md):
# users get prebuilt signed binaries and never see this script or a
# compiler. This source-build path serves contributors, grid-node
# operators on unreleased branches, and CI; it moves behind --dev once
# the release pipeline lands.
#
# Designed for a FIRST-TIME dev box with NOTHING pre-installed.
# Runs on the default Windows PowerShell 5.1 (ships with Windows 10+).
# Installs:
#   - Git              (clone + update)
#   - GitHub CLI (gh)  (gist transport -- the room IS a private gist)
#   - Rust (rustup)    (airc IS a Rust binary)
#   - VS 2022 Build Tools C++ workload (MSVC linker; machine-scope, so
#     an interactive non-admin session may see ONE OS UAC consent --
#     winget flags suppress winget's own prompts, not UAC. Run elevated
#     or be ready to click consent; non-interactive non-admin fails
#     loudly with recovery guidance.)
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

# Channel = the git branch of the install checkout (see "Clone or update"
# below). git is the state manager; no separate channel file.

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
        Write-Host '    choco install -y git gh rust'
        Write-Host ''
        Write-Host '    # OR scoop (user-scope, no admin needed):'
        Write-Host "    iwr -useb https://get.scoop.sh | iex"
        Write-Host '    scoop install git gh rust'
        Write-Host ''
        Write-Host '  After installing git, gh, rust manually, re-run this script;'
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
# usable (e.g. {Get-Command cargo} or a custom probe). Skips winget on
# hits -- saves the 30s+ download/install round trip.
function Install-IfMissing {
    param(
        [string]$Name,        # human label for messages
        [string]$WingetId,    # winget package id (e.g. Rustlang.Rustup)
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
# Order matters lightly: git first so we can clone, then gh for the
# substrate (gist transport), then the Rust toolchain to build airc.
# pwsh (PowerShell 7+) is intentionally NOT installed -- airc.ps1 is a
# thin bash shim that runs fine on the built-in PS 5.1, and dropping it
# removes a 30+ second prereq install (and the visible UAC prompt).

Install-IfMissing -Name 'Git for Windows'    -WingetId 'Git.Git'             -TestCmd { Get-Command git -ErrorAction SilentlyContinue }
Install-IfMissing -Name 'GitHub CLI (gh)'    -WingetId 'GitHub.cli'          -TestCmd { Get-Command gh -ErrorAction SilentlyContinue }
# git + gh are the only non-Rust prereqs (plus the Rust toolchain below).
# Identity, signing, hooks, config, and JSON handling are all Rust-owned.

# -- Rust toolchain ------------------------------------------------------
# airc IS a Rust binary; cargo is a hard prereq. install.sh auto-installs
# Rustlang.Rustup on the winget path -- mirror that here instead of the
# old hard-exit "go install Rust yourself". winget's Rustup package runs
# rustup-init, which installs the stable-msvc toolchain and adds
# %USERPROFILE%\.cargo\bin to the User PATH; Update-SessionPath inside
# Install-IfMissing makes it visible to THIS session.
Install-IfMissing -Name 'Rust (rustup)'      -WingetId 'Rustlang.Rustup'     -TestCmd { Get-Command cargo -ErrorAction SilentlyContinue }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    # rustup installed but cargo not resolving: a fresh rustup-init may
    # not have set a default toolchain (non-interactive install). Fix it
    # directly rather than telling the user to open a new shell and guess.
    $rustupExe = Join-Path $env:USERPROFILE '.cargo\bin\rustup.exe'
    if (Test-Path $rustupExe) {
        & $rustupExe default stable
        Update-SessionPath
    }
}

# -- MSVC C++ build tools ------------------------------------------------
# Validated live on a fresh Windows 11 box (2026-06-10): rustup's default
# x86_64-pc-windows-msvc target CANNOT LINK without the Visual Studio C++
# build tools -- `cargo build` dies with a wall of per-crate
# `error: linking with link.exe failed: exit code: 1` and no guidance.
# The windows-gnu toolchain is NOT a viable fallback for airc: windows-sys
# raw-dylib import libs trip the upstream bundled-dlltool bug
# (rust-lang/rust#103939) and `ring` needs a real C compiler regardless.
# So: probe for the VC.Tools component via vswhere and auto-install the
# (license-free) VS 2022 Build Tools with the C++ workload when absent.
function Test-MsvcToolchain {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }
    $vsPath = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    return [bool]$vsPath
}

if (Test-MsvcToolchain) {
    Write-Ok 'MSVC C++ build tools already installed'
} else {
    Write-Step 'Installing Visual Studio 2022 Build Tools + C++ workload (required to link Rust on Windows; ~2 GB, several minutes) ...'
    $btArgs = @(
        'install', '--id', 'Microsoft.VisualStudio.2022.BuildTools',
        '--exact', '--silent',
        '--accept-package-agreements', '--accept-source-agreements',
        '--disable-interactivity',
        '--override', '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    )
    & winget @btArgs
    if (Test-MsvcToolchain) {
        Write-Ok 'MSVC C++ build tools installed'
    } else {
        # Most likely: VS/BuildTools product exists but lacks the C++
        # workload, and winget won't modify an existing install. Send the
        # user to the one place that fixes it instead of letting cargo
        # produce the inscrutable link.exe wall.
        Write-Fail 'MSVC C++ build tools still missing after install attempt.'
        Write-Host '  Open "Visual Studio Installer" -> Modify -> check "Desktop development with C++" -> Install.'
        Write-Host '  Then re-run this script. (Without it, cargo cannot link on Windows.)'
        exit 1
    }
}


Write-Host ''

# -- Clone or update the airc source -------------------------------------
# Channel = the checkout's CURRENT BRANCH. git is the state manager: a
# canary user is simply on the `canary` branch, switching channels is
# `git checkout <branch>`, and there is no hardcoded default or `.channel`
# file. Parity with install.sh.
if (Test-Path (Join-Path $CLONE_DIR '.git')) {
    Write-Step "Updating existing checkout at $CLONE_DIR"
    # Fast-forward whatever branch is checked out; never switch branches.
    try {
        $current = (& git -C $CLONE_DIR rev-parse --abbrev-ref HEAD).Trim()
        if ([string]::IsNullOrEmpty($current) -or $current -eq 'HEAD') {
            Write-Warn2 "$CLONE_DIR is in detached HEAD -- check out a channel branch, then re-run."
        } else {
            Write-Step "Channel = current branch '$current'"
            & git -C $CLONE_DIR fetch --quiet origin $current
            & git -C $CLONE_DIR pull --ff-only --quiet
        }
    } catch {
        Write-Warn2 "git pull skipped: $_"
    }
} else {
    New-Item -ItemType Directory -Force -Path (Split-Path $CLONE_DIR) | Out-Null
    # AIRC_CHANNEL=<branch> clones that branch (any branch, e.g. canary);
    # unset → clone the remote's DEFAULT branch (git picks it).
    if ($env:AIRC_CHANNEL) {
        Write-Step "Cloning airc ($env:AIRC_CHANNEL) to $CLONE_DIR"
        & git clone --quiet --branch $env:AIRC_CHANNEL $REPO_URL $CLONE_DIR
        if ($LASTEXITCODE -ne 0) {
            Write-Fail "Couldn't clone branch '$env:AIRC_CHANNEL' from $REPO_URL. Check the branch name + network."
            exit 1
        }
    } else {
        Write-Step "Cloning airc (remote default branch) to $CLONE_DIR"
        & git clone --quiet $REPO_URL $CLONE_DIR
        if ($LASTEXITCODE -ne 0) {
            Write-Fail "git clone failed. Check network + that $REPO_URL is reachable."
            exit 1
        }
    }
}

# Record the install source so `airc update` can find it even when it's a
# dev checkout outside ~/.airc/src (parity with install.sh; the Rust
# update command reads ~/.airc/install-source). $CLONE_DIR is already a
# native Windows path here, which is what the native binary needs.
$aircHome = Join-Path $env:USERPROFILE '.airc'
New-Item -ItemType Directory -Force -Path $aircHome | Out-Null
Set-Content -Path (Join-Path $aircHome 'install-source') -Value $CLONE_DIR
Write-Ok "Recorded install source for 'airc update': $CLONE_DIR"

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

# -- Git fetch-before-commit/push staleness guard (card 64621946) --------
# Parity with install.sh's _install_airc_git_hooks. The hooks themselves
# are bash and run under git-bash on Windows, so the cleanest Windows path
# is to drive the SAME composition logic through bash rather than maintain
# a divergent PowerShell reimplementation. We invoke git-bash to run the
# composer that writes the wrapper hooks, composing (not clobbering) any
# pre-existing hooks — identical contract to the .sh side.
if ($env:AIRC_SKIP_GIT_HOOKS -ne '1') {
    $gitHooksWorker = Join-Path $CLONE_DIR 'integrations\git-hooks\airc-fetch-base.sh'
    $bashExe = $null
    foreach ($cand in @(
        (Get-Command bash.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source),
        'C:\Program Files\Git\bin\bash.exe',
        'C:\Program Files\Git\usr\bin\bash.exe'
    )) {
        if ($cand -and (Test-Path $cand)) { $bashExe = $cand; break }
    }
    if (-not (Test-Path $gitHooksWorker)) {
        Write-Warn2 "Git hook worker not found: $gitHooksWorker (skipping hook install)"
    } elseif (-not $bashExe) {
        Write-Warn2 'git-bash not found; skipping git fetch-before-commit/push hook install'
    } else {
        Write-Step 'Wiring git fetch-before-commit/push staleness guard (card 64621946)'
        $cloneForBash = $CLONE_DIR -replace '\\','/'
        $composer = @'
set -u
CLONE_DIR="$1"
hooks_dir="$CLONE_DIR/.git/hooks"
worker="$CLONE_DIR/integrations/git-hooks/airc-fetch-base.sh"
hp="$(git -C "$CLONE_DIR" config --get core.hooksPath 2>/dev/null || true)"
if [ -n "$hp" ]; then
  case "$hp" in
    /*|[A-Za-z]:*) hooks_dir="$hp" ;;
    *) hooks_dir="$CLONE_DIR/$hp" ;;
  esac
fi
[ -d "$CLONE_DIR/.git" ] || exit 0
[ -f "$worker" ] || exit 0
mkdir -p "$hooks_dir" || exit 0
chmod +x "$worker" 2>/dev/null || true
marker="# AIRC-FETCH-HOOK"
for phase in pre-commit pre-push; do
  hook="$hooks_dir/$phase"
  if [ -f "$hook" ] && ! grep -qF "$marker" "$hook" 2>/dev/null; then
    if [ ! -f "$hook.local" ]; then
      mv "$hook" "$hook.local"
      chmod +x "$hook.local" 2>/dev/null || true
    else
      rm -f "$hook"
    fi
  fi
  tmp="$hook.airc-tmp.$$"
  {
    printf '%s\n' "#!/usr/bin/env bash"
    printf '%s %s\n' "$marker" "— managed by airc install.ps1 (card 64621946); do not edit."
    printf '%s\n' "set -u"
    printf 'WORKER=%q\n' "$worker"
    printf 'LOCAL="${BASH_SOURCE[0]}.local"\n'
    printf 'if [ -x "$WORKER" ] || [ -f "$WORKER" ]; then\n'
    printf '  bash "$WORKER" %q "$@" || exit $?\n' "$phase"
    printf 'fi\n'
    printf 'if [ -x "$LOCAL" ]; then exec "$LOCAL" "$@"; fi\n'
    printf 'exit 0\n'
  } > "$tmp"
  mv "$tmp" "$hook"
  chmod +x "$hook" 2>/dev/null || true
  echo "  + Git hook installed: $phase"
done
'@
        # PS 5.1's native-arg passing shreds a multi-line string handed to
        # `bash -c` (embedded newlines / `()` get mangled, so bash receives
        # a truncated script → "unexpected EOF while looking for matching
        # `)'"), which broke fresh installs on STOCK Windows (PowerShell 5.1
        # is the default shell). PS 7 tolerated the -c form; 5.1 does not.
        # Robust path: write the composer to a temp .sh (LF endings, no BOM)
        # and run the FILE — the only argument bash gets is a clean path,
        # which PS quotes correctly. `$1` is the clone dir (no $0 placeholder
        # needed in file form: bash sets $0=<file>, $1=<first arg>).
        $composerFile = Join-Path $env:TEMP ("airc-githooks-{0}.sh" -f ([System.Guid]::NewGuid().ToString('N')))
        [System.IO.File]::WriteAllText($composerFile, ($composer -replace "`r`n", "`n"), (New-Object System.Text.UTF8Encoding($false)))
        try {
            & $bashExe --noprofile --norc $composerFile $cloneForBash 2>&1 | ForEach-Object { Write-Host "  $_" }
        } finally {
            Remove-Item -Force $composerFile -ErrorAction SilentlyContinue
        }
        Write-Ok 'Git fetch-before-commit/push staleness guard wired'
    }
}

# -- gh credential helper + git identity (only when gh is already authed) -
# install.ps1 doesn't drive gh auth (it prints the next-step below), so
# this only fires for users who authed before installing. The bootstrap
# (bootstrap-airc.ps1) covers the fresh-auth path. Mirrors install.sh:
#   - wire gh's token into git's credential helper (no gist password pops)
#   - derive git author identity from the gh account when unset, so the
#     first agent commit doesn't die with "Author identity unknown".
# Never clobbers an identity the user already set. No hardcoded values.
$ghAvail  = Get-Command gh  -ErrorAction SilentlyContinue
$gitAvail = Get-Command git -ErrorAction SilentlyContinue
if ($ghAvail -and $gitAvail) {
    # PS 5.1 converts a native command's REDIRECTED stderr into a terminating
    # NativeCommandError under $ErrorActionPreference='Stop' (PS 7 does not).
    # gh/git here legitimately write to stderr on a fresh box — `gh auth
    # status` with no login prints "You are not logged in" — which we already
    # handle via $LASTEXITCODE. Relax EAP for this probe block so those benign
    # stderr lines don't abort the whole install (the ps5 clean-install bug).
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = 'SilentlyContinue'
    try {
        & gh auth status 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            $ghHelper = & git config --global --get-all credential.https://github.com.helper 2>$null
            # Join to a scalar before -notmatch: --get-all can return multiple
            # helper values (array), and PS -notmatch on an array filters rather
            # than returning a strict boolean. Scalar form is correct.
            if ((@($ghHelper) -join "`n") -notmatch 'gh auth git-credential') {
                & gh auth setup-git 2>$null
                if ($LASTEXITCODE -eq 0) { Write-Ok 'gh token wired into git credential helper' }
            }
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
                    Write-Ok "git user.name set from gh: $ghName"
                }
                if (-not $gitEmail -and $ghEmail) {
                    & git config --global user.email $ghEmail
                    Write-Ok "git user.email set from gh: $ghEmail"
                }
            }
        }
    } finally {
        $ErrorActionPreference = $prevEAP
    }
}

# -- Final guidance ------------------------------------------------------
Write-Host ''
Write-Ok 'airc installed.'
Write-Host ''
Write-Host '  Next:'
Write-Host '    1. Open a NEW PowerShell window (so PATH refreshes)'
Write-Host '    2. Authenticate gh once:    gh auth login -h github.com -s gist'
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
