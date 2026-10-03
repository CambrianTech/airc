#!/usr/bin/env bash
# Keep this node on the airc mesh on macOS and Linux: run `airc join` as the
# session's supervisor from login, and start it again if it exits. The POSIX
# twin of windows/register-autostart.ps1 (Task Scheduler `airc-join`).
#
#   register-autostart.sh <airc-path> [--existing-only] [--remove] [--check]
#
# --check changes nothing: exit 0 when the supervisor is registered, current and
# loaded for <airc-path>; otherwise it names the drift and exits 1.
#
# macOS: a per-user LaunchAgent. Linux: a systemd user unit. Both run in the
# account's home so `airc join` resolves the machine-account scope, never the
# scope of whatever directory setup was launched from. Re-running is a no-op
# when the registration already matches; a changed registration is reloaded.
set -euo pipefail

# One name for the mesh supervisor on every OS: the Windows task is `airc-join` too.
LABEL=airc-join
UNIT=airc-join.service

die() { printf 'AIRC autostart: %s\n' "$*" >&2; exit 1; }

_xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }

# The LaunchAgent plist for <airc-path>, <home>, <path>.
autostart_plist() {
  local airc home path
  airc="$(printf '%s' "$1" | _xml)"; home="$(printf '%s' "$2" | _xml)"; path="$(printf '%s' "$3" | _xml)"
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>$airc</string><string>join</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>AIRC_SUPERVISOR</key><string>1</string>
    <key>PATH</key><string>$path</string>
  </dict>
  <key>WorkingDirectory</key><string>$home</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>120</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>/dev/null</string>
  <key>StandardErrorPath</key><string>$home/.airc/logs/join.err.log</string>
</dict>
</plist>
PLIST
}

# A value for a double-quoted systemd unit setting: backslash and quote are escaped
# C-style, and % is doubled so a path is never read as a specifier (%h, %u, ...).
_systemd_quote() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g'
}

# The inverse, for reading back a value this script wrote.
_systemd_unquote() {
  printf '%s' "$1" | sed -e 's/%%/%/g' -e 's/\\"/"/g' -e 's/\\\\/\\/g'
}

# The systemd user unit for <airc-path>, <path>.
autostart_unit() {
  local airc path
  airc="$(_systemd_quote "$1")"
  path="$(_systemd_quote "$2")"
  cat <<UNITFILE
[Unit]
Description=Keep this node on the airc mesh: run airc join from login and restart it if it exits

[Service]
Type=simple
ExecStart="$airc" join
Environment=AIRC_SUPERVISOR=1
Environment="PATH=$path"
WorkingDirectory=%h
Restart=always
RestartSec=120
StandardOutput=null
StandardError=append:%h/.airc/logs/join.err.log

[Install]
WantedBy=default.target
UNITFILE
}

_register_launchd() {
  local airc="$1" existing_only="$2" remove="$3" check="$4"
  local agents="$HOME/Library/LaunchAgents" target="gui/$(id -u)/$LABEL"
  local plist="$agents/$LABEL.plist"
  if [ "$check" = 1 ]; then
    [ -f "$plist" ] || { printf 'AIRC autostart: %s is not registered\n' "$target" >&2; return 1; }
    # Compare against the PATH it was registered with: the checker's own PATH (a
    # service, another shell) is not the supervisor's and is not drift.
    local registered_path
    registered_path="$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:PATH' "$plist" 2>/dev/null || true)"
    cmp -s <(autostart_plist "$airc" "$HOME" "$registered_path") "$plist" ||
      { printf 'AIRC autostart: %s runs a different command than %s\n' "$target" "$airc" >&2; return 1; }
    launchctl print "$target" >/dev/null 2>&1 ||
      { printf 'AIRC autostart: %s is not loaded\n' "$target" >&2; return 1; }
    printf 'AIRC autostart converged (%s)\n' "$target"
    return 0
  fi
  if [ "$remove" = 1 ]; then
    launchctl bootout "$target" >/dev/null 2>&1 || true
    rm -f "$plist"
    printf 'AIRC autostart removed (%s)\n' "$target"
    return 0
  fi
  if [ "$existing_only" = 1 ] && [ ! -f "$plist" ]; then return 0; fi
  mkdir -p "$agents" "$HOME/.airc/logs"
  local draft; draft="$(mktemp "$HOME/.airc/logs/$LABEL.XXXXXX")"
  autostart_plist "$airc" "$HOME" "$PATH" > "$draft"
  plutil -lint "$draft" >/dev/null || { rm -f "$draft"; die "rendered LaunchAgent does not lint"; }
  local loaded=0
  launchctl print "$target" >/dev/null 2>&1 && loaded=1
  if [ "$loaded" = 1 ] && cmp -s "$draft" "$plist"; then
    rm -f "$draft"
    return 0
  fi
  mv -f "$draft" "$plist"
  [ "$loaded" = 1 ] && launchctl bootout "$target" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$plist" || die "launchctl bootstrap gui/$(id -u) $plist failed"
  launchctl print "$target" >/dev/null 2>&1 || die "$target is not registered after bootstrap"
  printf 'AIRC autostart registered (%s)\n' "$target"
}

