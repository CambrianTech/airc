# Sourced by airc. cmd_doctor + all _doctor_* helpers +
# _doctor_run_tests. Self-contained — uses helpers (die,
# detect_platform, get_config_val) defined in airc top-level
# but exposes no functions outside the doctor surface.
# Extracted from airc as part of #152 Phase 3 file split.

cmd_doctor() {
  # Three modes:
  #   airc doctor           -- environment health check (default).
  #                            Probes each prereq and prints the exact
  #                            install command for whichever package
  #                            manager this platform uses, so any AI
  #                            reading the output can `proactively fix
  #                            recoverable issues` (per /doctor SKILL.md).
  #   airc doctor --connect -- pre-flight before `airc connect`. Runs
  #                            the default health probes PLUS connect-
  #                            specific checks (tailscale UP not just
  #                            installed, gist API reachable, port free,
  #                            cached host_target reachable). Issue #80.
  #                            Use case: airc doctor --connect && airc connect
  #   airc doctor --tests   -- run the integration test suite (the
  #   airc doctor tests        prior default behavior; aliased on the
  #                            dispatch via `tests|test`).
  case "${1:-}" in
    -h|--help)
      echo "Usage: airc doctor [mode]"
      echo "  airc doctor              environment health check (default)"
      echo "  airc doctor --fix        attempt to repair recoverable issues"
      echo "                           (currently: gh auth re-login if invalid)"
      echo "  airc doctor --connect    pre-flight checks for 'airc connect'"
      echo "  airc doctor --tests      run the integration test suite"
      echo "                           (aliases: tests, test, run, suite)"
      return 0 ;;
    --tests|-t|tests|test|run|suite) shift; _doctor_run_tests "$@"; return ;;
    --connect|-c|connect)            shift; _doctor_connect_preflight "$@"; return ;;
    --fix|fix)                       shift; _doctor_fix "$@"; return ;;
  esac

  echo ""
  echo "  airc doctor -- environment health"
  echo "  --------------------------------"
  echo ""
  local issues=0

  # Detect the platform's package manager so we can emit concrete fix
  # commands. Same shape as install.sh's ensure_prereqs.
  local mgr; mgr=$(_doctor_detect_pkgmgr)

  _doctor_probe "git"          "$mgr" "VCS for clone/update" || issues=$((issues+1))
  _doctor_probe "gh"           "$mgr" "Gist substrate (room discovery)" || issues=$((issues+1))
  _doctor_probe_gh_auth                                             || issues=$((issues+1))
  _doctor_probe "ssh"          "$mgr" "OpenSSH client for the wire"     || issues=$((issues+1))
  _doctor_probe "ssh-keygen"   "$mgr" "Identity keypair generation"     || issues=$((issues+1))
  _doctor_probe "python3"      "$mgr" "Monitor formatter + heredocs"    || issues=$((issues+1))
  _doctor_probe_cryptography                                            || issues=$((issues+1))
  _doctor_probe_sshd                                                    || issues=$((issues+1))
  _doctor_probe_tailscale "$mgr"  # optional, never increments issues

  echo ""
  echo "  Scope:"
  echo "    AIRC_HOME = $AIRC_WRITE_DIR"
  if [ -f "$CONFIG" ]; then
    local _name; _name=$(get_name)
    local _ht;   _ht=$(get_config_val host_target "")
    if [ -n "$_ht" ]; then
      echo "    Identity: $_name (joiner of $_ht)"
    else
      echo "    Identity: $_name (host or unconnected)"
    fi
  else
    echo "    Identity: not initialized (run 'airc join' to set up)"
  fi

  echo ""
  if [ "$issues" -eq 0 ]; then
    echo "  All required prereqs present. Behavioral suite:  airc doctor --tests"
  else
    echo "  $issues prereq(s) missing -- see fix lines above."
    echo "  Fastest path: re-run install.sh (auto-installs via brew/apt/dnf/pacman/apk):"
    echo "    curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash"
  fi
  echo ""
}

_doctor_detect_pkgmgr() {
  case "$(uname -s 2>/dev/null)" in
    Darwin)
      command -v brew >/dev/null 2>&1 && { echo "brew"; return; }
      echo "brew-missing"; return ;;
    Linux)
      command -v apt-get >/dev/null 2>&1 && { echo "apt";    return; }
      command -v dnf     >/dev/null 2>&1 && { echo "dnf";    return; }
      command -v pacman  >/dev/null 2>&1 && { echo "pacman"; return; }
      command -v apk     >/dev/null 2>&1 && { echo "apk";    return; }
      ;;
  esac
  echo "unknown"
}

