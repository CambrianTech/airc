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

# MSYS / Git Bash path conversion. Three callsites in this file used the
# same `if command -v cygpath ... else sed ...` block; #205 Target #3
# collapsed them. Mirrors lib/airc_bash/platform_adapters.sh's helpers
# (defined twice on purpose: install.sh runs pre-clone so it can't
# source from $CLONE_DIR, and the helper bodies are tiny).
_to_win_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1" 2>/dev/null
  else
    printf '%s' "$1" | sed 's|^/\([a-z]\)/|\U\1:\\\\|; s|/|\\\\|g'
  fi
}
_to_bash_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1" 2>/dev/null
  else
    printf '%s' "$1" | sed 's|\\|/|g; s|^\([A-Za-z]\):|/\L\1|'
  fi
}

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
      # Stage payload as a .ps1 file in $CLONE_DIR (Joel + continuum-b69f
      # 2026-04-28). Pre-fix: payload was inlined as
      #   ... -ArgumentList '-NoProfile -Command "$_elevated_payload"'
      # but the payload itself contains many "" (PowerShell strings) and
      # \\ (registry paths). Four layers of escaping (bash-double, ps1-
      # outer-Command, Start-Process-ArgumentList-single, inner-Command-
      # double) silently mangled the payload — PowerShell never parsed it,
      # the elevated window opened, ran nothing, exited silently, no
      # transcript ever written. continuum verified the .ps1 file approach
      # writes a clean transcript every time.
      # Stage in TMPDIR, NOT $CLONE_DIR — git clone (line ~872) needs an
      # empty/nonexistent target. Pre-fix, on a fresh-install user who'd
      # nuked ~/.airc-src, this function ran first via ensure_prereqs,
      # mkdir+wrote install-elevated.ps1 into $CLONE_DIR, and the
      # subsequent `git clone --branch ... $CLONE_DIR` died with
      # "destination path '...' already exists and is not an empty
      # directory." Hostile error, no recovery hint, broke #249's
      # Windows fresh-install row. Caught by continuum-b69f 2026-04-29.
      local _elevated_ps1="${TMPDIR:-/tmp}/airc-install-elevated.ps1"
      mkdir -p "$(dirname "$_elevated_ps1")"
      # NOTE: keep this heredoc ASCII-only. PowerShell 5.1 reads BOMless
      # .ps1 files as the system codepage (cp1252 on most Windows). A
      # UTF-8 em-dash (0xE2 0x80 0x94) ends in byte 0x94, which in
      # cp1252 is RIGHT-DOUBLE-QUOTATION-MARK -- the parser sees it as
      # a closing string quote and the rest of the file fails to parse.
      # We also add a UTF-8 BOM below as defense-in-depth, AND the bash
      # side runs a parse-check pass before invoking elevation so any
      # parser error fails loud (no silent .ps1 launch).
      cat > "$_elevated_ps1" <<'PSPAYLOAD'
$logPath = Join-Path ([System.IO.Path]::GetTempPath()) "airc-install-elevated.log";
Start-Transcript -Path $logPath -Force | Out-Null;

# No global try/catch, no $ErrorActionPreference = "Stop". Each step
# runs plainly; if a cmdlet errors, PowerShell prints the error to the
# transcript and execution continues. Bash side detects success/failure
# from Get-Service sshd post-check, not from this script's exit code.
# Anything wrapped in try/catch below is wrapped because the failure is
# *expected* and *recoverable* (e.g. ssh-keygen missing -> warn + skip).

Write-Host "==> OpenSSH.Server capability";
$cap = Get-WindowsCapability -Online -Name "OpenSSH.Server*";
if ($cap.State -ne "Installed") {
  Add-WindowsCapability -Online -Name $cap.Name | Out-Null;
  Write-Host "  installed: $($cap.Name)"
} else { Write-Host "  already installed" }

