# Sourced by airc. cmd_daemon family — install / status / uninstall /
# log of the OS auto-restart for `airc connect`.
#
# Functions exported back to airc's dispatch:
#   cmd_daemon              — verb router (install|status|uninstall|log)
#   cmd_daemon_install      — top-level installer, branches per platform
#   cmd_daemon_uninstall    — top-level uninstaller
#   cmd_daemon_status       — dump platform-native unit/plist state + log tail
#   cmd_daemon_log          — `tail` the daemon stdout log
#
# Private helpers (all `_daemon_*` named):
#   _daemon_airc_path       — resolve the absolute path airc was invoked as
#   _daemon_scope           — pick install scope (defaults to $HOME/.airc)
#   _daemon_installed       — fast yes/no probe used by monitor self-heal
#   _daemon_install_done    — shared post-install confirmation print
#   _daemon_install_launchd — macOS plist writer + launchctl bootstrap
#   _daemon_install_schtasks— Windows HKCU Run-key registration
#   _daemon_install_systemd — Linux/WSL systemd-user unit writer
#
# External cross-references (resolved at call time, defined inline in airc
# top-level): die, detect_platform. Also called BY cmd_connect / monitor
# (`_daemon_installed` for the no-claude-left-behind self-heal probe).
#
# Extracted from airc as part of #152 Phase 3 file split, after Joel
# 2026-04-27 push: "shell scripts are like classes; the 5200-line bash
# monolith was wrong." This is the cmd_daemon group — each command-family
# becomes one .sh file, mirroring the cmd_doctor.sh / cmd_connect.sh
# extraction pattern.

# ── cmd_daemon: install / manage the OS auto-restart for `airc connect` ────
# Issue followup to #39 substrate: the channel must auto-resume across machine
# sleep/wake/crash so users walk away and come back to a live mesh. Without
# this, every laptop sleep kills airc + the user must remember to restart it.
#
# Implementation: install a platform-native autostart that wraps `airc connect`
# with KeepAlive/Restart=always. AIRC_BACKGROUND_OK=1 is set in the env so
# airc's heartbeat-stdout-pipe-trap doesn't exit-3 under launchd/systemd
# (which have no notification-consumer reading stdout).
#
# Subcommands:
#   airc daemon install    Install + start the autostart entry
#   airc daemon uninstall  Stop + remove the autostart entry
#   airc daemon status     Show install state + running pid + log path
#   airc daemon log [N]    Tail the daemon stdout log
#
# Scope: defaults to the GLOBAL scope ($HOME/.airc), since the daemon is the
# user's "always-on" mesh presence — not tied to a specific project dir. If
# the user wants a per-project always-on daemon, they pass AIRC_HOME=<dir>
# in the environment when running install (and the generated unit/plist
# will carry that scope).
cmd_daemon() {
  local action="${1:-status}"
  shift 2>/dev/null || true
  case "$action" in
    -h|--help|help)
      echo "Usage: airc daemon [install|uninstall|restart|status|log]"
      echo "  install     register OS auto-restart (launchd/systemd/schtasks)"
      echo "  uninstall   remove auto-restart registration"
      echo "  restart     uninstall + install (pick up new airc binary)"
      echo "  status      print platform-native unit/plist state + log tail"
      echo "  log [N]     tail the daemon stdout log (default 50 lines)"
      return 0 ;;
    install)   cmd_daemon_install "$@" ;;
    uninstall|remove) cmd_daemon_uninstall "$@" ;;
    restart)   shift; cmd_daemon_uninstall "$@" >/dev/null && cmd_daemon_install "$@" ;;
    status)    cmd_daemon_status "$@" ;;
    log|logs)  cmd_daemon_log "$@" ;;
    stop|start)
      # 2026-05-02 QA caught: 'stop' was silently aliased to uninstall
      # (removes registration entirely, not just halts the running
      # process). systemd/launchd convention: stop = halt, disable =
      # unregister. Pre-fix users typing 'airc daemon stop' got the
      # daemon UNINSTALLED, which broke auto-restart on next login.
      # Surface this honestly + point at the right command.
      die "airc daemon $action is not a verb. Use:
  airc daemon uninstall   — remove the registration entirely
  airc daemon restart     — bounce the daemon to pick up new airc binary
  airc daemon install     — re-register (idempotent if already installed)
