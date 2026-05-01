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
