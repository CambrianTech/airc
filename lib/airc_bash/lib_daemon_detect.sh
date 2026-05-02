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
      [ -f "$HOME/Library/LaunchAgents/com.cambriantech.airc.plist" ] && return 0 ;;
    linux|wsl)
      [ -f "$HOME/.config/systemd/user/airc.service" ] && return 0 ;;
    windows)
      # Same query cmd_daemon.sh:_daemon_installed uses. //v is the
      # MSYS-friendly form of /v (the leading // gets stripped down to
      # / by the MSYS path-mangling shim).
      reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v airc-monitor >/dev/null 2>&1 && return 0 ;;
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
      local plist_path="$HOME/Library/LaunchAgents/com.cambriantech.airc.plist"
      [ -f "$plist_path" ] || return 1
      local got
      got=$(plutil -extract EnvironmentVariables.AIRC_HOME raw "$plist_path" 2>/dev/null)
      [ "$got" = "$target_scope" ] && return 0
      return 1
      ;;
    linux|wsl)
      local unit_path="$HOME/.config/systemd/user/airc.service"
      [ -f "$unit_path" ] || return 1
      # Match Environment="AIRC_HOME=<scope>" or Environment=AIRC_HOME=<scope>.
      grep -qE "Environment=\"?AIRC_HOME=${target_scope//\//\\/}\"?($|[[:space:]])" "$unit_path" \
        && return 0
      return 1
      ;;
    windows)
      airc_daemon_is_installed || return 1
      # Extract registered launcher cmd line. Format from cmd_daemon.sh:
      # `cmd /c start "" /MIN "<scope_win>\airc-daemon.bat"`.
      local got_value
      got_value=$(reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v airc-monitor 2>/dev/null \
                  | awk -F'    ' '/REG_SZ/ {print $NF}')
      [ -n "$got_value" ] || return 1
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
      case "$got_value" in
        *"$target_bat_win"*)
          [ -f "$target_bat_unix" ] && return 0
          return 1
          ;;
        *"$target_bat_unix"*)
          [ -f "$target_bat_unix" ] && return 0
          return 1
          ;;
      esac
      return 1
      ;;
  esac
  return 1
}