The OS launchd/systemd/HKCU manages start/stop of registered units automatically." ;;
    *)         die "Usage: airc daemon [install|uninstall|restart|status|log]" ;;
  esac
}

# Resolve the absolute path to airc binary that should run under the daemon.
# install.sh symlinks $HOME/.local/bin/airc → $AIRC_DIR/airc; we want the
# real path so a future `airc update` (which mutates $AIRC_DIR/airc in
# place) is picked up by launchd/systemd without re-installing the unit.
_daemon_airc_path() {
  local airc_link="${HOME}/.local/bin/airc"
  if [ -L "$airc_link" ] || [ -x "$airc_link" ]; then
    echo "$airc_link"
  elif [ -x "${AIRC_DIR:-$HOME/.airc-src}/airc" ]; then
    echo "${AIRC_DIR:-$HOME/.airc-src}/airc"
  else
    echo "/usr/local/bin/airc"  # last-resort guess; install will fail loud if wrong
  fi
}

# The scope the daemon will run under. Mirrors detect_scope() (line 135)
# so `airc daemon install` from a project dir captures THAT dir's
# .airc as the daemon's scope -- otherwise the daemon spawns a monitor
# pointed at $HOME/.airc (empty / wrong room) while the user's actual
# join state lives at $cwd/.airc. Joel 2026-04-28: "lol obv if it
# worked you would have a monitor and be online. FAIL" -- caught the
# scope mismatch on continuum-b69f's box.
_daemon_scope() {
  if [ -n "${AIRC_HOME:-}" ]; then
    echo "$AIRC_HOME"
  else
    echo "$(pwd -P)/.airc"
  fi
}

# Returns 0 if the autostart daemon (launchd / systemd unit) is installed
# on this OS, 1 otherwise. Used by the monitor escalation banner (#184)
# to tell the user whether the upcoming exit-99 will trigger self-heal
# (daemon present) or just kill the relay silently (no daemon — they
# need to `airc join` again).
_daemon_installed() {
  # Delegates to airc_daemon_is_installed (lib/airc_bash/lib_daemon_detect.sh).
  # Kept as a thin wrapper to preserve the local-private-helper shape
  # callers in this file use; the cross-platform detection logic lives
  # in the shared detector so install.sh + cmd_connect.sh see the same
  # answer (Copilot review #388 caught the prior drift).
  airc_daemon_is_installed
}

cmd_daemon_install() {
  local os; os=$(detect_platform)
  local airc_bin; airc_bin=$(_daemon_airc_path)
  local scope; scope=$(_daemon_scope)
  mkdir -p "$scope"

  case "$os" in
    darwin) _daemon_install_launchd "$airc_bin" "$scope" ;;
    linux|wsl) _daemon_install_systemd "$airc_bin" "$scope" "$os" ;;
    windows) _daemon_install_schtasks "$airc_bin" "$scope" ;;
    *) die "Daemon install not supported on $(uname -s). Manual workaround: run 'airc connect' under your platform's preferred autostart mechanism." ;;
  esac
}

# Print the common "daemon installed; here's where to look" footer.
# Three platform installers used to duplicate this 5-line block; now
# they call this helper. Pass the platform-specific lead line as $1 and
# any optional trailing note as $2 (heredoc-style multi-line OK).
_daemon_install_done() {
  local lead="$1" scope="$2" note="${3:-}"
  echo "  ✓ $lead"
  echo "  airc will now auto-start at login + restart on exit."
  echo "  Logs:   $scope/daemon.log"
  echo "  Status: airc daemon status"
  if [ -n "$note" ]; then echo ""; printf '  %s\n' "$note"; fi
}