# Map a generic prereq to the install command for the detected pkgmgr.
# Empty string = we don't have a one-liner to suggest; emits a generic
# pointer instead. Mirrors install.sh:pkgname_for + install_with_pkgmgr.
_doctor_install_cmd_for() {
  local mgr="$1" prereq="$2"
  local pkg
  case "$prereq" in
    ssh|ssh-keygen)
      case "$mgr" in
        brew)   pkg="openssh" ;;
        apt)    pkg="openssh-client" ;;
        dnf)    pkg="openssh-clients" ;;
        pacman) pkg="openssh" ;;
        apk)    pkg="openssh-client" ;;
      esac ;;
    python3)
      case "$mgr" in
        pacman) pkg="python" ;;
        *)      pkg="python3" ;;
      esac ;;
    *) pkg="$prereq" ;;
  esac
  case "$mgr" in
    brew)   echo "brew install $pkg" ;;
    apt)    echo "sudo apt-get install -y $pkg" ;;
    dnf)    echo "sudo dnf install -y $pkg" ;;
    pacman) echo "sudo pacman -S --needed $pkg" ;;
    apk)    echo "sudo apk add $pkg" ;;
    brew-missing)
      echo "Install Homebrew first: /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\", then: brew install $pkg" ;;
    *) echo "Install '$pkg' via your platform's package manager" ;;
  esac
}

_doctor_probe() {
  local cmd="$1" mgr="$2" purpose="$3"
  # Strict-probe ONLY the binaries that have known shadow-aliases on
  # Windows. PR #153's blanket strict-probe broke on macOS BSD utilities
  # — `ssh-keygen --version` exits 1 ("illegal option") because BSD
  # doesn't accept --version, and there's no portable single-flag that
  # discriminates "real ssh-keygen" from "stub" anyway. Only the
  # Microsoft Store {python.exe, python3.exe} aliases need defense
  # against; everything else is uniquely shipped by the user's package
  # manager (no shadowing ambiguity), so bare `command -v` is correct.
  case "$cmd" in
    python|python3)
      if command -v "$cmd" >/dev/null 2>&1 && "$cmd" --version >/dev/null 2>&1; then
        printf "  [ok] %s\n" "$cmd"
        return 0
      fi
      ;;
    *)
      if command -v "$cmd" >/dev/null 2>&1; then
        printf "  [ok] %s\n" "$cmd"
        return 0
      fi
      ;;
  esac
  # Distinguish "absent" from "stub on PATH" so the fix hint is correct.
  local fix
  if command -v "$cmd" >/dev/null 2>&1; then
    # Present but non-functional — almost certainly a stub.
    printf "  [BROKEN] %s -- %s\n" "$cmd" "$purpose"
    printf "         '%s' is on PATH but '%s --version' fails. " "$cmd" "$cmd"
    printf "Likely a Microsoft Store alias on Windows.\n"
    printf "         Disable: Settings -> Apps -> Advanced app settings -> App execution aliases\n"
    printf "         Or PATH-prepend a real install ahead of WindowsApps/.\n"
    fix=$(_doctor_install_cmd_for "$mgr" "$cmd")
    printf "         Or install fresh: %s\n" "$fix"
  else
    fix=$(_doctor_install_cmd_for "$mgr" "$cmd")
    printf "  [MISSING] %s -- %s\n" "$cmd" "$purpose"
    printf "         Fix: %s\n" "$fix"
  fi
  return 1
}

_doctor_probe_gh_auth() {
  if ! command -v gh >/dev/null 2>&1; then
    return 0  # already reported missing by the gh probe
  fi
  if gh auth status >/dev/null 2>&1; then
    printf "  [ok] gh authenticated\n"
    return 0
  fi
  printf "  [MISSING] gh authenticated (gist scope)\n"
  printf "         Fix: gh auth login -s gist\n"
  return 1
}

