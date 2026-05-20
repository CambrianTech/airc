# Sourced by airc + install.sh. Cross-platform "is the airc background
# daemon installed?" detector — single source of truth shared between
# the bootstrap installer and the runtime command surfaces.
#
# Why this exists: pre-fix, three places had near-identical detection
# logic that drifted:
#
#   - cmd_daemon.sh::_daemon_installed   (covers darwin, linux/wsl, windows)
#   - cmd_connect.sh first-host tip      (covered ONLY darwin + linux)
#   - install.sh::_daemon_already_installed (covered ONLY darwin + linux)
#
# Copilot review on PR #388 caught the install.sh + cmd_connect.sh gaps
# — the daemon-install prompt would re-prompt every install on Windows
# Git Bash even after `airc daemon install` had registered the HKCU
# Run-key entry, and the first-host tip never fired on Windows at all.
# Joel's "modular not duplicated" rule applies: ONE detect, called from
# every site that asks "is the daemon installed?".
#
# Depends on: detect_platform (lib/airc_bash/platform_adapters.sh).
# install.sh sources both files explicitly from $CLONE_DIR before
# calling this; runtime sources them via airc's lib-dir resolver.

airc_daemon_scope_id() {
  local target_scope="${1:-}"
  [ -n "$target_scope" ] || target_scope="${AIRC_HOME:-$HOME/.airc}"
  local id=""
  if command -v airc_core_bin >/dev/null 2>&1; then
    id=$("$(airc_core_bin)" daemon-scope-id "$target_scope" 2>/dev/null || true)
  elif command -v airc-core >/dev/null 2>&1; then
    id=$(airc-core daemon-scope-id "$target_scope" 2>/dev/null || true)
  fi
  if [ -z "$id" ] && command -v sha1sum >/dev/null 2>&1; then
    id=$(printf '%s' "$target_scope" | sha1sum 2>/dev/null | awk '{print substr($1,1,12)}')
  fi
  if [ -z "$id" ] && command -v shasum >/dev/null 2>&1; then
    id=$(printf '%s' "$target_scope" | shasum 2>/dev/null | awk '{print substr($1,1,12)}')
  fi
  if [ -z "$id" ] && command -v openssl >/dev/null 2>&1; then
    id=$(printf '%s' "$target_scope" | openssl dgst -sha1 2>/dev/null | awk '{print substr($NF,1,12)}')
  fi
  if [ -z "$id" ]; then
    id=$(printf '%s' "$target_scope" | tr -c 'A-Za-z0-9' '_' | tail -c 12)
  fi
  printf '%s\n' "${id:-aircdefault0}"
}

airc_daemon_service_name_for_scope() {
  local target_scope="${1:-}"
  printf 'com.cambriantech.airc.%s\n' "$(airc_daemon_scope_id "$target_scope")"
}

airc_daemon_unit_name_for_scope() {
  local target_scope="${1:-}"
  printf 'airc-%s.service\n' "$(airc_daemon_scope_id "$target_scope")"
}

airc_daemon_run_entry_for_scope() {
  local target_scope="${1:-}"
  printf 'airc-monitor-%s\n' "$(airc_daemon_scope_id "$target_scope")"
}

