# Sourced by airc. cmd_teardown + cmd_disconnect — leave/cleanup verbs.
#
# Functions exported back to airc's dispatch:
#   cmd_teardown    — kill all airc processes in this scope, free ports;
#                     --flush wipes state dir, --all nukes every airc-
#                     looking process on the machine.
#   cmd_disconnect  — "leave the room" softly: kill processes, clear
#                     host-pairing fields, preserve identity + peers +
#                     message history. Next `airc connect` is a fresh
#                     host instead of resume.
#
# External cross-references (call-time): die, ensure_init, get_config_val,
# unset_config_keys, proc_airc_pids_matching, port_listeners, AIRC_HOME,
# AIRC_WRITE_DIR. Both verbs share the kill loop but split on what to
# clear afterwards.
#
# Extracted from airc as part of #152 Phase 3 file split. Continues the
# Joel 2026-04-27 modularization push: every cmd_X group becomes its own
# file so the airc top-level retains only bootstrap + helpers + dispatch.

cmd_teardown() {
  # Kill all airc processes for this user and free any ports they hold.
  # Add --flush to also wipe the state dir (identity, peers, messages) — nuclear.
  # Add --all to nuke EVERY airc-looking process on this machine, ignoring
  # scope/PID file — for the "I just want it all dead" case after stale
  # zombies survive across sessions (verified 2026-04-21: /tmp/airc-prefix
  # connect processes from a previous session were still alive 2 days later
  # because teardown's PID file no longer existed for them).
  local flush=0 all=0
  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        echo "Usage:"
        echo "  airc teardown          kill all airc processes for this scope"
        echo "  airc teardown --flush  also wipe state dir (identity, peers, messages)"
        echo "  airc teardown --all    nuke EVERY airc-looking process on this machine"
        return 0 ;;
      --flush) flush=1 ;;
      --all)   all=1 ;;
      *) echo "  unknown teardown flag: $1" >&2; return 2 ;;
    esac
    shift
  done

  # ── --all: nuclear, scope-blind ───────────────────────────────────
  # Find every airc-related process for THIS user and kill it. Targets:
  #   - bash processes running `airc connect` (any scope)
  #   - bash processes running `<dir>/airc connect` or `/tmp/airc-prefix connect`
  #   - python processes spawned by airc (the inline -u -c monitor with
  #     the `WATCHDOG_SEC` heredoc) — identified by ppid pointing at one
  #     of the bash processes we're killing
  #   - python listeners holding any TCP port in the airc range (7547-7559)
  # Then proceeds to the scope-aware path below to clean up our own pidfile
  # + reap any orphaned listener on our specific port.
  if [ "$all" = "1" ]; then
    local nuked=0
    # Bash airc-connect processes (any path that ends in /airc connect or
    # the /tmp/airc-prefix bootstrap variant the curl|bash installer uses).
    local bash_pids
    bash_pids=$(proc_airc_pids_matching '(airc|airc-prefix)[[:space:]]+connect' || true)
    if [ -n "$bash_pids" ]; then
      echo "  --all: killing airc bash processes: $(echo $bash_pids | tr '\n' ' ')"
      kill -9 $bash_pids 2>/dev/null || true
      nuked=1
    fi
    # Python listeners on airc port range (7547-7559). Don't touch python
    # outside that range — could be unrelated processes.
    local port
    for port in 7547 7548 7549 7550 7551 7552 7553 7554 7555 7556 7557 7558 7559; do
      local lpids
      lpids=$(port_listeners "$port" || true)
      for lpid in $lpids; do
        local cmd
        cmd=$(proc_cmdline "$lpid" || true)
        if echo "$cmd" | grep -q "socket.SOCK_STREAM\|socket.AF_INET"; then
          echo "  --all: freeing port $port (python pid $lpid)"
          kill -9 "$lpid" 2>/dev/null || true
          nuked=1
        fi
      done
    done
    # Stale tail/ssh subprocesses that look like airc message tails
    # (ssh ... tail -F .../.airc/messages.jsonl).
    local tail_pids
    tail_pids=$(proc_airc_pids_matching '\.airc/messages\.jsonl' || true)
    if [ -n "$tail_pids" ]; then
      echo "  --all: killing stale airc message tails: $(echo $tail_pids | tr '\n' ' ')"
      kill -9 $tail_pids 2>/dev/null || true
      nuked=1
    fi
    [ "$nuked" = "0" ] && echo "  --all: no machine-wide airc processes to kill."
    # Fall through to scope-aware path below to also clean up THIS scope's
    # pidfile + flush if requested. (--all is additive, not exclusive.)
  fi


  local killed=0
  # Hosted gist cleanup BEFORE process kill. The cmd_connect EXIT trap
  # would normally delete our hosted gist on graceful shutdown, but the
  # kill -9 below skips traps entirely. Without this explicit step,
  # every `airc teardown` of a host left an orphan gist on the gh
  # account that joiners couldn't tell apart from a live host until
  # heartbeat went stale (~90s later). Caught by Joel's other tab
  # bouncing repeatedly and accumulating fresh #general gists each
  # cycle.
  if [ -f "$AIRC_WRITE_DIR/host_gist_id" ] && command -v gh >/dev/null 2>&1; then
    local _td_gist; _td_gist=$(cat "$AIRC_WRITE_DIR/host_gist_id" 2>/dev/null)
    if [ -n "$_td_gist" ]; then
      if gh gist delete "$_td_gist" --yes >/dev/null 2>&1; then
        echo "  deleted hosted gist: $_td_gist"
      fi
      rm -f "$AIRC_WRITE_DIR/host_gist_id"
    fi
  fi

  # Sidecar scope cleanup (issue #121 — multi-room presence).
  # When the primary tab spawned a #general sidecar, that sidecar runs
  # in a sibling .general scope with its own pidfile + (if hosting)
  # its own host_gist_id. Mirror the primary's gist cleanup + pidfile
  # kill there. Without this, killing the primary leaves an orphan
  # #general gist on the gh account AND an orphan sidecar process that
  # the primary's pidfile descendant-walk wouldn't catch (sidecar's
  # bash isn't a child of cmd_teardown — it was forked detached).
  #
  # Guard: AIRC_TEARDOWN_PART_ONLY=1 (set by cmd_part) skips the sidecar
  # block. IRC `/part` should leave only the current channel; the
  # sidecar (#general lobby) should keep running. cmd_teardown without
  # this flag is the "kill everything in this scope tree" semantic.
  local _sidecar_scope="${AIRC_WRITE_DIR}.general"
  if [ "${AIRC_TEARDOWN_PART_ONLY:-0}" = "1" ]; then
    : # cmd_part path — skip sidecar
  elif [ -d "$_sidecar_scope" ]; then
    if [ -f "$_sidecar_scope/host_gist_id" ] && command -v gh >/dev/null 2>&1; then
      local _td_sc_gist; _td_sc_gist=$(cat "$_sidecar_scope/host_gist_id" 2>/dev/null)
      if [ -n "$_td_sc_gist" ]; then
        if gh gist delete "$_td_sc_gist" --yes >/dev/null 2>&1; then
          echo "  deleted sidecar #general gist: $_td_sc_gist"
        fi
        rm -f "$_sidecar_scope/host_gist_id"
      fi
    fi
    if [ -f "$_sidecar_scope/airc.pid" ]; then
      local _sc_pids; _sc_pids=$(cat "$_sidecar_scope/airc.pid" 2>/dev/null | tr '\n' ' ')
      if [ -n "$_sc_pids" ]; then
        local _all_sc="$_sc_pids"
        for _p in $_sc_pids; do
          local _kids; _kids=$(proc_children "$_p" | tr '\n' ' ' || true)
          [ -n "$_kids" ] && _all_sc="$_all_sc $_kids"
        done
        _all_sc=$(echo "$_all_sc" | tr ' ' '\n' | sort -u | grep -v '^$' || true)
        if [ -n "$_all_sc" ]; then
          echo "  killing sidecar scope $_sidecar_scope: $(echo $_all_sc | tr '\n' ' ')"
          kill -9 $_all_sc 2>/dev/null || true
          killed=1
        fi
      fi
      rm -f "$_sidecar_scope/airc.pid"
    fi
    if [ "$flush" = "1" ]; then
      rm -rf "$_sidecar_scope"
    fi
  fi

  # Scope-aware via PID file: cmd_connect wrote its PID(s) to $AIRC_WRITE_DIR/airc.pid.
  # We kill ONLY those PIDs + their descendants. Never touches other scopes.
  local pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ -f "$pidfile" ]; then
    local main_pids
    # `|| true` — same class as #6: if $pidfile is racily removed between the
    # `-f` test and this read, cat+pipefail would abort cmd_teardown before we
    # reach `rm -f` below. Empty main_pids → we fall through cleanly.
    main_pids=$(cat "$pidfile" 2>/dev/null | tr '\n' ' ' || true)
    if [ -n "$main_pids" ]; then
      # Collect descendants (Python listener etc) before killing the parent.
      local all_pids="$main_pids"
      for pid in $main_pids; do
        local kids
        kids=$(proc_children "$pid" | tr '\n' ' ' || true)
        [ -n "$kids" ] && all_pids="$all_pids $kids"
      done
      all_pids=$(echo "$all_pids" | tr ' ' '\n' | sort -u | grep -v '^$' || true)
      # Part-only path: exclude the sidecar's bash + its descendants so
      # `airc part` doesn't sweep them via the primary's child-tree.
      # The sidecar's bash is forked from primary, so pgrep -P picks it
      # up here; without exclusion we'd kill the sidecar in violation
      # of IRC /part semantics (leave one channel, keep others alive).
      if [ "${AIRC_TEARDOWN_PART_ONLY:-0}" = "1" ] && [ -n "$all_pids" ]; then
        local _exclude_pids=""
        local _sc_pidfile="${AIRC_WRITE_DIR}.general/airc.pid"
        if [ -f "$_sc_pidfile" ]; then
          local _sc_pids; _sc_pids=$(cat "$_sc_pidfile" 2>/dev/null | tr '\n' ' ')
          for _scp in $_sc_pids; do
            _exclude_pids="$_exclude_pids $_scp"
            local _scp_kids; _scp_kids=$(proc_children "$_scp" | tr '\n' ' ' || true)
            [ -n "$_scp_kids" ] && _exclude_pids="$_exclude_pids $_scp_kids"
          done
        fi
        if [ -n "$_exclude_pids" ]; then
          local _filtered=""
          for _p in $all_pids; do
            local _skip=0
            for _ex in $_exclude_pids; do
              [ "$_p" = "$_ex" ] && { _skip=1; break; }
            done
            [ "$_skip" = "0" ] && _filtered="$_filtered $_p"
          done
          all_pids=$(echo "$_filtered" | tr ' ' '\n' | grep -v '^$' || true)
        fi
      fi
      if [ -n "$all_pids" ]; then
        echo "  killing scope $AIRC_WRITE_DIR: $(echo $all_pids | tr '\n' ' ')"
        kill -9 $all_pids 2>/dev/null || true
        killed=1
      fi
    fi
    rm -f "$pidfile" 2>/dev/null
  fi

  # Scope-path catch-all: ANY process whose argv contains this scope's
  # path is ours, even if airc.pid never knew about it. Catches:
  #   - Python handshake / monitor_formatter / bearer_cli children
  #     whose parent died before airc.pid was updated.
  #   - Subshells reparented to init that still hold scope state.
  #   - Stale processes from multi-bounce sessions.
  # pgrep -f matches command + arguments (not env). Every airc python
  # subprocess passes scope paths on its argv (--peers-dir,
  # --offset-file, etc), so cmdline match catches them all. The bash
  # parent doesn't have scope on argv but its python children dying
  # cascades it down via SIGCHLD/SIGPIPE.
  # Skipped under AIRC_TEARDOWN_PART_ONLY (cmd_part shouldn't sweep).
  if [ "${AIRC_TEARDOWN_PART_ONLY:-0}" != "1" ]; then
    local _scope_path_pids
    _scope_path_pids=$(pgrep -f "$AIRC_WRITE_DIR" 2>/dev/null | sort -un)
    if [ -n "$_scope_path_pids" ]; then
      # Exclude our own pid + parent (this very teardown subshell) so
      # we don't suicide before completing the cleanup.
      local _self_pid="$$"
      local _parent_pid="$PPID"
      local _filter_pids=""
      for _p in $_scope_path_pids; do
        [ "$_p" = "$_self_pid" ] && continue
        [ "$_p" = "$_parent_pid" ] && continue
        _filter_pids="$_filter_pids $_p"
      done
      if [ -n "$_filter_pids" ]; then
        echo "  killing scope-path-tagged orphans: $(echo $_filter_pids | tr '\n' ' ')"
        kill -9 $_filter_pids 2>/dev/null || true
        killed=1
      fi
    fi
  fi

  # Brief pause to let the kernel reparent any airc python listener children
  # to init (PID 1) after we killed their bash parent. Then reap orphans.
  [ "$killed" = "1" ] && sleep 0.5

  # Free the TCP port we were listening on. Kill any python socket listener
  # that's now orphaned (parent=1). Don't touch anything else.
  local ports="${AIRC_PORT:-7547}"
  [ "$ports" != "7547" ] && ports="$ports 7547"
  for port in $ports; do
    local lpids
    lpids=$(port_listeners "$port" || true)
    for lpid in $lpids; do
      # `|| true` on both — $lpid came from lsof a moment ago; if the process
      # exited in the interim, `ps -p` returns 1 and pipefail/errexit would
      # abort the port-reap loop mid-scan, leaving later ports unchecked.
      # Empty parent/cmd → the `if` below falls through, which is correct.
      local parent; parent=$(proc_parent "$lpid" || true)
      local cmd; cmd=$(proc_cmdline "$lpid" || true)
      # Reap if orphaned AND is a python socket listener.
      if [ "$parent" = "1" ] && echo "$cmd" | grep -q "socket.SOCK_STREAM"; then
        echo "  freeing orphaned port $port (pid $lpid)"
        kill -9 "$lpid" 2>/dev/null || true
        killed=1
      fi
    done
  done

  if [ "$flush" = "1" ]; then
    # Wipe current tier's state. Leaves the other tier alone.
    local dir="$AIRC_WRITE_DIR"
    if [ -n "$dir" ] && [ -d "$dir" ]; then
      echo "  flushing state: $dir"
      rm -rf "$dir"
    fi
  fi

  [ "$killed" = "0" ] && echo "  No airc processes running." || echo "  Teardown complete."
}

cmd_disconnect() {
  # "Leave the room" — kill running processes in scope, then clear only the
  # host-pairing fields from config.json. Your identity (name + keys), peers
  # list, and message history are all preserved. Next `airc connect` (no
  # args) starts fresh host mode instead of auto-resuming the prior pairing.
  # Use when you want to switch to a different mesh or host a new one, but
  # keep your agent identity stable.
  case "${1:-}" in
    -h|--help)
      echo "Usage:"
      echo "  airc disconnect / quit    leave the mesh: kill processes + clear host pairing"
      echo "                            (identity, peers, message history preserved)"
      return 0 ;;
  esac
  cmd_teardown >/dev/null 2>&1 || true
  if [ -f "$CONFIG" ]; then
    "$AIRC_PYTHON" -c "
import json
try:
    c = json.load(open('$CONFIG'))
    for k in ('host_target', 'host_name', 'host_airc_home', 'host_port', 'host_ssh_pub'):
        c.pop(k, None)
    json.dump(c, open('$CONFIG', 'w'), indent=2)
except Exception:
    pass
" 2>/dev/null || true
  fi
  echo "  Disconnected. Identity preserved. Next 'airc connect' starts fresh (not a resume)."
}