# Probe the venv cryptography package — issue #341 follow-up. airc's
# Ed25519 identity gen + signing now route through python-cryptography;
# without it init_identity / sign_message hard-fail. install.sh's venv
# step pip-installs it, so the failure surface here is "venv setup
# didn't complete cleanly" or "the system python the resolver picked
# differs from the venv one". Either way: surface clearly so doctor
# tells the user to re-run install.sh.
_doctor_probe_cryptography() {
  if ! command -v "${AIRC_PYTHON:-python3}" >/dev/null 2>&1; then
    return 0  # already reported missing by the python3 probe
  fi
  if "${AIRC_PYTHON:-python3}" -c "import cryptography.hazmat.primitives.asymmetric.ed25519" >/dev/null 2>&1; then
    printf "  [ok] cryptography (Ed25519 identity gen + signing)\n"
    return 0
  fi
  printf "  [MISSING] cryptography (Python package, used for Ed25519 identity)\n"
  printf "         Fix: re-run install.sh (sets up the venv with cryptography)\n"
  return 1
}

# Probe sshd (SSH server). airc joiners ssh into the host's airc_home
# to `tail -F messages.jsonl`. So every airc user who'll host a room
# (which is most users — first to discover a room becomes its host)
# needs sshd running on their box. Pre-fix: airc doctor probed for the
# ssh CLIENT but not the SERVER. Joel + continuum-b69f hit this on
# 2026-04-27 mid-cross-machine bringup: TCP handshake worked, but
# message stream silently failed because Windows ships OpenSSH client
# but NOT the server enabled by default.
#
# Per-platform probes:
#   macOS         — launchctl + systemsetup (Remote Login)
#   linux / wsl   — systemctl is-active on ssh OR sshd unit names
#                   (Debian/Ubuntu unit is 'ssh', RHEL/Fedora is 'sshd')
#   windows-bash  — powershell.exe Get-Service sshd, distinguish
#                   Running / Stopped / Missing-capability
#
# Returns 0 on ok, 1 on missing/broken, 0 on platforms we can't probe
# (don't penalize if we can't tell).
_doctor_probe_sshd() {
  local plat; plat=$(detect_platform)
  case "$plat" in
    darwin)
      # macOS Remote Login = launchd-managed sshd. Detect WITHOUT sudo:
      #   - `launchctl list` (user scope) does NOT show system services
      #     like com.openssh.sshd, so the user-scope probe always misses.
      #   - `launchctl print system` DOES list system services and works
      #     without sudo. Look for `com.openssh.sshd` (the service id).
      #   - `systemsetup -getremotelogin` requires admin to read state
      #     (returns "You need administrator access..." otherwise) — keep
      #     it as the second-attempt fallback in case sudo is cached.
      if launchctl print system 2>/dev/null | grep -qE 'com\.openssh\.sshd($|[[:space:]])'; then
        printf "  [ok] sshd (Remote Login enabled)\n"
        return 0
      fi
      if systemsetup -getremotelogin 2>/dev/null | grep -qi "Remote Login: On"; then
        printf "  [ok] sshd (Remote Login enabled)\n"
        return 0
      fi
      printf "  [MISSING] sshd -- needed when you HOST a room\n"
      printf "         Fix: System Settings -> General -> Sharing -> Remote Login (toggle on)\n"
      printf "         Or:  sudo systemsetup -setremotelogin on\n"
      return 1
      ;;
    linux|wsl)
      # Debian/Ubuntu uses 'ssh', RHEL/Fedora/Arch uses 'sshd'.
      if systemctl is-active --quiet ssh 2>/dev/null || systemctl is-active --quiet sshd 2>/dev/null; then
        printf "  [ok] sshd (systemd active)\n"
        return 0
      fi
      printf "  [MISSING] sshd -- needed when you HOST a room\n"
      printf "         Fix (Debian/Ubuntu): sudo apt-get install openssh-server && sudo systemctl enable --now ssh\n"
      printf "         Fix (RHEL/Fedora):    sudo dnf install openssh-server && sudo systemctl enable --now sshd\n"
      return 1
      ;;
    windows)
      # powershell.exe is the canonical PS launcher in Git Bash. Some
      # boxes also ship pwsh.exe (PS Core); prefer powershell.exe for
      # broadest reach since OpenSSH service control works in both.
      local _ps=""
      if command -v powershell.exe >/dev/null 2>&1; then _ps="powershell.exe"
      elif command -v pwsh.exe >/dev/null 2>&1; then _ps="pwsh.exe"
      fi
      if [ -z "$_ps" ]; then
        printf "  [info] sshd probe skipped (powershell.exe not on PATH)\n"
        return 0
      fi
      local _state
      _state=$("$_ps" -NoProfile -Command "(Get-Service sshd -ErrorAction SilentlyContinue).Status" 2>/dev/null | tr -d '\r\n ')
      case "$_state" in
        Running)
          printf "  [ok] sshd (Windows OpenSSH.Server running)\n"
          return 0
          ;;
        Stopped|StopPending|StartPending|Paused)
          printf "  [BROKEN] sshd -- installed but not running (state: %s)\n" "$_state"
          printf "         Fix (admin PowerShell):  Start-Service sshd; Set-Service sshd -StartupType Automatic\n"
          return 1
          ;;
        "")
          printf "  [MISSING] sshd -- needed when you HOST a room\n"
          printf "         Fix (admin PowerShell — five lines, run all together):\n"
          printf "           Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0\n"
          printf "           reg add HKLM\\\\SYSTEM\\\\CurrentControlSet\\\\Services\\\\hns\\\\State /v EnableExcludedPortRange /d 0 /f\n"
          printf "           netsh int ipv4 add excludedportrange protocol=tcp startport=22 numberofports=1\n"
          printf "           Start-Service sshd\n"
          printf "           Set-Service -Name sshd -StartupType Automatic\n"
          printf "         (The reg+netsh lines work around Windows HNS holding port 22 randomly per boot —\n"
          printf "          continuum-b69f's diagnosis 2026-04-27. Without them, sshd bind returns EPERM.)\n"
          return 1
          ;;
        *)
          printf "  [info] sshd state unknown (Get-Service returned: '%s')\n" "$_state"
          return 0
          ;;
      esac
      ;;
    *)
      printf "  [info] sshd probe unsupported on platform '%s'\n" "$plat"
      return 0
      ;;
  esac
}