_daemon_install_launchd() {
  local airc_bin="$1" scope="$2"
  local plist_dir="$HOME/Library/LaunchAgents"
  local plist_path="$plist_dir/com.cambriantech.airc.plist"
  mkdir -p "$plist_dir"
  cat > "$plist_path" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.cambriantech.airc</string>
    <key>ProgramArguments</key>
    <array>
        <string>${airc_bin}</string>
        <string>connect</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>AIRC_BACKGROUND_OK</key>
        <string>1</string>
        <key>AIRC_HOME</key>
        <string>${scope}</string>
        <key>HOME</key>
        <string>${HOME}</string>
        <key>PATH</key>
        <string>/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:${HOME}/.local/bin</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${scope}/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>${scope}/daemon.err</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>ThrottleInterval</key>
    <integer>10</integer>
</dict>
</plist>
PLIST
  echo "  Wrote $plist_path"
  # Bootout first to reset any prior load (idempotent install).
  launchctl bootout "gui/$(id -u)/com.cambriantech.airc" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$plist_path" 2>&1 \
    || die "launchctl bootstrap failed. Plist written but not loaded; check Console.app for errors."
  launchctl enable "gui/$(id -u)/com.cambriantech.airc" 2>/dev/null || true
  _daemon_install_done "Loaded into launchd (gui/$(id -u)/com.cambriantech.airc)" "$scope" \
    "Note: if 'airc canary' / gist push fails under launchd, the gh keychain may not be unlocked at boot. Workaround: 'gh auth status' once after login to unlock; airc daemon picks it up on next restart."
}

_daemon_install_schtasks() {
  # Windows daemon via HKCU Run-key (no admin; HKCU\...\Run is user-
  # scope, so per-user autostart at logon without UAC). PRs #200/#202
  # for the why; this function for the how.
  local airc_bin="$1" scope="$2"
  local entry_name="airc-monitor"

  # Find Git Bash — the launcher .bat needs it to exec airc.
  local bash_exe=""
  for c in 'C:\Program Files\Git\bin\bash.exe' 'C:\Program Files (x86)\Git\bin\bash.exe' "$HOME/AppData/Local/Programs/Git/bin/bash.exe"; do
    local check_path; check_path=$(echo "$c" | sed 's|\\|/|g; s|^C:|/c|')
    if [ -f "$c" ] || [ -f "$check_path" ]; then bash_exe="$c"; break; fi
  done
  [ -z "$bash_exe" ] && die "bash.exe not found at any standard Git for Windows path. Install Git for Windows + re-run."

  # Convert paths to Windows form; cmd.exe can't read /c/Users/... .
  local airc_bin_win; airc_bin_win=$(_to_win_path "$airc_bin")
  local scope_win; scope_win=$(_to_win_path "$scope")

  # Launcher .bat: cd to cwd (so airc's detect_scope finds <cwd>/.airc),
  # bash -c (not -lc, to keep cmd-set env), absolute unix airc path
  # (bash -c doesn't read .bashrc so PATH won't have ~/.local/bin).
  # Loop with 5s restart matches launchd KeepAlive / systemd Restart=always.
  # See PR #202 for the bug history that necessitated each of those choices.
  local cwd_win; cwd_win=$(_to_win_path "$(pwd -P)")
  local airc_bin_unix; airc_bin_unix=$(_to_bash_path "$airc_bin")
  [ -z "$airc_bin_unix" ] && airc_bin_unix="$airc_bin"
  # Marker path the .bat polls to distinguish intentional re-exec
  # (written by _reexec_into) from "actual crash" (#203/#204).
  local marker_win; marker_win=$(_to_win_path "$scope/airc.reexec-marker")
  local launcher_bash="$scope/airc-daemon.bat"
  cat > "$launcher_bash" <<EOF
@echo off
REM AIRC daemon launcher — generated by 'airc daemon install' on Windows.
REM Runs airc connect under bash, restarting on exit. Logs to daemon.log.
REM On intentional re-exec (host-takeover or rejoin-as-joiner), airc
REM writes airc.reexec-marker — we step aside rather than respawn,
REM since the new airc bash from the exec is now the daemon.
cd /d "$cwd_win"
set AIRC_BACKGROUND_OK=1
:loop
REM Stdout → daemon.log so the operator + the AI Monitor (when daemon
REM is being read post-mortem) can see what airc actually emitted.
REM Pre-fix: stdout went to nowhere (start /MIN cmd window had no
REM redirect), only daemon.err captured the launcher's own restart
REM messages — so 'airc daemon log' showed nothing useful, and
REM "daemon.log doesn't exist" became a real symptom (b69f
REM 2026-05-02 in #cambriantech). Stderr → daemon.err keeps the
REM launcher's restart records separate from the airc event stream.
"$bash_exe" -c "exec '$airc_bin_unix' connect" 1>> "$scope_win\\daemon.log" 2>> "$scope_win\\daemon.err"
REM Did airc just intentionally re-exec? If marker exists and is recent,
REM the new airc process from the exec is now the running daemon —
REM exit the launcher loop instead of racing-respawn it.
REM forfiles /m airc.reexec-marker /d 0 /c "cmd /c exit 0" succeeds when
REM the file's mtime is today (fine-grained age check below via type +
REM date math is too brittle for .bat; "today" is our 60s proxy).
if exist "$marker_win" (
  forfiles /p "$scope_win" /m airc.reexec-marker /d 0 /c "cmd /c exit 0" >nul 2>&1
  if not errorlevel 1 (
    echo [%date% %time%] airc re-exec'd into different mode ^(host-takeover or rejoin^); new process is now daemon, launcher exiting. >> "$scope_win\\daemon.err"
    del "$marker_win" >nul 2>&1
    exit /b 0
  )
)
echo [%date% %time%] airc connect exited. Restarting in 5s. >> "$scope_win\\daemon.err"
timeout /t 5 /nobreak >nul
goto loop
EOF
  local launcher_win; launcher_win=$(_to_win_path "$launcher_bash")

  # `cmd /c start "" /MIN <bat>` launches detached + minimized; empty ""
  # is start's title slot. reg add /f is idempotent (overwrites prior).
  local run_cmd="cmd /c start \"\" /MIN \"$launcher_win\""
  reg add "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v "$entry_name" //t REG_SZ //d "$run_cmd" //f >/dev/null 2>&1 \
    || die "reg add failed for HKCU Run\\$entry_name"
  # Start now (no logout/login needed). Fires-and-forgets.
  cmd //c start "" //MIN "$launcher_win" >/dev/null 2>&1 || true

  echo "  ✓ Started monitor in detached cmd window (minimized)"
  _daemon_install_done "Registered HKCU Run entry '$entry_name' (runs at every Windows logon)" "$scope"
}

