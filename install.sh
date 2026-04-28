#!/usr/bin/env bash
#
# AIRC installer
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
#
# Clones the repo, puts `airc` on PATH, symlinks skills into ~/.claude/skills/

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/airc.git"
CLONE_DIR="${AIRC_DIR:-$HOME/.airc-src}"
# BIN_DIR + SKILLS_TARGET respect env-var overrides so test harnesses
# (and packagers, distros, etc.) can point install.sh at a sandbox
# instead of stomping ~/.local/bin and ~/.claude/skills. Pre-fix, a
# test passing BIN_DIR=/tmp/foo would be silently ignored and the
# real ~/.local/bin/airc symlink would get rewritten to point at the
# test dir — caught when our own canary test corrupted the real install.
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SKILLS_TARGET="${SKILLS_TARGET:-$HOME/.claude/skills}"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
warn()  { printf '  \033[1;33m!\033[0m %s\n' "$*" >&2; }

# ── Prereq auto-install ─────────────────────────────────────────────────
# Mirrors the Windows install.ps1 winget path: detect what's missing,
# install via the platform's package manager, then verify. Designed for
# FIRST-TIME users with nothing pre-installed beyond a shell.
#
# Required: git, gh, openssl, ssh-keygen, python3
# Optional: tailscale (only needed for cross-LAN mesh; LAN works without)
#
# AIRC_SKIP_PREREQS=1 short-circuits the whole block (CI, dev installs,
# users who manage their own packages).

detect_pkgmgr() {
  case "$(uname -s 2>/dev/null)" in
    Darwin)
      if command -v brew >/dev/null 2>&1; then echo "brew"; return; fi
      echo "brew-missing"; return ;;
    Linux)
      if command -v apt-get >/dev/null 2>&1; then echo "apt";    return; fi
      if command -v dnf     >/dev/null 2>&1; then echo "dnf";    return; fi
      if command -v pacman  >/dev/null 2>&1; then echo "pacman"; return; fi
      if command -v apk     >/dev/null 2>&1; then echo "apk";    return; fi
      ;;
    MINGW*|MSYS*|CYGWIN*)
      # Windows Git Bash / MSYS2 / Cygwin. winget is the standard
      # package manager on modern Windows and what install.ps1 uses;
      # it's reachable from Git Bash as winget.exe via PATH or as
      # `cmd /c winget`. If winget isn't there (older Win10), fall
      # through to the unknown branch which emits the manual prereq
      # list. Issue #83 follow-up: pre-fix, install.sh on Git Bash
      # said "Unknown package manager (uname=MINGW64_NT-10.0-26200)"
      # and skipped prereq install entirely.
      if command -v winget.exe >/dev/null 2>&1 || command -v winget >/dev/null 2>&1; then
        echo "winget"; return
      fi
      ;;
  esac
  echo "unknown"
}

# Map a generic prereq name to the package id for a given pkg manager.
# Most names match across managers; the exceptions are listed inline.
pkgname_for() {
  local mgr="$1" prereq="$2"
  case "$prereq" in
    ssh-keygen|ssh)
      case "$mgr" in
        brew)   echo "openssh" ;;
        apt)    echo "openssh-client" ;;
        dnf)    echo "openssh-clients" ;;
        pacman) echo "openssh" ;;
        apk)    echo "openssh-client" ;;
        winget) echo "" ;;  # OpenSSH ships with modern Windows; nothing to install
      esac ;;
    openssl)
      case "$mgr" in
        winget) echo "" ;;  # bundled with Git for Windows; if Git is installed, openssl is there
        *)      echo "openssl" ;;
      esac ;;
    python3)
      case "$mgr" in
        pacman) echo "python" ;;
        winget) echo "Python.Python.3.12" ;;
        *)      echo "python3" ;;
      esac ;;
    git)
      case "$mgr" in
        winget) echo "Git.Git" ;;
        *)      echo "git" ;;
      esac ;;
    gh)
      case "$mgr" in
        winget) echo "GitHub.cli" ;;
        *)      echo "gh" ;;
      esac ;;
    jq)
      case "$mgr" in
        winget) echo "jqlang.jq" ;;
        *)      echo "jq" ;;
      esac ;;
    *) echo "$prereq" ;;
  esac
}

install_with_pkgmgr() {
  local mgr="$1"; shift
  local pkgs=("$@")
  [ ${#pkgs[@]} -eq 0 ] && return 0
  case "$mgr" in
    brew)   brew install "${pkgs[@]}" ;;
    apt)    sudo apt-get update -qq && sudo apt-get install -y "${pkgs[@]}" ;;
    dnf)    sudo dnf install -y "${pkgs[@]}" ;;
    pacman) sudo pacman -S --noconfirm --needed "${pkgs[@]}" ;;
    apk)    sudo apk add --no-cache "${pkgs[@]}" ;;
    winget)
      # winget on Git Bash: install one ID at a time, --accept-* flags so
      # it doesn't prompt during the script. winget.exe is the binary;
      # plain `winget` works if PATHEXT is honored.
      local wbin; wbin=$(command -v winget.exe 2>/dev/null || command -v winget 2>/dev/null || true)
      [ -z "$wbin" ] && return 1
      local pkg
      for pkg in "${pkgs[@]}"; do
        [ -z "$pkg" ] && continue
        "$wbin" install --id "$pkg" --silent --accept-source-agreements --accept-package-agreements 2>&1 \
          || warn "winget install $pkg returned non-zero (may already be installed; continuing)"
      done ;;
    *)      return 1 ;;
  esac
}