_doctor_probe_tailscale() {
  local mgr="$1"
  # Use resolve_tailscale_bin so we find macOS GUI-installed Tailscale.app
  # (the binary lives at /Applications/Tailscale.app/Contents/MacOS/Tailscale,
  # not on PATH by default). Bare `command -v tailscale` false-negatives
  # on every Mac that installed via the App Store / dmg — caught live
  # 2026-04-27 when Mac doctor said "tailscale not installed" while
  # airc was actively publishing a Tailscale IP from the running app.
  local _ts_bin
  _ts_bin=$(resolve_tailscale_bin 2>/dev/null || true)
  if [ -n "$_ts_bin" ]; then
    if "$_ts_bin" status >/dev/null 2>&1; then
      printf "  [ok] tailscale (optional) -- daemon up\n"
    else
      printf "  [info] tailscale (optional) -- installed but daemon not up\n"
      printf "         Bring up:  tailscale up    (or skip; LAN mesh works without it)\n"
    fi
    return 0
  fi
  # Optional -- print the install hint but don't count toward issues.
  local fix
  case "$mgr" in
    brew)         fix="brew install --cask tailscale" ;;
    apt|dnf|pacman|apk) fix="curl -fsSL https://tailscale.com/install.sh | sh" ;;
    *)            fix="https://tailscale.com/download" ;;
  esac
  printf "  [info] tailscale (optional) -- not installed; only needed for cross-LAN mesh\n"
  printf "         Install: %s\n" "$fix"
  return 0
}