Write-Host "==> SSH host keys (regenerate so ACLs are clean from birth)";
# Why "delete + regenerate" instead of "fix ACLs on existing":
#
# Verified on continuum-b69f's box (2026-04-28): even after icacls reset
# to SYSTEM + Administrators only, sshd still refused with error:5
# (ACCESS_DENIED) and error:13 (ACL fails OpenSSH secure_permission_check).
# Apparently icacls /grant alone isn't enough -- the file owner and the
# combination of explicit + inherited ACEs has to match what OpenSSH's
# secure_permission_check expects, which is fragile.
#
# Cleaner approach: nuke any existing host keys, then run ssh-keygen -A
# from this elevated SYSTEM-context process. ssh-keygen -A sets the
# right ACLs at creation time (owner = SYSTEM, ACEs = SYSTEM + Admins).
# Since this is install-time setup and the host hasn't published any
# fingerprint yet, regenerating is safe -- nobody is trusting these
# keys yet from a client.
$sshKeygen = Join-Path $env:WINDIR "System32\OpenSSH\ssh-keygen.exe";
if (-not (Test-Path $sshKeygen)) {
  Write-Host "  WARN: ssh-keygen.exe not found at $sshKeygen -- sshd will fail to start"
} else {
  $sshDir = 'C:\ProgramData\ssh';
  if (-not (Test-Path $sshDir)) { New-Item -Path $sshDir -ItemType Directory -Force | Out-Null }
  $existing = Get-ChildItem (Join-Path $sshDir 'ssh_host_*') -ErrorAction SilentlyContinue
  if ($existing) {
    Write-Host "  removing $($existing.Count) existing host key file(s)"
    $existing | Remove-Item -Force -ErrorAction SilentlyContinue
  }
  & $sshKeygen -A 2>&1 | ForEach-Object { Write-Host "  ssh-keygen: $_" }
  # ssh-keygen -A on Windows leaves an ACE for the user who ran it
  # (e.g. BIGMAMA\green:(M) for an admin elevation), even though that
  # user is just the file creator. OpenSSH's secure_permission_check
  # rejects any ACE that isn't owner / SYSTEM / Administrators -- so
  # we strip the creator's ACE explicitly. Verified on continuum-b69f
  # 2026-04-28: with regenerate alone, sshd kept failing with error 13
  # (ACL secure_permission_check); with this strip, the ACL is just
  # SYSTEM + Administrators and sshd accepts it.
  # ssh-keygen -A leaves the file owner as the user who ran it
  # (BIGMAMA\green even when running elevated). OpenSSH's
  # secure_permission_check requires owner in {SYSTEM, Administrators,
  # running sshd user}. Setting owner to SYSTEM is the safe default.
  $me = (whoami).Trim()
  $newKeys = Get-ChildItem (Join-Path $sshDir 'ssh_host_*_key') -ErrorAction SilentlyContinue
  foreach ($k in $newKeys) {
    icacls $k.FullName /setowner 'NT AUTHORITY\SYSTEM' 2>&1 | Out-Null
    icacls $k.FullName /inheritance:r 2>&1 | Out-Null
    icacls $k.FullName /grant 'NT AUTHORITY\SYSTEM:(F)' 'BUILTIN\Administrators:(F)' 2>&1 | Out-Null
    icacls $k.FullName /remove:g $me 2>&1 | Out-Null
  }
  # Dump the post-fix ACL + OWNER on the rsa key so we can see in the
  # transcript whether the result matches what sshd expects: owner must
  # be SYSTEM or Administrators, ACEs must be only owner + SYSTEM + Admins.
  $rsa = Join-Path $sshDir 'ssh_host_rsa_key'
  if (Test-Path $rsa) {
    Write-Host "  post-fix ACL on ssh_host_rsa_key:"
    icacls $rsa 2>&1 | ForEach-Object { Write-Host "    $_" }
    Write-Host "  post-fix OWNER on ssh_host_rsa_key: $((Get-Acl $rsa).Owner)"
  }
}