# Ensure sshd is installed AND running. Per-platform with one sudo / UAC
# prompt at most. Idempotent — if already running, no-op.
_ensure_sshd_running() {
  case "$(uname -s 2>/dev/null)" in
    Darwin)
      # macOS: sshd is launchd-managed via "Remote Login". Detection
      # without sudo: `launchctl print system` shows system services
      # including com.openssh.sshd when Remote Login is on. Bare
      # `launchctl list` is user-scope and never shows it.
      if launchctl print system 2>/dev/null | grep -qE 'com\.openssh\.sshd($|[[:space:]])' \
         || systemsetup -getremotelogin 2>/dev/null | grep -qi "Remote Login: On"; then
        ok "sshd running (Remote Login enabled)"
        return 0
      fi
      info "Enabling Remote Login (sshd) — admin password prompt incoming."
      info "  airc joiners need this to ssh-tail your messages.jsonl when you host."
      # Two paths: terminal sudo (if a TTY is attached) or osascript GUI
      # admin prompt (when called from non-terminal context — e.g. a
      # Monitor-spawned shell, or via curl|bash piping). The osascript
      # path uses macOS native admin dialog with a branded prompt
      # explaining what airc is doing — Joel 2026-04-27 (continuum
      # relay): "if we can prompt the user, we do NOT have them do
      # annoying setup shit we automate into install."
      if [ -t 0 ] && [ -t 1 ]; then
        # Interactive shell — sudo can read the password.
        if sudo systemsetup -setremotelogin on 2>&1; then
          ok "Remote Login enabled."
        else
          warn "systemsetup failed. Manual: System Settings -> General -> Sharing -> Remote Login."
        fi
      else
        # Non-interactive (Monitor/pipe/script) — use osascript GUI prompt.
        if osascript -e 'do shell script "systemsetup -setremotelogin on" with administrator privileges with prompt "AIRC needs admin to enable Remote Login (sshd) — one-time setup so peers can ssh-tail your messages when you host an airc room."' 2>&1; then
          ok "Remote Login enabled."
        else
          warn "osascript admin dialog cancelled or failed."
          warn "  Manual: System Settings -> General -> Sharing -> Remote Login."
        fi
      fi
      ;;
    Linux)
      # Already running?
      if systemctl is-active --quiet ssh 2>/dev/null || systemctl is-active --quiet sshd 2>/dev/null; then
        ok "sshd running"
        return 0
      fi
      # Install (if missing) + enable. Try Debian/Ubuntu unit name first
      # (ssh) then RHEL/Fedora (sshd). Guarded by detect_pkgmgr — if the
      # package is missing we use install_with_pkgmgr which already
      # handles sudo + the per-distro install command.
      info "Installing + enabling sshd — needed for hosting airc rooms."
      local _pkgmgr; _pkgmgr=$(detect_pkgmgr)
      case "$_pkgmgr" in
        apt|dnf|pacman|apk)
          install_with_pkgmgr "$_pkgmgr" "openssh-server" 2>&1 || \
            warn "openssh-server install failed (already present? Try: airc doctor)."
          # After install, enable + start the right unit.
          if systemctl list-unit-files 2>/dev/null | grep -q "^ssh\.service"; then
            sudo systemctl enable --now ssh 2>&1 \
              && ok "ssh.service enabled + running" \
              || warn "Failed to start ssh.service. Manual: sudo systemctl enable --now ssh"
          elif systemctl list-unit-files 2>/dev/null | grep -q "^sshd\.service"; then
            sudo systemctl enable --now sshd 2>&1 \
              && ok "sshd.service enabled + running" \
              || warn "Failed to start sshd.service. Manual: sudo systemctl enable --now sshd"
          else
            warn "Neither ssh.service nor sshd.service found. Check distro docs."
          fi
          ;;
        *)
          warn "Linux without recognized package manager — install + enable sshd manually."
          ;;
      esac
      ;;
    MINGW*|MSYS*|CYGWIN*)
      # Windows Git Bash: probe via powershell.exe; install via UAC-elevated
      # PowerShell (Start-Process -Verb RunAs).
      #
      # HNS port-22 reservation: Windows HNS (Host Network Service)
      # randomly reserves dynamic port ranges per boot to support
      # Hyper-V/WSL2/Docker. When port 22 falls inside an HNS range,
      # sshd bind() returns EPERM even with admin. Persistent fix:
      # (a) reg-disable HNS auto-exclusion + (b) reserve port 22 in the
      # static excluded-port-range. Both run inside the elevated payload
      # so user clicks UAC once for the whole sshd setup.
      # Diagnosis: continuum-b69f via cross-Mac/Windows coord gist
      # 2026-04-27. Refs:
      #   keasigmadelta.com/blog/how-to-solve-cannot-bind-to-port-...
      #   github.com/docker/for-win/issues/3171
      if ! command -v powershell.exe >/dev/null 2>&1; then
        warn "powershell.exe not on PATH; can't auto-configure sshd."
        return 0
      fi
      local _state
      _state=$(powershell.exe -NoProfile -Command "(Get-Service sshd -ErrorAction SilentlyContinue).Status" 2>/dev/null | tr -d '\r\n ')
      # Single elevated payload: capability + HNS workaround + firewall
      # rule + start + persist. Idempotent — the inner commands check
      # state before writing, so re-running install on a healthy box
      # doesn't re-prompt or duplicate state.
      # DefaultShell = Git for Windows bash (#98). Without this, every
      # Windows airc HOST silently fails inbound `airc msg` from peers
      # because the OpenSSH default shell is cmd.exe, which lacks `cat`,
      # `>>`, and the rest of the POSIX vocabulary airc remote commands
      # rely on. Locate bash.exe; idempotent registry write.
      # Payload wraps work in Start-Transcript so we ALWAYS get a log
       # file we can show the user — the elevated window auto-closes when
       # the script ends and any red errors flash too fast to read (Joel
       # 2026-04-28: "your powershell crashes. It has red all over but
       # blinks for a half second so i have no idea"). Log lives at
       # $env:TEMP\airc-install-elevated.log; bash side surfaces it
       # below regardless of success/failure.
      local _elevated_payload='