_daemon_install_systemd() {
  local airc_bin="$1" scope="$2" os="$3"
  local unit_dir="$HOME/.config/systemd/user"
  local unit_path="$unit_dir/airc.service"
  if ! command -v systemctl >/dev/null 2>&1; then
    if [ "$os" = "wsl" ]; then
      die "systemctl not found. Enable systemd in WSL: edit /etc/wsl.conf to add [boot]\nsystemd=true, then 'wsl --shutdown' from PowerShell + restart your distro."
    else
      die "systemctl not found. Daemon install requires systemd."
    fi
  fi
  # Probe the user-level systemd bus BEFORE writing the unit. WSL2 ships
  # systemctl on PATH but typically has init (not systemd) as PID 1, so
  # `systemctl --user` returns "Failed to connect to bus" — we'd write
  # the unit then fail to load it, leaving cruft on disk. Detect early.
  if ! systemctl --user is-system-running >/dev/null 2>&1 \
     && ! systemctl --user list-units >/dev/null 2>&1; then
    if [ "$os" = "wsl" ]; then
      cat >&2 <<EOF
ERROR: systemctl is on PATH but the user-level systemd bus isn't reachable
       (Failed to connect to bus). On WSL2 this means systemd isn't running
       as PID 1 — the default Ubuntu image launches with init instead.

Enable systemd in WSL:
  1. In your WSL distro, edit /etc/wsl.conf and add:

       [boot]
       systemd=true

  2. From PowerShell on Windows:    wsl --shutdown
  3. Reopen your WSL terminal. Confirm with:    ps -p 1 -o comm=    (should print "systemd")
  4. Re-run:    airc daemon install

Until systemd is enabled, airc daemon can't auto-resume on this WSL distro.
A manual fallback for now:  run 'airc connect' in a tmux/screen session that
won't get killed by your WSL shell exit.
EOF
      return 1
    else
      die "systemctl present but user-level systemd bus unreachable. Check: systemctl --user status (and ensure systemd is PID 1 on this host)."
    fi
  fi
  mkdir -p "$unit_dir"
  cat > "$unit_path" <<UNIT
[Unit]
Description=airc — agentic IRC chat for AI peers (auto-resume mesh)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${airc_bin} connect
Restart=always
RestartSec=5
Environment="AIRC_BACKGROUND_OK=1"
Environment="AIRC_HOME=${scope}"
StandardOutput=append:${scope}/daemon.log
StandardError=append:${scope}/daemon.err

[Install]
WantedBy=default.target
UNIT
  echo "  Wrote $unit_path"
  systemctl --user daemon-reload || die "systemctl --user daemon-reload failed."
  systemctl --user enable --now airc.service \
    || die "systemctl --user enable --now airc.service failed."
  _daemon_install_done "Loaded into systemd-user (airc.service)" "$scope" \
    "Note: systemd-user units stop at logout unless lingering is enabled. For always-on across logout: sudo loginctl enable-linger \$USER"
}