# ── airc_daemon_is_installed — yes/no probe across all supported OSes ──
#
# Returns:
#   0 — daemon autostart entry IS installed for the current user
#   1 — daemon entry NOT installed (or unsupported platform)
#
# Detection strategy by platform:
#   darwin    — $HOME/Library/LaunchAgents/com.cambriantech.airc.plist
#   linux/wsl — $HOME/.config/systemd/user/airc.service
#   windows   — HKCU\Software\Microsoft\Windows\CurrentVersion\Run
#               (Run value name: airc-monitor; matches the entry name
#               cmd_daemon.sh's installer creates)
#
# This MUST stay aligned with cmd_daemon.sh::cmd_daemon_install — if
# the installer ever changes the path / unit name / entry name, this
# detector is what tells the install-time + first-host UX whether the
# offer/tip should fire. Misalignment = re-prompt loop or never-prompt
# silent miss; both are user experience bugs Copilot flagged.
airc_daemon_is_installed() {
  local os; os=$(detect_platform)
  case "$os" in
    darwin)
      ls "$HOME"/Library/LaunchAgents/com.cambriantech.airc*.plist >/dev/null 2>&1 && return 0 ;;
    linux|wsl)
      ls "$HOME"/.config/systemd/user/airc*.service >/dev/null 2>&1 && return 0 ;;
    windows)
      # Same query cmd_daemon.sh:_daemon_installed uses. //v is the
      # MSYS-friendly form of /v (the leading // gets stripped down to
      # / by the MSYS path-mangling shim).
      reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" 2>/dev/null | grep -q 'airc-monitor' && return 0 ;;
  esac
  return 1
}

# ── airc_daemon_is_installed_for_scope <scope> ─────────────────────────
#
# STRICTER variant: returns 0 only when the daemon's autostart entry is
# installed AND wired to the given scope (i.e. a launcher / unit / plist
# pointing at <scope>'s .bat or AIRC_HOME=<scope>).
#
# Use case: install.sh idempotency. The plain airc_daemon_is_installed
# answers "user has any airc daemon" — fine for global presence checks
# (post-disconnect tip, status banner). It returns true even when the
# registered daemon is wired to a DIFFERENT scope from the one being
# bootstrapped. b69f 2026-05-02 hit this: installed daemon while in
# /c/.airc-src, then re-ran AIRC_INSTALL_YES=1 install.sh from
# ~/continuum — install.sh saw "any airc daemon registered" → no-op'd
# the prompt → ~/continuum had no daemon serving it. The fix is for
# install.sh to ask scope-aware: "does the registered daemon point at
# THIS scope?" If not, regenerate.
#
# Returns:
#   0 — daemon entry exists AND points at <scope>
#   1 — no daemon entry OR points at a different scope OR the launcher
#       file the entry points at no longer exists on disk
#
# Detection strategy by platform:
#   darwin    — read AIRC_HOME from plist EnvironmentVariables; match scope
#   linux/wsl — grep Environment=AIRC_HOME=<scope> from systemd unit
#   windows   — extract launcher .bat path from registry value, match
#               against expected <scope>/airc-daemon.bat AND verify
#               the .bat file exists
airc_daemon_is_installed_for_scope() {
  local target_scope="${1:-}"
  [ -n "$target_scope" ] || return 1
  local os; os=$(detect_platform)
  case "$os" in
    darwin)
      local service; service=$(airc_daemon_service_name_for_scope "$target_scope")
      local plist_path
      for plist_path in "$HOME/Library/LaunchAgents/${service}.plist" "$HOME/Library/LaunchAgents/com.cambriantech.airc.plist"; do
        [ -f "$plist_path" ] || continue
        local got
        got=$(plutil -extract EnvironmentVariables.AIRC_HOME raw "$plist_path" 2>/dev/null)
        [ "$got" = "$target_scope" ] && return 0
      done
      return 1
      ;;
    linux|wsl)
      local unit; unit=$(airc_daemon_unit_name_for_scope "$target_scope")
      local unit_path
      for unit_path in "$HOME/.config/systemd/user/$unit" "$HOME/.config/systemd/user/airc.service"; do
        [ -f "$unit_path" ] || continue
      # Fixed-string match (Copilot #422 review caught regex injection):
      # target_scope contains '.' and other regex metacharacters
      # (paths like '/Users/.../.airc/.airc'); the prior ERE form
      # only escaped '/' which let '.airc' false-match. Two passes
      # cover both quoted and unquoted forms emitted by cmd_daemon.sh.
        grep -qF "Environment=\"AIRC_HOME=${target_scope}\"" "$unit_path" && return 0
        grep -qF "Environment=AIRC_HOME=${target_scope}"     "$unit_path" && return 0
      done
      return 1
      ;;
    windows)
      airc_daemon_is_installed || return 1
      # Extract registered launcher cmd line. Format from cmd_daemon.sh:
      # `cmd /c start "" /MIN "<scope_win>\airc-daemon.bat"`.
      local got_value
      local entry_name; entry_name=$(airc_daemon_run_entry_for_scope "$target_scope")
      local values
      values=$( { reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v "$entry_name" 2>/dev/null; \
                  reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v airc-monitor 2>/dev/null; } \
                  | awk -F'    ' '/REG_SZ/ {print $NF}')
      [ -n "$values" ] || return 1
      # Need _to_win_path from platform_adapters.sh. Both install.sh and
      # the airc lib-dir resolver source platform_adapters before this
      # file. If somehow absent (atypical), fall back to a substring
      # match on the unix-form scope which the registered .bat path
      # won't contain — caller will see "different scope, not installed
      # for me" which is the safer side of the failure mode (re-prompts
      # vs falsely claims-already-installed).
      local target_bat_win=""
      if command -v _to_win_path >/dev/null 2>&1; then
        target_bat_win="$(_to_win_path "$target_scope/airc-daemon.bat")"
      fi
      local target_bat_unix="$target_scope/airc-daemon.bat"
      # Match either path representation in the registered cmd line.
      # Windows form is what cmd_daemon writes, but defense-in-depth.
      while IFS= read -r got_value; do
        [ -z "$got_value" ] && continue
        case "$got_value" in
        *"$target_bat_win"*)
          [ -f "$target_bat_unix" ] && return 0
          ;;
        *"$target_bat_unix"*)
          [ -f "$target_bat_unix" ] && return 0
          ;;
        esac
      done <<< "$values"
      return 1
      ;;
  esac
  return 1
}