_register_systemd() {
  local airc="$1" existing_only="$2" remove="$3" check="$4"
  local dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user" unit
  unit="$dir/$UNIT"
  if [ "$check" = 1 ]; then
    [ -f "$unit" ] || { printf 'AIRC autostart: %s is not registered\n' "$UNIT" >&2; return 1; }
    local registered_path
    registered_path="$(_systemd_unquote "$(sed -n 's/^Environment="PATH=\(.*\)"$/\1/p' "$unit")")"
    [ "$(cat "$unit")" = "$(autostart_unit "$airc" "$registered_path")" ] ||
      { printf 'AIRC autostart: %s runs a different command than %s\n' "$UNIT" "$airc" >&2; return 1; }
    systemctl --user is-enabled --quiet "$UNIT" 2>/dev/null ||
      { printf 'AIRC autostart: %s is not enabled\n' "$UNIT" >&2; return 1; }
    printf 'AIRC autostart converged (%s)\n' "$UNIT"
    return 0
  fi
  if [ "$remove" = 1 ]; then
    systemctl --user disable --now "$UNIT" >/dev/null 2>&1 || true
    rm -f "$unit"
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    printf 'AIRC autostart removed (%s)\n' "$UNIT"
    return 0
  fi
  if [ "$existing_only" = 1 ] && [ ! -f "$unit" ]; then return 0; fi
  if ! { command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; }; then
    # A container or headless box without a user session cannot host the unit.
    # Not an install failure: say so, so the operator knows nothing restarts airc.
    printf 'AIRC autostart: no systemd user session here; airc will not restart at login. Run `airc join` from your session manager.\n' >&2
    return 0
  fi
  mkdir -p "$dir" "$HOME/.airc/logs"
  local rendered; rendered="$(autostart_unit "$airc" "$PATH")"
  if [ -f "$unit" ] && [ "$(cat "$unit")" = "$rendered" ] && systemctl --user is-enabled --quiet "$UNIT"; then
    systemctl --user start "$UNIT"
    return 0
  fi
  printf '%s\n' "$rendered" > "$unit"
  systemctl --user daemon-reload
  systemctl --user enable "$UNIT" >/dev/null
  systemctl --user restart "$UNIT"
  systemctl --user is-enabled --quiet "$UNIT" || die "$UNIT is not enabled after registration"
  printf 'AIRC autostart registered (%s)\n' "$UNIT"
}

main() {
  local airc="" existing_only=0 remove=0 check=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --existing-only) existing_only=1 ;;
      --remove) remove=1 ;;
      --check) check=1 ;;
      -*) die "unknown option $1" ;;
      *) airc="$1" ;;
    esac
    shift
  done
  [ "$remove" = 1 ] || [ -n "$airc" ] || die "usage: register-autostart.sh <airc-path> [--existing-only] [--remove] [--check]"
  if [ -n "$airc" ]; then
    [ -x "$airc" ] || die "$airc is not an executable"
    airc="$(cd "$(dirname "$airc")" && pwd -P)/$(basename "$airc")"
  fi
  case "$(uname -s)" in
    Darwin) _register_launchd "$airc" "$existing_only" "$remove" "$check" ;;
    Linux) _register_systemd "$airc" "$existing_only" "$remove" "$check" ;;
    *) die "unsupported platform $(uname -s); Windows registers through windows/register-autostart.ps1" ;;
  esac
}

# Sourced by test/unix-autostart.sh for the renderers; executed by install.sh.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
