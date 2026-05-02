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
      echo "  airc doctor --health     LIVE bus health (after join — daemon, gh"
      echo "                           rate-limit headroom, channel last-recv age)"
      echo "  airc doctor --tests      run the integration test suite"
      echo "                           (aliases: tests, test, run, suite)"
      return 0 ;;
    --tests|-t|tests|test|run|suite) shift; _doctor_run_tests "$@"; return ;;
    --connect|-c|connect)            shift; _doctor_connect_preflight "$@"; return ;;
    --health|-H|health)              shift; _doctor_health "$@"; return ;;
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
  # sshd probe removed post-3c: the gist IS the wire for ALL peers; airc no
  # longer ssh's into the host's airc_home. ssh-keygen above stays (identity
  # key generation), ssh client stays (occasional manual diagnostic + future
  # wire-pluggable bearers). Issue #341 follow-up.
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
  # sshd probe removed post-3c — see cmd_doctor() in this file for rationale.

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

_doctor_health() {
  # LIVE bus-health probe — answers "is my bus actively working RIGHT NOW?"
  # Complements --connect (pre-flight, before join) with post-join checks
  # against the running substrate. Joel 2026-05-02: "maybe doctor can test /
  # so doctor could check the connection health". Surfaces the silent-
  # blackout failure modes that bit us through the bios-hardening sprint —
  # bearer falling behind, daemon crashed, gh rate-limit eating throughput.
  echo
  echo "  airc doctor --health -- live bus health"
  echo "  ---------------------------------------"
  echo
  local issues=0 warns=0
  local now; now=$(date +%s 2>/dev/null || echo 0)

  # ── gh API headroom (rate-limit). Cheap; reveals whether we're near
  # the cliff that wedged the bus pre-#416/#419.
  if command -v gh >/dev/null 2>&1; then
    local rate_json
    if rate_json=$(gh api rate_limit 2>/dev/null); then
      local core_remaining core_limit
      core_remaining=$(echo "$rate_json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['resources']['core']['remaining'])" 2>/dev/null || echo "")
      core_limit=$(echo "$rate_json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['resources']['core']['limit'])" 2>/dev/null || echo "")
      if [ -n "$core_remaining" ] && [ -n "$core_limit" ]; then
        if [ "$core_remaining" -lt 100 ]; then
          printf "  [WARN] gh core rate-limit: %s/%s remaining — bus may stall soon\n" "$core_remaining" "$core_limit"
          printf "         Mitigation: bearer auto-throttles (#416); peers will resume when window resets\n"
          warns=$((warns+1))
        elif [ "$core_remaining" -lt 1000 ]; then
          printf "  [info] gh core rate-limit: %s/%s remaining (healthy headroom)\n" "$core_remaining" "$core_limit"
        else
          printf "  [ok] gh core rate-limit: %s/%s remaining\n" "$core_remaining" "$core_limit"
        fi
      else
        printf "  [info] gh rate_limit reachable but parse failed (skipping)\n"
      fi
    else
      printf "  [BLOCKED] gh API not reachable — can't probe rate-limit\n"
      printf "         Fix: airc doctor (full env probe will diagnose)\n"
      issues=$((issues+1))
    fi
  fi

  # ── Daemon installed-for-this-scope check. Pre-fix probed for a
  # `daemon.pid` file that the daemon launcher never writes anywhere
  # (Copilot caught this on PR #422 review — `--health` always reported
  # "not installed" even when the daemon was running). Use the canonical
  # detector (`airc_daemon_is_installed_for_scope`) which checks the
  # registered launchd plist / systemd unit / HKCU Run entry. Liveness
  # itself (is the launcher actually running and successfully polling?)
  # is what the per-channel bearer last-recv timestamps below measure
  # transitively — if the daemon is installed AND bearer last-recv is
  # fresh, the daemon is alive. Fresh state with no installed daemon =
  # an interactive `airc connect` is doing the work.
  if command -v airc_daemon_is_installed_for_scope >/dev/null 2>&1 \
     && airc_daemon_is_installed_for_scope "$AIRC_WRITE_DIR" 2>/dev/null; then
    printf "  [ok] daemon installed for this scope (liveness inferred from per-channel last-recv below)\n"
  else
    printf "  [info] daemon not installed (substrate runs in-shell only)\n"
    printf "         Optional: airc daemon install  (survives sleep/crash, see README → Optional layers)\n"
  fi

  # ── Per-channel bearer health. bearer_state.<channel>.json's last_recv_ts
  # is the heartbeat — if it's > 5min stale, the bearer is wedged and the
  # AI session is going dormant on that channel.
  #
  # Scope to subscribed_channels ONLY (Codex's first-run report 2026-05-02
  # exposed this — same fix-shape as #406's beacon scoping). Pre-fix the
  # probe globbed every bearer_state.*.json on disk INCLUDING stale files
  # from prior subscriptions (a #cambriantech the user parted, an old
  # qa-foo room from a previous test, etc.). Codex correctly identified
  # the noise: "sees stale bearer-state files for older channels". Real
  # fix is to intersect with the current subscribed_channels list — same
  # principle as bearer scoping in the receive-silence beacon.
  local _subs=""
  if [ -f "$CONFIG" ] && command -v "$AIRC_PYTHON" >/dev/null 2>&1; then
    _subs=$("$AIRC_PYTHON" -m airc_core.config read_channels --config "$CONFIG" 2>/dev/null || true)
  fi
  local found_state=0
  if [ -d "$AIRC_WRITE_DIR" ]; then
    for state_file in "$AIRC_WRITE_DIR"/bearer_state.*.json; do
      [ -f "$state_file" ] || continue
      local channel; channel=$(basename "$state_file" .json | sed 's/^bearer_state\.//')
      # Skip stale files for channels we no longer subscribe to.
      # Empty _subs (legacy scope without subscribed_channels populated)
      # falls back to checking everything — preserves old behavior on
      # uninitialized scopes.
      if [ -n "$_subs" ] && ! printf '%s\n' "$_subs" | grep -qFx "$channel"; then
        continue
      fi
      found_state=1
      local last_recv_ts
      last_recv_ts=$(python3 -c "import sys,json; d=json.load(open('$state_file')); print(int(d.get('last_recv_ts',0)))" 2>/dev/null || echo 0)
      if [ "$last_recv_ts" = "0" ]; then
        printf "  [WARN] #%s — bearer state has no last_recv_ts (never received?)\n" "$channel"
        warns=$((warns+1))
      else
        local age=$((now - last_recv_ts))
        if [ "$age" -lt 60 ]; then
          printf "  [ok] #%s — last bearer recv %ds ago (healthy)\n" "$channel" "$age"
        elif [ "$age" -lt 300 ]; then
          printf "  [info] #%s — last bearer recv %ds ago (idle channel?)\n" "$channel" "$age"
        elif [ "$age" -lt 1800 ]; then
          printf "  [WARN] #%s — last bearer recv %ds ago (>5min stale; check daemon/rate-limit)\n" "$channel" "$age"
          warns=$((warns+1))
        else
          printf "  [BLOCKED] #%s — last bearer recv %ds ago (>30min — bearer is wedged)\n" "$channel" "$age"
          printf "           Fix: airc teardown && airc join  (re-establishes bearer poll loop)\n"
          issues=$((issues+1))
        fi
      fi
    done
  fi
  if [ "$found_state" = "0" ]; then
    printf "  [info] no bearer state files — not joined to any channel yet\n"
    printf "         Fix: airc join  (then re-run airc doctor --health)\n"
  fi

  echo
  if [ "$issues" -eq 0 ] && [ "$warns" -eq 0 ]; then
    echo "  ✓ Bus healthy."
    return 0
  elif [ "$issues" -eq 0 ]; then
    echo "  ⚠ Bus working, $warns warning(s) above worth a look."
    return 0
  else
    echo "  ✗ Bus DEGRADED on $issues issue(s) ($warns warning(s)) — see fixes above."
    return 1
  fi
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