Write-Host "==> SSH directory ACLs (C:\ProgramData\ssh + logs/)";
# Per Microsoft KB on Error 1067 / Event 7034 (Oct 2024 Windows update
# regression that became permanent in newer builds):
#   "This issue occurs if the C:\ProgramData\ssh and C:\ProgramData\ssh\logs
#    folders have incorrect permissions. The permissions might be too limited
#    or too open. For example, the SYSTEM account or the Administrators group
#    might not have write permissions. For a second example, regular users
#    might have write or full control permissions."
# https://learn.microsoft.com/en-us/troubleshoot/windows-server/system-management-components/error-1053-1067-7034-after-update-openssh-doesnt-start
#
# Required ACL on each folder:
#   SYSTEM              : Full Control
#   Administrators      : Full Control
#   Authenticated Users : Read & execute (read-only, no write)
# Owner: SYSTEM (not the user who created the folder).
$sshDir = 'C:\ProgramData\ssh'
$logsDir = Join-Path $sshDir 'logs'
foreach ($d in @($sshDir, $logsDir)) {
  if (-not (Test-Path $d)) { New-Item -Path $d -ItemType Directory -Force | Out-Null }
  icacls $d /setowner 'NT AUTHORITY\SYSTEM' 2>&1 | Out-Null
  icacls $d /inheritance:r 2>&1 | Out-Null
  icacls $d /grant 'NT AUTHORITY\SYSTEM:(OI)(CI)(F)' 'BUILTIN\Administrators:(OI)(CI)(F)' 'NT AUTHORITY\Authenticated Users:(OI)(CI)(RX)' 2>&1 | Out-Null
  Write-Host "  $d :"
  icacls $d 2>&1 | Select-Object -First 5 | ForEach-Object { Write-Host "    $_" }
}

Write-Host "==> sshd dry-run (config + key load test)";
# Run sshd -t from elevated context to surface the *real* reason sshd
# is failing -- Start-Service sshd hides the underlying error behind a
# generic "Failed to start service" message. -t exits non-zero with a
# specific error message ("no hostkeys available", config syntax,
# privilege separation user missing, etc.). Captures stderr too.
$sshdExe = Join-Path $env:WINDIR "System32\OpenSSH\sshd.exe"
if (Test-Path $sshdExe) {
  $sshdTest = & $sshdExe -t 2>&1
  $sshdTestExit = $LASTEXITCODE
  if ($sshdTestExit -eq 0) {
    Write-Host "  sshd -t: OK (exit 0)"
  } else {
    Write-Host "  sshd -t: FAILED (exit $sshdTestExit)";
    $sshdTest | ForEach-Object { Write-Host "    $_" }
  }
}