_doctor_connect_preflight() {
  # Pre-flight check before `airc connect`. Issue #80. Runs the default
  # prereq probes PLUS connect-specific checks. Output is a checklist
  # with fix commands; exit non-zero if any blocking issue. Use case:
  #
  #   airc doctor --connect && airc connect
  #
  # Catches the silent-fail classes that produced #78 / #85 / #79
  # cascades for first-time users and surfaced as detective-work bugs.
  echo ""
  echo "  airc doctor --connect -- pre-flight checks"
  echo "  ------------------------------------------"
  echo ""
  local issues=0
  local mgr; mgr=$(_doctor_detect_pkgmgr)

  # ── Required prereqs (same as default doctor) ──
  _doctor_probe "git"          "$mgr" "VCS for clone/update"           || issues=$((issues+1))
  _doctor_probe "ssh"          "$mgr" "OpenSSH client for the wire"    || issues=$((issues+1))
  _doctor_probe "ssh-keygen"   "$mgr" "Identity keypair generation"    || issues=$((issues+1))
  _doctor_probe "python3"      "$mgr" "Monitor formatter + heredocs"   || issues=$((issues+1))
  _doctor_probe_cryptography                                           || issues=$((issues+1))
  _doctor_probe_sshd                                                   || issues=$((issues+1))

  # ── gh chain: installed → authed → gist scope → gists API reachable.
  # Single chain (early-return on first failure) so a missing gh isn't
  # counted 3-4x as a separate issue per dependent probe. Gist scope is
  # checked explicitly because `gh auth status` alone passes for a
  # gist-scope-less token (Copilot caught this on #87 review).
  if ! _doctor_probe "gh" "$mgr" "Gist substrate (room discovery)"; then
    issues=$((issues+1))
  elif ! gh auth status >/dev/null 2>&1; then
    # Distinguish a real auth failure from a GitHub secondary rate limit
    # (abuse detection). The /rate_limit endpoint is reachable during
    # secondary limits, so if it works, the token is fine — the user just
    # needs to wait. `gh auth status` probes /user, which gets 403'd, and
    # gh then misreports the symptom as 'token invalid'. Issue #341.
    if gh api rate_limit >/dev/null 2>&1; then
      printf "  [BLOCKED] gh secondary rate limit (abuse detection) — token is fine\n"
      printf "         Fix: wait 5-15 min then re-run; cause is too many gh API calls in a short window\n"
    else
      printf "  [BLOCKED] gh authenticated\n"
      printf "         Fix: gh auth login -s gist\n"
    fi
    issues=$((issues+1))
  elif ! gh auth status 2>&1 | grep -qiE '(scopes|token scopes):.*\bgist\b'; then
    printf "  [BLOCKED] gh authed but missing 'gist' scope (room substrate needs it)\n"
    printf "         Fix: gh auth refresh -s gist\n"
    issues=$((issues+1))
  elif ! gh api 'gists?per_page=1' >/dev/null 2>&1; then
    # Same misdiagnosis risk here — distinguish rate-limit vs other.
    if gh api rate_limit >/dev/null 2>&1; then
      printf "  [BLOCKED] gh secondary rate limit (abuse detection) — token + scope are fine\n"
      printf "         Fix: wait 5-15 min then re-run\n"
    else
      printf "  [BLOCKED] gist API not reachable -- network outage or token revoked\n"
      printf "         Fix: check internet; if persistent, run 'gh auth refresh'\n"
    fi
    issues=$((issues+1))
  else
    printf "  [ok] gh authed with gist scope, gists API reachable\n"
  fi

  # ── Connect-specific: tailscale state. The default doctor only marks
  # tailscale as "info" since it's optional for LAN-only mesh. In
  # --connect mode, if there's a saved host_target in tailnet CGNAT
  # range, Tailscale being UP is a HARD requirement.
  local prior_host_target=""
  [ -f "$CONFIG" ] && prior_host_target=$(get_config_val host_target "")
  local prior_host_only="${prior_host_target##*@}"
  local target_is_cgnat=0
  case "$prior_host_only" in
    100.6[4-9].*|100.[7-9][0-9].*|100.1[01][0-9].*|100.12[0-7].*) target_is_cgnat=1 ;;
  esac
  if [ "$target_is_cgnat" = "1" ]; then
    # Use resolve_tailscale_bin so the .app-bundle / Program Files paths
    # are checked, not just PATH (consistency with the rest of airc).
    local ts_bin; ts_bin=$(resolve_tailscale_bin 2>/dev/null || true)
    if [ -n "$ts_bin" ]; then
      if "$ts_bin" status >/dev/null 2>&1; then
        printf "  [ok] tailscale UP (cached host_target is tailnet CGNAT)\n"
      else
        printf "  [BLOCKED] tailscale CLI installed but DOWN -- cached host is tailnet, can't reach\n"
        printf "         Fix: tailscale up\n"
        issues=$((issues+1))
      fi
    else
      printf "  [BLOCKED] tailscale CLI missing -- cached host is tailnet, can't reach\n"
      printf "         Fix: install tailscale (https://tailscale.com/download), then 'tailscale up'\n"
      issues=$((issues+1))
    fi
  else
    _doctor_probe_tailscale "$mgr"  # optional, info-only
  fi

  # ── Connect-specific: AIRC_PORT free or auto-shift available ──
  local target_port="${AIRC_PORT:-7547}"
  if [ -n "$(port_listeners "$target_port")" ]; then
    printf "  [info] port %s busy -- airc will auto-shift to next free port\n" "$target_port"
  else
    printf "  [ok] port %s available for hosting\n" "$target_port"
  fi

  # ── Connect-specific: cached host_target reachable (resume scenario) ──
  if [ -n "$prior_host_target" ]; then
    local probe_key="$IDENTITY_DIR/ssh_key"
    if [ -f "$probe_key" ]; then
      if ssh -i "$probe_key" -o StrictHostKeyChecking=accept-new \
              -o ConnectTimeout=3 -o BatchMode=yes \
              "$prior_host_target" "echo __PROBE_OK__" 2>/dev/null | grep -q __PROBE_OK__; then
        printf "  [ok] cached host %s reachable + auth works\n" "$prior_host_target"
      else
        printf "  [warn] cached host %s not reachable -- may need re-pair\n" "$prior_host_target"
        printf "         Fix: airc teardown --flush && airc join (fresh pairing)\n"
        # Not blocking — fresh-pair flow handles this
      fi
    fi
  fi

  echo ""
  if [ "$issues" -eq 0 ]; then
    echo "  ✓ READY -- airc connect should work."
    return 0
  else
    echo "  ✗ BLOCKED on $issues issue(s) -- fix the items above before 'airc connect'."
    return 1
  fi
}