$ErrorActionPreference = "Stop";
# Use [System.IO.Path]::GetTempPath() not $env:TEMP — when called from
# Git Bash, the inherited TEMP env var can be the bash-side /tmp, not
# the Windows user temp directory. GetTempPath() asks the OS directly
# (resolves to %LOCALAPPDATA%\Temp on Windows) regardless of the env.
$logPath = Join-Path ([System.IO.Path]::GetTempPath()) "airc-install-elevated.log";
Start-Transcript -Path $logPath -Force | Out-Null;
try {
  Write-Host "==> OpenSSH.Server capability";
  $cap = Get-WindowsCapability -Online -Name "OpenSSH.Server*";
  if ($cap.State -ne "Installed") { Add-WindowsCapability -Online -Name $cap.Name | Out-Null; Write-Host "  installed: $($cap.Name)" } else { Write-Host "  already installed" }
  Write-Host "==> HNS port-22 reservation";
  $reg = (Get-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Services\hns\State" -Name "EnableExcludedPortRange" -ErrorAction SilentlyContinue).EnableExcludedPortRange;
  if ($reg -ne 0) { reg add "HKLM\SYSTEM\CurrentControlSet\Services\hns\State" /v "EnableExcludedPortRange" /d 0 /f | Out-Null; Write-Host "  HNS auto-exclusion disabled" } else { Write-Host "  HNS auto-exclusion already off" }
  $excl = netsh int ipv4 show excludedportrange protocol=tcp | Out-String;
  if ($excl -notmatch "(?m)^\s*22\s+22\b") { netsh int ipv4 add excludedportrange protocol=tcp startport=22 numberofports=1 | Out-Null; Write-Host "  port 22 reserved in static excluded-port-range" } else { Write-Host "  port 22 already reserved" }
  Write-Host "==> Firewall rule";
  if (-not (Get-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -DisplayName "OpenSSH Server (sshd)" -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 | Out-Null;
    Write-Host "  inbound TCP/22 rule created"
  } else { Write-Host "  inbound TCP/22 rule already exists" }
  Write-Host "==> sshd service";
  Start-Service sshd;
  Set-Service -Name sshd -StartupType Automatic;
  Write-Host "  started + auto-start on boot";
  Write-Host "==> DefaultShell registry";
  $bashCandidates = @("C:\Program Files\Git\bin\bash.exe", "C:\Program Files (x86)\Git\bin\bash.exe", "$env:USERPROFILE\AppData\Local\Programs\Git\bin\bash.exe");
  $bashPath = $null;
  foreach ($c in $bashCandidates) { if (Test-Path $c) { $bashPath = $c; break } }
  if (-not $bashPath) { $cmd = Get-Command bash.exe -ErrorAction SilentlyContinue; if ($cmd) { $bashPath = $cmd.Source } }
  if ($bashPath) {
    $cur = (Get-ItemProperty -Path "HKLM:\SOFTWARE\OpenSSH" -Name DefaultShell -ErrorAction SilentlyContinue).DefaultShell;
    if ($cur -ne $bashPath) {
      if (-not (Test-Path "HKLM:\SOFTWARE\OpenSSH")) { New-Item -Path "HKLM:\SOFTWARE\OpenSSH" -Force | Out-Null }
      New-ItemProperty -Path "HKLM:\SOFTWARE\OpenSSH" -Name DefaultShell -Value $bashPath -PropertyType String -Force | Out-Null;
      Write-Host "  DefaultShell -> $bashPath"
    } else { Write-Host "  DefaultShell already $bashPath" }
  } else { Write-Host "  WARN: bash.exe not found; DefaultShell left at OS default (cmd.exe). Install Git for Windows + re-run." }
  Write-Host "";
  Write-Host "airc: sshd ready (capability + HNS + firewall + service auto-start + DefaultShell=bash)";
  $global:LASTEXITCODE = 0;
} catch {
  Write-Host "";
  Write-Host "airc-elevated-error: $_";
  Write-Host "Stack trace:";
  Write-Host $_.ScriptStackTrace;
  $global:LASTEXITCODE = 1;
} finally {
  Stop-Transcript | Out-Null;
}
exit $global:LASTEXITCODE;
'
      case "$_state" in
        Running)
          ok "sshd running (Windows OpenSSH.Server)"
          return 0
          ;;
        Stopped|StopPending|StartPending|Paused|"")
          info "Configuring OpenSSH.Server + HNS port-22 reservation (UAC prompt incoming)."
          info "  airc joiners need this to ssh-tail your messages.jsonl when you host."
          # Log path lives at %LOCALAPPDATA%\Temp\airc-install-elevated.log
          # on Windows. Use [System.IO.Path]::GetTempPath() not $env:TEMP
          # — Git Bash's inherited TEMP=/tmp leaks into powershell.exe and
          # would resolve to /tmp instead of the real Windows user temp,
          # making us look for the log at the wrong path (Joel 2026-04-28
          # — \"Elevated transcript not written\" but the log was written;
          # we just looked at /tmp/airc-install-elevated.log instead of
          # C:\\Users\\green\\AppData\\Local\\Temp\\airc-install-elevated.log).
          local _ps_log_win _ps_log_bash _elev_rc=0
          _ps_log_win=$(powershell.exe -NoProfile -Command "Join-Path ([System.IO.Path]::GetTempPath()) 'airc-install-elevated.log'" 2>/dev/null | tr -d '\r')
          if command -v cygpath >/dev/null 2>&1; then
            _ps_log_bash=$(cygpath -u "$_ps_log_win" 2>/dev/null || echo "")
          else
            # MSYS-style sed translation: 'C:\Users\...' → '/c/Users/...'
            _ps_log_bash=$(printf '%s' "$_ps_log_win" | sed 's|\\|/|g; s|^\([A-Za-z]\):|/\L\1|')
          fi
          info "  elevated log: $_ps_log_win  (also at $_ps_log_bash from Git Bash)"
          # Run the elevated payload. Start-Process exits 0 if it could
          # launch the elevated process; the payload's own exit code is
          # what we care about (it explicitly `exit $LASTEXITCODE`s based
          # on try/catch).
          powershell.exe -NoProfile -Command "Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile -Command \"$_elevated_payload\"'" 2>&1 \
            || _elev_rc=$?
          # Always dump the transcript — success or failure, the user
          # sees what happened. If transcript file is missing, the
          # payload didn't even start (UAC denied / Start-Process
          # itself failed).
          if [ -n "$_ps_log_bash" ] && [ -f "$_ps_log_bash" ]; then
            echo ""
            info "─── elevated PowerShell output ───"
            sed 's/^/    /' "$_ps_log_bash"
            info "─── (end log; full file: $_ps_log_win) ───"
            echo ""
            # Detect failure inside the transcript even if Start-Process
            # itself returned 0 (the elevated PS process could exit
            # non-zero; Start-Process -Wait still propagates that, but
            # check airc-elevated-error pattern as belt-and-suspenders).
            if grep -q "airc-elevated-error:" "$_ps_log_bash"; then
              _elev_rc=1
            fi
          else
            warn "  Elevated transcript not written — UAC denied, or Start-Process failed."
          fi
          if [ "$_elev_rc" = "0" ]; then
            ok "OpenSSH.Server installed + started + HNS port-22 reserved + auto-start + DefaultShell=bash."
          else
            warn "Elevated payload failed (exit $_elev_rc). See log above."
            warn "Manual fix (admin PowerShell):"
            warn "    Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0"
            warn "    reg add HKLM\\SYSTEM\\CurrentControlSet\\Services\\hns\\State /v EnableExcludedPortRange /d 0 /f"
            warn "    netsh int ipv4 add excludedportrange protocol=tcp startport=22 numberofports=1"
            warn "    Start-Service sshd"
            warn "    Set-Service -Name sshd -StartupType Automatic"
            warn "    New-ItemProperty -Path 'HKLM:\\SOFTWARE\\OpenSSH' -Name DefaultShell -Value 'C:\\Program Files\\Git\\bin\\bash.exe' -PropertyType String -Force"
          fi
          ;;
        *)
          warn "sshd state unknown (Get-Service returned: '$_state'). Run airc doctor to diagnose."
          ;;
      esac
      ;;
    *)
      info "sshd auto-config skipped (unsupported platform: $(uname -s))"
      ;;
  esac
}