Write-Host "==> HNS port-22 reservation";
$reg = (Get-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Services\hns\State" -Name "EnableExcludedPortRange" -ErrorAction SilentlyContinue).EnableExcludedPortRange;
$regChanged = $false
if ($reg -ne 0) {
  reg add "HKLM\SYSTEM\CurrentControlSet\Services\hns\State" /v "EnableExcludedPortRange" /d 0 /f | Out-Null;
  Write-Host "  HNS auto-exclusion disabled"
  $regChanged = $true
} else { Write-Host "  HNS auto-exclusion already off" }
$excl = netsh int ipv4 show excludedportrange protocol=tcp | Out-String;
if ($excl -notmatch "(?m)^\s*22\s+22\b") {
  netsh int ipv4 add excludedportrange protocol=tcp startport=22 numberofports=1 | Out-Null;
  Write-Host "  port 22 reserved in static excluded-port-range"
} else { Write-Host "  port 22 already reserved" }

# Verify port 22 is actually claimable. If HNS has it reserved at a
# layer below netsh-visible (Hyper-V/WSL2/Docker share dynamic port
# ranges via HNS), a restart of the HNS service is the only way to
# re-evaluate the reservation. Without this, netsh shows port 22
# excluded but sshd-as-LocalSystem still gets EACCES on bind:
#   sshd: error: Bind to port 22 on 0.0.0.0 failed: Permission denied.
#   sshd: fatal: Cannot bind any address.
# Verified on continuum-b69f 2026-04-28 in OpenSSH/Admin event log.
$hns = Get-Service hns -ErrorAction SilentlyContinue
if ($hns -and $hns.Status -eq 'Running') {
  Write-Host "  restarting HNS service so port-22 reservation takes effect"
  Restart-Service hns -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 2
  Write-Host "  HNS state: $((Get-Service hns).Status)"
}

Write-Host "==> Firewall rule (TCP/22 inbound)";
if (-not (Get-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -ErrorAction SilentlyContinue)) {
  New-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -DisplayName "OpenSSH Server (sshd)" -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 | Out-Null;
  Write-Host "  inbound TCP/22 rule created"
} else { Write-Host "  inbound TCP/22 rule already exists" }

Write-Host "==> sshd service (start + auto-start on boot)";
Start-Service sshd;
Set-Service -Name sshd -StartupType Automatic;
Write-Host "  Get-Service sshd: $((Get-Service sshd).Status)";

Write-Host "==> DefaultShell registry (bash for joiners)";
$bashCandidates = @("C:\Program Files\Git\bin\bash.exe", "C:\Program Files (x86)\Git\bin\bash.exe", "$env:USERPROFILE\AppData\Local\Programs\Git\bin\bash.exe");
$bashPath = $null;
foreach ($c in $bashCandidates) { if (Test-Path $c) { $bashPath = $c; break } }
if (-not $bashPath) { $cmd = Get-Command bash.exe -ErrorAction SilentlyContinue; if ($cmd) { $bashPath = $cmd.Source } }
if (-not $bashPath) {
  Write-Host "  WARN: bash.exe not found; DefaultShell left at OS default. Install Git for Windows + re-run."
} else {
  $cur = (Get-ItemProperty -Path "HKLM:\SOFTWARE\OpenSSH" -Name DefaultShell -ErrorAction SilentlyContinue).DefaultShell;
  if ($cur -eq $bashPath) {
    Write-Host "  DefaultShell already $bashPath"
  } else {
    if (-not (Test-Path "HKLM:\SOFTWARE\OpenSSH")) { New-Item -Path "HKLM:\SOFTWARE\OpenSSH" -Force | Out-Null }
    New-ItemProperty -Path "HKLM:\SOFTWARE\OpenSSH" -Name DefaultShell -Value $bashPath -PropertyType String -Force | Out-Null;
    Write-Host "  DefaultShell -> $bashPath"
  }
}

Write-Host "";
Write-Host "airc: elevated install steps complete";
Stop-Transcript | Out-Null;
exit 0;
PSPAYLOAD

      # Defense-in-depth: prepend a UTF-8 BOM so PowerShell 5.1 reads
      # the .ps1 as UTF-8 (not cp1252). Heredoc is ASCII-only so this
      # is just insurance for future edits.
      if [ -f "$_elevated_ps1" ]; then
        local _tmp_bom="$_elevated_ps1.bom"
        printf '\xEF\xBB\xBF' > "$_tmp_bom"
        cat "$_elevated_ps1" >> "$_tmp_bom"
        mv "$_tmp_bom" "$_elevated_ps1"
      fi

      # Translate the .ps1 path to Windows form for Start-Process -File
      # and the parse-check below.
      local _elevated_ps1_win; _elevated_ps1_win=$(_to_win_path "$_elevated_ps1")

      # Pre-flight parse-check: catch syntax errors in the staged .ps1
      # BEFORE we trigger UAC. Without this, a parser error means the
      # elevated window opens, fails to parse, blinks closed, no log
      # is written, bash side reports "transcript not written" and the
      # user has no idea what went wrong (Joel 2026-04-28: "we prefer
      # parser issues to actually error" -- this is how we make them
      # actually error). Parser errors here abort the install loud.
      local _parse_errs
      _parse_errs=$(powershell.exe -NoProfile -Command "
        \$tokens = \$null; \$errors = \$null;
        [System.Management.Automation.Language.Parser]::ParseFile('$_elevated_ps1_win', [ref]\$tokens, [ref]\$errors) | Out-Null;
        if (\$errors) { \$errors | ForEach-Object { Write-Output \$_.ToString() } }
      " 2>&1 | tr -d '\r')
      if [ -n "$_parse_errs" ]; then
        warn "Staged elevated payload has PARSE ERRORS -- aborting before UAC."
        warn "  This is a bug in install.sh. File a bug w/ this output:"
        printf '%s\n' "$_parse_errs" | sed 's/^/    /'
        warn "  staged file: $_elevated_ps1_win"
        return 1
      fi
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
          _ps_log_bash=$(_to_bash_path "$_ps_log_win")
          info "  elevated payload: $_elevated_ps1_win"
          info "  elevated log:     $_ps_log_win"
          info "  (bash log path:   $_ps_log_bash)"
          # Run the elevated payload via -File (no quoting hell). Start-
          # Process -Wait propagates the elevated process's exit code.
          # -ExecutionPolicy Bypass so the elevated PS doesn't refuse
          # the unsigned .ps1.
          powershell.exe -NoProfile -Command "Start-Process powershell -Verb RunAs -Wait -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File','$_elevated_ps1_win')" 2>&1 \
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
          # Belt-and-suspenders: re-query sshd state from non-elevated PS
          # (continuum-b69f 2026-04-28). If the elevated payload claimed
          # exit 0 but sshd isn't actually Running, surface that — the
          # silent-success-while-broken path was the worst version of
          # this bug. The Get-Service call is cheap; doing it always
          # is fine.
          local _post_state
          _post_state=$(powershell.exe -NoProfile -Command "(Get-Service sshd -ErrorAction SilentlyContinue).Status" 2>/dev/null | tr -d '\r ')
          if [ "$_elev_rc" = "0" ] && [ "$_post_state" = "Running" ]; then
            ok "OpenSSH.Server installed + sshd Running + HNS port-22 reserved + auto-start + DefaultShell=bash."
          elif [ "$_elev_rc" = "0" ]; then
            warn "Elevated payload exit 0 but sshd state is '$_post_state' — partial install."
            warn "  Re-run install or check elevated log: $_ps_log_win"
            _elev_rc=1
          else
            warn "Elevated payload failed (exit $_elev_rc, sshd state '$_post_state'). See log above."
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
      # Joel 2026-04-29: 'whatever agent we have ought to talk to the
      # user about prereq if they dont have something, and it should
      # still auto install for the most part'.
      # When stdin is a TTY, just RUN the official Homebrew installer
      # — it asks for the user's password (sudo) and runs cleanly.
      # User clicks through prompts. AI-driven installs see the prompt
      # surface and can guide the user. Non-TTY installs fall through
      # to the manual instruction.
      warn "macOS detected but Homebrew not found."
      if [ -t 0 ] && [ -t 1 ]; then
        info "  Running Homebrew's official installer now (you'll be prompted for sudo)..."
        if /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"; then
          # Refresh PATH so the rest of this script sees brew.
          if [ -x /opt/homebrew/bin/brew ]; then
            eval "$(/opt/homebrew/bin/brew shellenv)"
          elif [ -x /usr/local/bin/brew ]; then
            eval "$(/usr/local/bin/brew shellenv)"
          fi
          if command -v brew >/dev/null 2>&1; then
            ok "Homebrew installed and active in this session"
            mgr="brew"
          else
            warn "Homebrew install ran but 'brew' still not on PATH — open a new shell and re-run install.sh"
            return 0
          fi
        else
          warn "Homebrew install did not complete. Manual:"
          warn "  /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
          warn "  Then re-run this installer."
          return 0
        fi
      else
        warn "  Install Homebrew first:  /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
        warn "  Then re-run this installer."
        warn "  (auto-install requires a TTY; this is a non-interactive run)"
        return 0
      fi
    else
      warn "Unknown package manager (uname=$(uname -s)). Skipping prereq auto-install."
      warn "Required prereqs: git, gh, openssl, python3"
      return 0
    fi
  fi

  local missing=() pkgs=() unmappable=()
  # #188: jq removed — airc's gist envelope parser now uses Python's
  # stdlib JSON (lib/airc_core/gistparse.py). Python was already a hard
  # dep since #152 Phase 0; jq was redundant. Drop the dep + the
  # winget step that would install it.
  for cmd in git gh openssl ssh-keygen python3; do
    # Strict probe: presence on PATH AND a successful --version invocation.
    # Used selectively: python3 needs the strict variant because Windows
    # Store's python3.exe alias is on PATH but exits 49 with a Store-
    # redirect (continuum-b69f, 2026-04-27). git/gh/openssl all
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
  # Phase 3c: sshd setup + Tailscale install removed from default path.
  # Cross-network messaging routes through gh-as-bearer (envelope-encrypted
  # gist), which works on every platform with `gh auth login` — no
  # privileged daemon, no sign-in popup. The functions _ensure_sshd_running
  # and install_tailscale stay defined for any user who explicitly needs
  # them via opt-in flag, but the default install no longer invokes them.
  : "Phase 3c: skipping sshd + Tailscale (gh-as-bearer is the cross-network path)"

  # gh auth: required for the gist substrate. We CAN drive the login
  # interactively when stdin is a TTY (Joel 2026-04-29: 'thought that'd
  # be in setup or at least doctor (then claude could always do it for
  # them)'). The browser/device-code flow needs a real user to click
  # but doesn't need them to remember the command. Falls back to the
  # warning path for non-interactive installs (curl|bash piped without
  # a TTY, CI, etc).
  if command -v gh >/dev/null 2>&1; then
    if ! gh auth status >/dev/null 2>&1; then
      if [ -t 0 ] && [ -t 1 ]; then
        info "gh is not authenticated — launching 'gh auth login -s gist' now."
        info "  (Browser will open; sign in to GitHub. The 'gist' scope is required for the substrate.)"
        if gh auth login -h github.com -s gist; then
          ok "gh auth complete"
          # Re-run setup-git so the just-acquired token gets wired.
          gh auth setup-git 2>/dev/null && info "  gh token wired into git credential helper" || true
        else
          warn "gh auth login did not complete — re-run when ready:"
          warn "    gh auth login -h github.com -s gist"
        fi
      else
        warn "gh CLI is not authenticated. Run once before 'airc join':"
        warn "    gh auth login -h github.com -s gist"
        warn "  (interactive; can't run from a non-TTY install)"
      fi
    else
      # Wire gh's token into git's credential helper. Without this,
      # every git-over-HTTPS op (gist fetch/push -- airc's substrate
      # hot path) prompts the user for a password, repeatedly. gh ships
      # with `gh auth git-credential` for exactly this purpose; the
      # `gh auth setup-git` one-liner registers it in ~/.gitconfig.
      # Idempotent (no-op if already configured), safe to always run.
      # Joel hit this on 2026-04-28 — Windows install where gh was
      # auth'd-in-keyring but git itself didn't know. Resulted in a
      # GUI password popup every airc operation that touched a gist.
      if ! git config --global --get-all credential.https://github.com.helper 2>/dev/null | grep -q 'gh auth git-credential'; then
        if gh auth setup-git 2>/dev/null; then
          info "  gh token wired into git credential helper (no more password popups for gist ops)"
        fi
      fi
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

# Windows: also place airc.cmd + airc.ps1 forwarders on PATH.
# Without these, `airc` invoked from native PowerShell or cmd.exe
# resolves to the bash script, which PowerShell can't execute
# ("Cannot run a document in the middle of a pipeline"). PR #262
# made airc.ps1 a thin forwarder to bash, but that's moot if the
# .ps1 isn't on PATH. cp (not ln -sf) — Windows symlinks are
# privileged + flaky; copying is universal. Caught by
# continuum-b69f 2026-04-29 (issue #249 PowerShell row).
case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    [ -f "$CLONE_DIR/airc.cmd" ] && cp -f "$CLONE_DIR/airc.cmd" "$BIN_DIR/airc.cmd"
    [ -f "$CLONE_DIR/airc.ps1" ] && cp -f "$CLONE_DIR/airc.ps1" "$BIN_DIR/airc.ps1"
    ;;
esac

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

# ── Python venv with crypto deps (Phase E: envelope encryption) ────────
# airc's envelope-layer end-to-end encryption needs the cryptography
# package. We create a venv inside the install dir and pip-install
# there because PEP 668 makes `pip install --user` fail on managed
# Pythons (homebrew, system Python on Debian/Ubuntu/etc). The venv
# avoids touching system Python at all — fully self-contained.
#
# airc's bash wrapper detects this venv at AIRC_PYTHON resolution time
# and prefers it over system python3. If venv setup fails (no python3,
# pip module missing, network failure during install), airc falls back
# to system python3 and runs in plaintext mode. Per the "no scary
# popups" rule: pip-install never elevates, never prompts; failures
# print a non-fatal warning.
_airc_venv="$CLONE_DIR/.venv"
if [ ! -d "$_airc_venv" ] && command -v python3 >/dev/null 2>&1; then
  if python3 -m venv "$_airc_venv" 2>/dev/null; then
    ok "Python venv created: $_airc_venv"
  else
    warn "Could not create Python venv (python3-venv missing?). airc will run in plaintext mode."
  fi
fi
# Locate venv pip — POSIX vs Windows-Git-Bash paths.
_airc_venv_pip=""
if [ -x "$_airc_venv/bin/pip" ]; then
  _airc_venv_pip="$_airc_venv/bin/pip"
elif [ -x "$_airc_venv/Scripts/pip.exe" ]; then
  _airc_venv_pip="$_airc_venv/Scripts/pip.exe"
fi
if [ -n "$_airc_venv_pip" ]; then
  # Check if cryptography is already installed (idempotent install).
  _airc_venv_python_bin=""
  if [ -x "$_airc_venv/bin/python" ]; then
    _airc_venv_python_bin="$_airc_venv/bin/python"
  elif [ -x "$_airc_venv/Scripts/python.exe" ]; then
    _airc_venv_python_bin="$_airc_venv/Scripts/python.exe"
  fi
  if [ -n "$_airc_venv_python_bin" ] && \
     "$_airc_venv_python_bin" -c "import cryptography" >/dev/null 2>&1; then
    : # already installed; skip
  else
    if "$_airc_venv_pip" install -q --upgrade pip >/dev/null 2>&1; then : ; fi
    if "$_airc_venv_pip" install -q cryptography 2>&1 | tail -3; then
      ok "cryptography installed in venv (envelope encryption ready)"
    else
      warn "cryptography install failed; airc will run in plaintext mode"
      warn "  Manual fix:  $_airc_venv_pip install cryptography"
    fi
  fi
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
    [ -n "$_wherewin" ] && ts_bin=$(_to_bash_path "$_wherewin")
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

# Phase 3c: ts_post_check call removed. Tailscale is no longer used for
# cross-network messaging — gh-as-bearer (envelope-encrypted gist) is
# the universal path. Function definitions remain for any opt-in user.

# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
echo "  Next:"
echo "    1. gh auth login -s gist          # one-time, browser flow"
echo "    2. airc join                      # auto-#general (joins existing or hosts)"
echo "    3. airc msg @<peer> <message>     # DM (or omit @peer to broadcast)"
echo ""
echo "  Diagnose anytime:    airc doctor"
echo ""