_doctor_fix() {
  # Attempt to repair recoverable issues. Currently scoped to gh auth
  # because that's the highest-impact silent-failure mode (Joel
  # 2026-04-29 — token expired, every gh API call failed silently,
  # peers froze). Future fixes can be added here as discrete recovery
  # paths.
  echo
  echo "  airc doctor --fix"
  echo "  -----------------"
  local fixed=0 skipped=0 failed=0

  # gh auth: if invalid, re-run gh auth login. Needs a TTY for the
  # browser/device-code flow.
  if command -v gh >/dev/null 2>&1; then
    if gh auth status >/dev/null 2>&1; then
      echo "  [skip] gh auth already valid"
      skipped=$((skipped + 1))
    elif [ -t 0 ] && [ -t 1 ]; then
      echo "  [fix]  gh auth invalid — running 'gh auth login -h github.com -s gist'"
      if gh auth login -h github.com -s gist; then
        echo "  [ok]   gh auth restored"
        # Re-wire git credential helper while we have the token.
        gh auth setup-git 2>/dev/null && echo "  [ok]   gh token wired into git credential helper" || true
        fixed=$((fixed + 1))
      else
        echo "  [FAIL] gh auth login did not complete; re-run when ready"
        failed=$((failed + 1))
      fi
    else
      echo "  [FAIL] gh auth invalid AND no TTY for the interactive login"
      echo "         Run from a real shell:  gh auth login -h github.com -s gist"
      failed=$((failed + 1))
    fi
  else
    echo "  [skip] gh CLI not installed (separate fix — install via brew/apt/winget)"
    skipped=$((skipped + 1))
  fi

  echo
  echo "  Summary: $fixed fixed, $skipped skipped, $failed failed."
  [ "$failed" = "0" ]
}

_doctor_run_tests() {
  # Behavioral suite -- the prior cmd_doctor entry point. Kept reachable
  # via `airc doctor --tests` (or the `tests`/`test` aliases in dispatch)
  # so existing CI / muscle memory still works.
  case "${1:-}" in
    -h|--help)
      echo "Usage:"
      echo "  airc tests              run the full integration suite"
      echo "  airc tests <scenario>   run one scenario (see test/integration.sh)"
      echo "  airc doctor --tests     same as 'airc tests'"
      return 0 ;;
  esac
  local script="${AIRC_DIR:-$HOME/.airc-src}/test/integration.sh"
  if [ ! -x "$script" ]; then
    local self; self="$(realpath "$0" 2>/dev/null || echo "$0")"
    local here; here="$(dirname "$self")"
    [ -x "$here/test/integration.sh" ] && script="$here/test/integration.sh"
  fi
  [ -x "$script" ] || die "Can't find test script. Expected at \$AIRC_DIR/test/integration.sh"
  exec bash "$script" "$@"
}