tailscale_present() {
  # macOS GUI install puts Tailscale.app at /Applications without putting
  # `tailscale` on PATH; Windows winget can install to Program Files OR
  # LocalAppData (user scope) depending on package metadata. Probe many
  # locations cheap-to-thorough.
  command -v tailscale >/dev/null 2>&1 && return 0
  command -v tailscale.exe >/dev/null 2>&1 && return 0
  [ -d /Applications/Tailscale.app ] && return 0
  [ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ] && return 0
  [ -x "/c/Program Files/Tailscale/tailscale.exe" ] && return 0
  [ -x "/c/Program Files (x86)/Tailscale/tailscale.exe" ] && return 0
  # Last-resort Windows probe: `where.exe` searches every PATH+PATHEXT
  # location and catches winget user-scope installs (%LOCALAPPDATA%\...)
  # that aren't in any of the hard-coded paths above. Joel's catch
  # 2026-04-28: post-install said "Tailscale is optional but recommended"
  # even though winget had just installed it to user scope; bash's
  # `command -v tailscale` didn't honor PATHEXT, none of the hard-coded
  # paths matched, so we lied to the user.
  if command -v where.exe >/dev/null 2>&1; then
    where.exe tailscale.exe >/dev/null 2>&1 && return 0
  fi
  return 1
}