cmd_daemon_uninstall() {
  local os; os=$(detect_platform)
  case "$os" in
    darwin)
      local plist_path="$HOME/Library/LaunchAgents/com.cambriantech.airc.plist"
      launchctl bootout "gui/$(id -u)/com.cambriantech.airc" 2>/dev/null \
        && echo "  ✓ Unloaded from launchd" \
        || echo "  (was not loaded)"
      [ -f "$plist_path" ] && rm "$plist_path" && echo "  ✓ Removed $plist_path" \
        || echo "  (no plist on disk)"
      ;;
    linux|wsl)
      systemctl --user disable --now airc.service 2>/dev/null \
        && echo "  ✓ Stopped + disabled airc.service" \
        || echo "  (was not enabled)"
      local unit_path="$HOME/.config/systemd/user/airc.service"
      [ -f "$unit_path" ] && rm "$unit_path" && systemctl --user daemon-reload && echo "  ✓ Removed $unit_path" \
        || echo "  (no unit on disk)"
      ;;
    windows)
      local entry_name="airc-monitor"
      if reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v "$entry_name" >/dev/null 2>&1; then
        reg delete "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v "$entry_name" //f >/dev/null 2>&1 \
          && echo "  ✓ Removed HKCU Run entry '$entry_name'" \
          || echo "  (reg delete failed — try 'reg delete' manually)"
      else
        echo "  (no Run entry '$entry_name' registered)"
      fi
      # Kill any currently-running daemon-launched airc-connect tree.
      # Match on the launcher .bat path so we don't kill foreground
      # `airc join` running in the user's terminal.
      local scope; scope=$(_daemon_scope)
      if ps -ef 2>/dev/null | grep 'airc-daemon.bat' | grep -v grep >/dev/null; then
        ps -ef | grep 'airc-daemon.bat' | grep -v grep | awk '{print $2}' | while read pid; do
          kill "$pid" 2>/dev/null || true
        done
        echo "  ✓ Killed running daemon launcher process(es)"
      fi
      [ -f "$scope/airc-daemon.bat" ] && rm "$scope/airc-daemon.bat" \
        && echo "  ✓ Removed $scope/airc-daemon.bat"
      ;;
    *) echo "  Daemon uninstall not supported on $(uname -s)."; return 1 ;;
  esac
}