# ── airc_daemon_is_running_for_scope <scope> ───────────────────────────
#
# Returns 0 only when the platform supervisor is actively loaded/running
# for the given scope. "Installed" is not enough: a launchd plist or
# systemd unit can exist on disk while the job is unloaded, crashed, or
# never bootstrapped. Monitor recovery code must not claim "daemon will
# self-heal" unless this probe is true.
airc_daemon_is_running_for_scope() {
  local target_scope="${1:-}"
  [ -n "$target_scope" ] || return 1
  airc_daemon_is_installed_for_scope "$target_scope" || return 1
  local os; os=$(detect_platform)
  case "$os" in
    darwin)
      local service; service=$(airc_daemon_service_name_for_scope "$target_scope")
      launchctl list 2>/dev/null | awk '{print $3}' | grep -qFx "$service" && return 0
      launchctl list 2>/dev/null | awk '{print $3}' | grep -qFx "com.cambriantech.airc" && return 0
      return 1
      ;;
    linux|wsl)
      local unit; unit=$(airc_daemon_unit_name_for_scope "$target_scope")
      systemctl --user is-active --quiet "$unit" 2>/dev/null && return 0
      systemctl --user is-active --quiet airc.service 2>/dev/null && return 0
      return 1
      ;;
    windows)
      # HKCU Run starts the daemon at login; there is no supervisor API
      # equivalent to launchd/systemd. Treat a live airc-daemon.bat or
      # `airc connect` process for this scope as running.
      if command -v powershell.exe >/dev/null 2>&1; then
        local needle="$target_scope"
        powershell.exe -NoProfile -Command \
          "Get-CimInstance Win32_Process | Where-Object { \$_.CommandLine -like '*airc*' -and \$_.CommandLine -like '*$needle*' } | Select-Object -First 1 | ForEach-Object { 'yes' }" \
          2>/dev/null | grep -q yes && return 0
      fi
      return 1
      ;;
  esac
  return 1
}