install_tailscale() {
  # Optional. macOS: brew cask. Linux: tailscale's official installer.
  # Windows Git Bash: winget (case-sensitive id, see #94).
  tailscale_present && return 0
  case "$(uname -s)" in
    Darwin)
      if command -v brew >/dev/null 2>&1; then
        brew install --cask tailscale 2>/dev/null || warn "Tailscale install via brew failed; install manually: https://tailscale.com/download/mac"
      else
        warn "brew not present; install Tailscale manually: https://tailscale.com/download/mac"
      fi ;;
    Linux)
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL https://tailscale.com/install.sh | sh \
          || warn "Tailscale installer script failed; install manually: https://tailscale.com/download/linux"
      else
        warn "curl missing; install Tailscale manually: https://tailscale.com/download/linux"
      fi ;;
    MINGW*|MSYS*|CYGWIN*)
      # Windows Git Bash: winget. Package id is case-sensitive (#94 —
      # 'tailscale.tailscale' lowercase silently fails; 'Tailscale.Tailscale'
      # is the actual id). Mirrors install.ps1's Install-IfMissing line.
      local wbin; wbin=$(command -v winget.exe 2>/dev/null || command -v winget 2>/dev/null || true)
      if [ -n "$wbin" ]; then
        "$wbin" install --id Tailscale.Tailscale --silent --accept-source-agreements --accept-package-agreements 2>&1 \
          || warn "Tailscale install via winget failed; install manually: https://tailscale.com/download/windows"
      else
        warn "winget not present; install Tailscale manually: https://tailscale.com/download/windows"
      fi ;;
    *)
      warn "Don't know how to install Tailscale on $(uname -s); see https://tailscale.com/download" ;;
  esac
}