cmd_daemon_status() {
  local os; os=$(detect_platform)
  case "$os" in
    darwin)
      local plist_path="$HOME/Library/LaunchAgents/com.cambriantech.airc.plist"
      if [ -f "$plist_path" ]; then
        echo "  Plist:   $plist_path"
        # launchctl print returns rich state; grep the key fields.
        local state; state=$(launchctl print "gui/$(id -u)/com.cambriantech.airc" 2>/dev/null \
          | grep -E 'state =|pid =|last exit code' | head -3)
        if [ -n "$state" ]; then
          echo "  Loaded:  yes"
          printf '%s\n' "$state" | sed 's/^[[:space:]]*/    /'
        else
          echo "  Loaded:  no (plist present but not bootstrapped — try 'airc daemon install' to reload)"
        fi
        local scope; scope=$(_daemon_scope)
        echo "  Logs:    $scope/daemon.log"
      else
        echo "  No daemon installed. Run: airc daemon install"
      fi
      ;;
    linux|wsl)
      local unit_path="$HOME/.config/systemd/user/airc.service"
      if [ -f "$unit_path" ]; then
        echo "  Unit:    $unit_path"
        local active; active=$(systemctl --user is-active airc.service 2>/dev/null)
        local enabled; enabled=$(systemctl --user is-enabled airc.service 2>/dev/null)
        echo "  Active:  $active"
        echo "  Enabled: $enabled"
        local scope; scope=$(_daemon_scope)
        echo "  Logs:    $scope/daemon.log  (journalctl --user -u airc -f for live)"
      else
        echo "  No daemon installed. Run: airc daemon install"
      fi
      ;;
    windows)
      local entry_name="airc-monitor"
      if reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" //v "$entry_name" >/dev/null 2>&1; then
        echo "  Type:    HKCU Run-key (per-user logon autostart, no admin)"
        echo "  Entry:   $entry_name"
        local scope; scope=$(_daemon_scope)
        echo "  Logs:    $scope/daemon.log"
        echo "  Errors:  $scope/daemon.err"
        echo "  Launcher: $scope/airc-daemon.bat"
        # Is the daemon-launched airc actually running right now? The
        # launcher .bat spawns bash + airc-connect then exits, so we
        # look for the airc-connect process (PPID=1 = orphaned-into-
        # init, which is what `start /B` produces on Windows). Falling
        # back to airc.pid lookup if that fails.
        # Bug #3 from b69f's 2026-05-02 audit: pre-fix reported RUNNING
        # whenever ps-ef awk matched, WITHOUT verifying with kill -0.
        # ps-ef can report zombie/defunct/stale matches. ALWAYS verify
        # the matched PID with kill -0 before claiming RUNNING.
        # Also verify the launcher .bat still exists — if registry points
        # to a deleted path, status must surface STALE rather than say
        # RUNNING based on an unrelated airc-connect process.
        local launcher_bat="$scope/airc-daemon.bat"
        local launcher_status="ok"
        if [ ! -f "$launcher_bat" ]; then
          launcher_status="missing"
        fi
        local live_pid
        local raw_pid
        raw_pid=$(ps -ef 2>/dev/null | awk '$3 == 1 && /airc.*connect/ && !/grep/ {print $2; exit}')
        if [ -n "$raw_pid" ] && kill -0 "$raw_pid" 2>/dev/null; then
          live_pid="$raw_pid"
        fi
        if [ -z "$live_pid" ] && [ -f "$scope/airc.pid" ]; then
          local pidfile_pid
          pidfile_pid=$(head -1 "$scope/airc.pid" 2>/dev/null | tr -d '[:space:]')
          if [ -n "$pidfile_pid" ] && kill -0 "$pidfile_pid" 2>/dev/null; then
            live_pid="$pidfile_pid (from airc.pid)"
          fi
        fi
        # Status decision tree, in priority order so the user sees the
        # actionable failure mode first when more than one applies:
        #   1. launcher_status=missing → MISSING_LAUNCHER (registry
        #      points to a path that doesn't exist; reinstall needed)
        #   2. live_pid set + launcher present → RUNNING (truly alive)
        #   3. launcher present, no live pid → registered (waiting on
        #      next logon OR daemon was killed; user can re-fire)
        if [ "$launcher_status" = "missing" ]; then
          echo "  Status:  MISSING_LAUNCHER ($launcher_bat absent — registry stale; reinstall: airc daemon uninstall && airc daemon install)"
        elif [ -n "$live_pid" ]; then
          echo "  Status:  RUNNING (PID $live_pid, launcher exists, kill -0 verified)"
        else
          echo "  Status:  STALE/STOPPED (launcher exists but no live airc process; will start at next logon — or 'airc daemon install' to start now)"
        fi
      else
        echo "  No daemon installed. Run: airc daemon install"
      fi
      ;;
    *) echo "  Daemon status not supported on $(uname -s)." ;;
  esac
}

cmd_daemon_log() {
  local n="${1:-50}"
  local scope; scope=$(_daemon_scope)
  local log="$scope/daemon.log"
  if [ ! -f "$log" ]; then
    echo "  No log at $log. Daemon may not have started yet."
    return 1
  fi
  tail -"$n" "$log"
}