ensure_prereqs() {
  [ "${AIRC_SKIP_PREREQS:-0}" = "1" ] && { info "AIRC_SKIP_PREREQS=1 -- skipping prereq install"; return 0; }

  local mgr; mgr=$(detect_pkgmgr)
  if [ "$mgr" = "unknown" ] || [ "$mgr" = "brew-missing" ]; then
    if [ "$mgr" = "brew-missing" ]; then
      warn "macOS detected but Homebrew not found."
      warn "  Install Homebrew first:  /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
      warn "  Then re-run this installer."
    else
      warn "Unknown package manager (uname=$(uname -s)). Skipping prereq auto-install."
    fi
    warn "Required prereqs: git, gh, openssl, openssh-client, python3"
    return 0
  fi

  local missing=() pkgs=() unmappable=()
  # jq added 2026-04-27: airc's gist envelope parser uses jq for the
  # canonical path; bash bare-grep fallback handles JSON-key-prefix
  # leak now (PR fix), but jq is the right tool — without it the
  # fallback can't extract host.addresses[] for multi-address pick.
  # On Git Bash, jq is winget-installable as 'jqlang.jq'.
  for cmd in git gh jq openssl ssh-keygen python3; do
    # Strict probe: presence on PATH AND a successful --version invocation.
    # Used selectively: python3 needs the strict variant because Windows
    # Store's python3.exe alias is on PATH but exits 49 with a Store-
    # redirect (continuum-b69f, 2026-04-27). git/gh/jq/openssl all
    # support --version cleanly. ssh-keygen does NOT have a version
    # flag at all (different from `ssh -V`); calling `ssh-keygen
    # --version` exits non-zero on every install, so the strict probe
    # produces false positives — Joel 2026-04-28 saw "ssh-keygen needs
    # manual install on winget" on a perfectly good Git for Windows
    # install. Skip the strict variant for ssh-keygen; presence-on-PATH
    # is sufficient since Git for Windows bundles a working binary.
    local _missing=0
    if ! command -v "$cmd" >/dev/null 2>&1; then
      _missing=1
    elif [ "$cmd" != "ssh-keygen" ] && ! "$cmd" --version >/dev/null 2>&1; then
      _missing=1
    fi
    if [ "$_missing" = "1" ]; then
      missing+=("$cmd")
      local pkg; pkg=$(pkgname_for "$mgr" "$cmd")
      if [ -z "$pkg" ]; then
        # Manager has no auto-install path for this prereq (e.g., winget
        # treats ssh + openssl as bundled-with-Windows / Git-for-Windows
        # but the user hits this case if those bundles are absent).
        # Surface clearly instead of silently skipping (#92 Copilot).
        unmappable+=("$cmd")
      else
        pkgs+=("$pkg")
      fi
    fi
  done
  if [ ${#missing[@]} -gt 0 ]; then
    if [ ${#pkgs[@]} -gt 0 ]; then
      info "Installing missing prereqs via $mgr: ${pkgs[*]}"
      if install_with_pkgmgr "$mgr" "${pkgs[@]}"; then
        ok "Auto-installable prereqs installed"
      else
        warn "Package install reported failure; airc may not run until you fix: ${missing[*]}"
      fi
    else
      warn "Missing prereqs not auto-installable on $mgr: ${missing[*]}"
    fi
    if [ ${#unmappable[@]} -gt 0 ]; then
      warn "These prereqs need manual install on $mgr: ${unmappable[*]}"
      case "$mgr" in
        winget)
          warn "  ssh / ssh-keygen: Settings -> Apps -> Optional Features -> Add OpenSSH Client"
          warn "  openssl: bundled with Git for Windows -- 'winget install Git.Git' provides it" ;;
      esac
    fi
  else
    ok "All required prereqs present"
  fi

  # sshd: airc joiners ssh into the host's airc_home to tail messages.
  # Every airc user who'll host a room (which is most users — first to
  # discover becomes the host) needs sshd RUNNING. install.sh actually
  # turns it on instead of just warning, since "warn + leave it to the
  # user" was Joel's "this needs to be in the install dude" pushback
  # 2026-04-27. ONE sudo / UAC prompt during install (same shape as
  # install_with_pkgmgr already uses for apt/dnf/etc); after that
  # airc just works for hosting.
  #
  # AIRC_SKIP_SSHD=1 short-circuits the whole block — for headless CI
  # boxes that genuinely don't host, or environments that manage sshd
  # via their own config-management (Ansible, Chef).
  #
  # Auto-detect: GitHub Actions sets CI=true; so does almost every CI
  # system (Travis, CircleCI, GitLab, BuildKite, Jenkins). On macOS
  # specifically, the osascript admin-prompt path hangs forever in CI
  # because there's no Touch ID / password input — the runner job
  # silently runs for the full 6-hour timeout. Skip when CI=true so
  # the install completes cleanly and CI tests the rest of the path.
  if [ "${CI:-}" = "true" ] || [ "${CI:-}" = "1" ]; then
    info "CI=true — skipping sshd setup (no host-capability test in CI)"
  elif [ "${AIRC_SKIP_SSHD:-0}" != "1" ]; then
    _ensure_sshd_running
  fi

  # Tailscale is optional -- only needed for cross-LAN mesh. LAN-only
  # works fine without it, so we attempt install but don't fail loud.
  # Skip in CI: brew install --cask tailscale on macOS runners is slow
  # (multi-minute download + GUI app install) and there's no tailnet
  # behind the runner anyway. The install itself is what we're gating
  # on — Tailscale-as-optional is documented; CI doesn't need it.
  if [ "${CI:-}" = "true" ] || [ "${CI:-}" = "1" ]; then
    info "CI=true — skipping Tailscale install (optional, no tailnet in CI)"
  elif ! tailscale_present; then
    info "Tailscale not present (optional -- LAN mesh works without it). Attempting install ..."
    install_tailscale
  fi

  # gh auth: required for the gist substrate (#general room discovery).
  # We can't auto-login (browser flow), but we surface the exact command
  # so the user runs it once before `airc join`.
  if command -v gh >/dev/null 2>&1; then
    if ! gh auth status >/dev/null 2>&1; then
      warn "gh CLI is not authenticated. Run once before 'airc join':"
      warn "    gh auth login -s gist"
    fi
  fi
}

ensure_prereqs

# ── Clone or update ─────────────────────────────────────────────────────

if [ -d "$CLONE_DIR/.git" ]; then
  info "Updating existing install"
  # Recovery: if the install dir is on a non-channel branch (e.g. someone
  # / some AI checked out a feature branch for testing and forgot to
  # switch back), the ff-pull below fails with cryptic "Not possible to
  # fast-forward". Worse, the user can't escape via `airc canary` if
  # they're on a pre-channels binary — `canary` is an unknown command
  # there. So install.sh itself takes responsibility: detect non-channel
  # branches + auto-switch to the saved channel (or main) before pulling.
  CURRENT_BRANCH=$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
  SAVED_CHANNEL=""
  [ -f "$CLONE_DIR/.channel" ] && SAVED_CHANNEL=$(tr -d '[:space:]' < "$CLONE_DIR/.channel")
  TARGET_BRANCH="${SAVED_CHANNEL:-main}"
  case "$CURRENT_BRANCH" in
    main|canary)
      # On a known channel — leave it alone unless the saved channel
      # disagrees (e.g. user just `airc channel canary`'d but didn't
      # update yet).
      if [ -n "$SAVED_CHANNEL" ] && [ "$SAVED_CHANNEL" != "$CURRENT_BRANCH" ]; then
        info "Saved channel '$SAVED_CHANNEL' differs from current branch '$CURRENT_BRANCH' — switching"
        git -C "$CLONE_DIR" fetch --quiet origin "$SAVED_CHANNEL"
        git -C "$CLONE_DIR" checkout -q "$SAVED_CHANNEL" \
          || git -C "$CLONE_DIR" checkout -q -B "$SAVED_CHANNEL" "origin/$SAVED_CHANNEL"
      fi
      ;;
    *)
      info "Install dir on '$CURRENT_BRANCH' (not a known channel) — switching to '$TARGET_BRANCH'"
      git -C "$CLONE_DIR" fetch --quiet origin "$TARGET_BRANCH" || {
        echo "ERROR: Couldn't fetch origin/$TARGET_BRANCH. Network? gh auth?" >&2
        exit 1
      }
      git -C "$CLONE_DIR" checkout -q "$TARGET_BRANCH" \
        || git -C "$CLONE_DIR" checkout -q -B "$TARGET_BRANCH" "origin/$TARGET_BRANCH" \
        || {
          cat >&2 <<EOF
ERROR: Couldn't switch $CLONE_DIR to '$TARGET_BRANCH'.
Recover manually:
  cd $CLONE_DIR
  git fetch origin
  git status               # see why checkout was blocked
  git stash                # if you have local edits worth keeping
  git checkout $TARGET_BRANCH
  git pull --ff-only
  bash install.sh
EOF
          exit 1
        }
      ;;
  esac
  if ! git -C "$CLONE_DIR" pull --ff-only --quiet 2>&1; then
    cat >&2 <<EOF
ERROR: Couldn't fast-forward $CLONE_DIR (currently on $(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)).
Likely cause: local edits or a divergent history.
Recover with:
  cd $CLONE_DIR
  git status
  git stash               # if you have local edits worth keeping
  git fetch origin
  git reset --hard origin/$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)
  bash install.sh
EOF
    exit 1
  fi
else
  # First install. Honor AIRC_CHANNEL if set so users can land on canary
  # directly via `AIRC_CHANNEL=canary curl|bash` without a follow-up
  # `airc canary && airc update` dance. Default to main (the release
  # branch) when AIRC_CHANNEL is unset. Caught by vhsm-d1f4 2026-04-28
  # during the #191 release-gate fresh-install verification: env var was
  # silently ignored, install landed on main.
  CHANNEL_TARGET="${AIRC_CHANNEL:-main}"
  case "$CHANNEL_TARGET" in
    main|canary) ;;
    *)
      warn "AIRC_CHANNEL='$CHANNEL_TARGET' is not a known channel (main, canary). Defaulting to main."
      CHANNEL_TARGET="main"
      ;;
  esac
  info "Installing AIRC (channel: $CHANNEL_TARGET)"
  git clone --quiet --branch "$CHANNEL_TARGET" "$REPO_URL" "$CLONE_DIR"
  # Persist the channel choice so future `airc update` follows the same
  # branch. Mirrors what `airc canary` / `airc main` write.
  echo "$CHANNEL_TARGET" > "$CLONE_DIR/.channel"
fi

# ── airc on PATH ───────────────────────────────────────────────────────

mkdir -p "$BIN_DIR"
ln -sf "$CLONE_DIR/airc" "$BIN_DIR/airc"
# Back-compat: `relay` still works for muscle-memory and stale docs.
# The airc binary detects the invocation name and behaves identically.
ln -sf "$CLONE_DIR/airc" "$BIN_DIR/relay"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    if [ -f "$rc" ] && ! grep -q 'airc' "$rc"; then
      echo 'export PATH="$HOME/.local/bin:$PATH"  # airc' >> "$rc"
      ok "Added ~/.local/bin to PATH in $(basename "$rc")"
      break
    fi
  done
  export PATH="$BIN_DIR:$PATH"
fi

# ── Skills into Claude Code ─────────────────────────────────────────────

if [ -d "$CLONE_DIR/skills" ]; then
  mkdir -p "$SKILLS_TARGET"

  # Clean up old symlinks from previous installs.
  # Includes the airc-classic skill names (connect/send/rename/disconnect) that
  # were renamed to IRC-canonical (join/msg/nick/quit) — leaving the old symlinks
  # in place would shadow the new skills with stale content.
  for old in "$SKILLS_TARGET"/relay-* "$SKILLS_TARGET"/monitor "$SKILLS_TARGET"/setup "$SKILLS_TARGET"/uninstall \
             "$SKILLS_TARGET"/connect "$SKILLS_TARGET"/send "$SKILLS_TARGET"/rename "$SKILLS_TARGET"/disconnect; do
    [ -L "$old" ] && rm "$old" 2>/dev/null
  done

  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$SKILLS_TARGET/$skill_name"
    # If the target is a real directory (from a pre-rename hand-install
    # or an old copy-based installer), it shadows the new symlink. Nuke it.
    if [ -d "$target" ] && [ ! -L "$target" ]; then
      rm -rf "$target"
    elif [ -L "$target" ]; then
      rm "$target"
    fi
    ln -sf "$skill_dir" "$target"
    ok "Skill: /$skill_name"
  done
fi

# ── Tailscale login check ──────────────────────────────────────────────
# Common state: Tailscale is installed but the user isn't signed in (just
# rebooted, fresh install, auth expired). Without this check, the user's
# first 'airc join' silently hangs trying to reach a Tailscale CGNAT IP
# until the SSH timeout, then prints a confusing "daemon down" message.
# Detect it here and trigger sign-in proactively.

ts_post_check() {
  local ts_bin=""
  if command -v tailscale >/dev/null 2>&1; then
    ts_bin="tailscale"
  elif command -v tailscale.exe >/dev/null 2>&1; then
    ts_bin="tailscale.exe"
  elif [ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]; then
    ts_bin="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
  elif [ -x "/c/Program Files/Tailscale/tailscale.exe" ]; then
    # Windows Git Bash: winget installs Tailscale to Program Files;
    # PATH may not yet include it in the current shell. Mirror
    # airc.ps1's resolve_tailscale_bin candidates.
    ts_bin="/c/Program Files/Tailscale/tailscale.exe"
  elif [ -x "/c/Program Files (x86)/Tailscale/tailscale.exe" ]; then
    ts_bin="/c/Program Files (x86)/Tailscale/tailscale.exe"
  elif command -v where.exe >/dev/null 2>&1; then
    # Last resort: where.exe searches every PATH+PATHEXT location.
    # Catches winget user-scope installs (%LOCALAPPDATA%\...). Translate
    # the returned Windows path to MSYS form for [ -x ].
    local _wherewin
    _wherewin=$(where.exe tailscale.exe 2>/dev/null | head -1 | tr -d '\r')
    if [ -n "$_wherewin" ]; then
      if command -v cygpath >/dev/null 2>&1; then
        ts_bin=$(cygpath -u "$_wherewin" 2>/dev/null || echo "")
      else
        ts_bin=$(printf '%s' "$_wherewin" | sed 's|\\|/|g; s|^\([A-Za-z]\):|/\L\1|')
      fi
    fi
  fi
  [ -z "$ts_bin" ] && return 0   # not installed, nothing to nag about

  local ts_out
  ts_out=$("$ts_bin" status 2>&1) || true
  case "$ts_out" in
    *"Logged out"*|*"NeedsLogin"*)
      echo ""
      warn "Tailscale is installed but you're not signed in."
      case "$(uname -s)" in
        Darwin)
          if [ -d /Applications/Tailscale.app ]; then
            info "Opening Tailscale.app — sign in there before running 'airc join'."
            open -a Tailscale 2>/dev/null || true
          else
            info "Sign in:  tailscale up"
          fi ;;
        MINGW*|MSYS*|CYGWIN*)
          info "Click the Tailscale tray icon to sign in, or run:  tailscale up"
          info "Do this BEFORE 'airc join', or cross-machine joins will hang." ;;
        *)
          info "Sign in:  tailscale up   (follow the printed URL)" ;;
      esac
      ;;
    *)
      # Logged in / running normally — silent (good UX, nothing to nag).
      ;;
  esac
}

ts_post_check

# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
# Tailscale post-install message — be honest about installed state. The
# pre-fix text always read "Tailscale is optional but recommended:
# https://tailscale.com" even when winget had just installed it 30s ago,
# which (per Joel 2026-04-28) reads as a fail. ts_post_check above
# already nudges sign-in if installed-but-logged-out, so here we only
# print the "go install it" line when tailscale really isn't present.
if tailscale_present; then
  :  # ts_post_check handled the messaging if relevant
else
  echo "  Cross-LAN mesh? Tailscale is optional (not installed):"
  echo "    https://tailscale.com    (then: tailscale up)"
  echo "  Same-LAN mesh works without it; gist orchestration handles either."
  echo ""
fi
echo "  Next:"
echo "    1. gh auth login -s gist          # one-time, browser flow"
echo "    2. airc join                      # auto-#general (joins existing or hosts)"
echo "    3. airc msg @<peer> <message>     # DM (or omit @peer to broadcast)"
echo ""
echo "  Diagnose anytime:    airc doctor"
echo ""
