# Sourced by airc. cmd_status + cmd_logs — introspection verbs.
#
# Functions exported back to airc's dispatch:
#   cmd_status  — human-readable liveness snapshot. Fast (no network)
#                 by default; `--probe` adds an SSH host check.
#   cmd_logs    — tail messages.jsonl. Falls back to host's log via
#                 ssh when not the host.
#
# Both are read-only introspection and share no helpers, but live in
# the same conceptual group ("what is happening?"). External cross-
# references (call-time): die, ensure_init, get_config_val, relay_ssh,
# remote_home, MESSAGES, AIRC_PYTHON.
#
# Extracted from airc as part of #152 Phase 3 file split.

cmd_status() {
  # Human-readable liveness view. Fast — no network calls by default; `--probe`
  # opts into a 3s SSH reachability check.
  case "${1:-}" in
    -h|--help)
      echo "Usage:"
      echo "  airc status            print local liveness snapshot (fast)"
      echo "  airc status --probe    add a 3s SSH reachability check to the host"
      return 0 ;;
  esac
  ensure_init
  local probe=0
  [ "${1:-}" = "--probe" ] && probe=1

  local my_name host_target host_name host_port
  my_name=$(get_name)
  host_target=$(get_config_val host_target "")
  host_name=$(get_config_val host_name "")
  host_port=$(get_config_val host_port 7547)

  echo "  airc status — scope $AIRC_WRITE_DIR"

  # Identity + role line.
  if [ -n "$host_target" ]; then
    echo "  identity:    $my_name (joiner of ${host_name:-?} @ ${host_target}:${host_port})"
  else
    local my_port; my_port="${AIRC_PORT:-7547}"
    [ -f "$AIRC_WRITE_DIR/host_port" ] && my_port=$(cat "$AIRC_WRITE_DIR/host_port" 2>/dev/null)
    echo "  identity:    $my_name (hosting on port ${my_port})"
  fi

  # Channel participation (#149). Post-Phase-2B.3 the canonical source
  # is config.json's subscribed_channels; first element is the default
  # channel that cmd_send stamps. Display the list with a marker for
  # the default.
  local _channels; _channels=$("$AIRC_PYTHON" -m airc_core.config read_channels --config "$CONFIG" 2>/dev/null)
  if [ -n "$_channels" ]; then
    local _default; _default=$(echo "$_channels" | head -1)
    local _rest; _rest=$(echo "$_channels" | tail -n +2 | tr '\n' ',' | sed 's/,$//' | sed 's/,/, #/g')
    if [ -n "$_rest" ]; then
      echo "  channels:    #${_default} (default), #${_rest}"
    else
      echo "  channels:    #${_default}"
    fi
  fi

  # Monitor alive? Read the scope's pidfile — cmd_connect writes its own PID
  # there. pgrep'd descendants (python listener, tail loop) should be children
  # of that PID. If the main PID is gone, the monitor is down.
  local monitor_state="not running"
  local pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ -f "$pidfile" ]; then
    # cmd_connect writes multiple space-separated PIDs on one line (parent +
    # python listener). Monitor is "running" if ANY of them is alive.
    local pids_raw; pids_raw=$(cat "$pidfile" 2>/dev/null | tr '\n' ' ' || true)
    local any_alive=""
    for p in $pids_raw; do
      if kill -0 "$p" 2>/dev/null; then any_alive="$p"; break; fi
    done
    if [ -n "$any_alive" ]; then
      monitor_state="running (PID $any_alive)"
    else
      monitor_state="stale pidfile (PIDs $pids_raw not alive — run 'airc connect' to self-heal)"
    fi
  fi
  echo "  monitor:     $monitor_state"

  # Host reachability. Only meaningful for joiners; opt-in via --probe to keep
  # `airc status` fast by default (SSH connect can hang for seconds).
  if [ -n "$host_target" ] && [ "$probe" = "1" ]; then
    local ssh_key="$IDENTITY_DIR/ssh_key"
    local probe_out
    probe_out=$(ssh -i "$ssh_key" -o StrictHostKeyChecking=accept-new \
                    -o ConnectTimeout=3 -o BatchMode=yes \
                    "$host_target" "echo __REACHABLE__" 2>/dev/null || true)
    if echo "$probe_out" | grep -q '^__REACHABLE__$'; then
      echo "  host:        reachable"
    else
      echo "  host:        UNREACHABLE (ssh timeout or auth failure)"
    fi
  fi

  # Last send / receive timestamps. last_sent is a unix epoch written by
  # cmd_send. last receive: tail the local messages.jsonl for the most recent
  # inbound line (from != $my_name).
  local now; now=$(date +%s)
  if [ -f "$AIRC_WRITE_DIR/last_sent" ]; then
    local ls; ls=$(cat "$AIRC_WRITE_DIR/last_sent" 2>/dev/null)
    if [ -n "$ls" ] && [ "$ls" -gt 0 ] 2>/dev/null; then
      echo "  last send:   $(( now - ls ))s ago"
    else
      echo "  last send:   never"
    fi
  else
    echo "  last send:   never"
  fi

  # Last receive: read the bearer-attested state file written by
  # bearer_cli recv on each event (Phase 2c, #270 fix). The previous
  # implementation parsed messages.jsonl for the most recent inbound
  # ts, but that lied for a 30+ minute mesh outage in #270 — the local
  # mirror said "fresh" while the bearer was actually wedged. The
  # bearer-state file is the truth: it's only updated when an event
  # actually flows off the wire.
  local bearer_state="$AIRC_WRITE_DIR/bearer_state.json"
  if [ -f "$bearer_state" ]; then
    local _bs_summary
    _bs_summary=$("$AIRC_PYTHON" -c "
import json, sys, time
try:
    s = json.load(open('$bearer_state'))
except Exception as e:
    print(f'unreadable: {e}'); sys.exit(0)
ts = s.get('last_recv_ts')
kind = s.get('kind', '?')
diag = s.get('diag', '')
total = s.get('events_total', 0)
if ts is None:
    print(f'awaiting first event (bearer={kind}, {diag})')
else:
    age = int(time.time() - float(ts))
    print(f'{age}s ago via {kind} ({total} events; {diag})')
" 2>/dev/null)
    echo "  bearer:      ${_bs_summary:-unreadable}"
  elif [ -n "$host_target" ]; then
    # Joiner with no bearer state — monitor never came up or hasn't
    # opened the bearer yet. This was previously a silent gap: status
    # claimed "monitor running" while the inbound path was dead.
    echo "  bearer:      no state file ($AIRC_WRITE_DIR/bearer_state.json) — monitor not yet streaming"
  else
    # Host: no inbound bearer (we ARE the host). The bearer-state file
    # is a joiner-side artifact; on a host the local messages.jsonl IS
    # the source of truth, but we still surface that explicitly.
    echo "  bearer:      n/a (this scope is hosting; inbound is local log)"
  fi

  # Pending queue — how many sends are waiting for a drain. Populated by
  # cmd_send's wire-failure branch; drained by flush_pending_loop.
  local pending="$AIRC_WRITE_DIR/pending.jsonl"
  local pending_count=0
  [ -f "$pending" ] && pending_count=$(grep -c '^.' "$pending" 2>/dev/null || echo 0)
  if [ "$pending_count" -gt 0 ]; then
    echo "  queue:       ${pending_count} pending (auto-retries every ~5s)"
  else
    echo "  queue:       empty"
  fi

  # Reminder state
  local reminder_file="$AIRC_WRITE_DIR/reminder"
  if [ -f "$reminder_file" ]; then
    local rv; rv=$(cat "$reminder_file" 2>/dev/null)
    if [ "$rv" = "0" ]; then
      echo "  reminder:    paused"
    elif [ -n "$rv" ] && [ "$rv" -gt 0 ] 2>/dev/null; then
      echo "  reminder:    every ${rv}s"
    fi
  else
    echo "  reminder:    off"
  fi
}

cmd_logs() {
  ensure_init
  local count="${1:-20}"
  local host_target
  host_target=$(get_config_val host_target "")

  local raw
  if [ -n "$host_target" ]; then
    local rhome; rhome=$(remote_home)
    raw=$(relay_ssh "$host_target" "tail -${count} $rhome/messages.jsonl 2>/dev/null" 2>/dev/null) || true
  else
    raw=$(tail -"$count" "$MESSAGES" 2>/dev/null) || true
  fi
  echo "$raw" | "$AIRC_PYTHON" -c "
import sys, json
for line in sys.stdin:
    try:
        m = json.loads(line.strip())
        print(f\"[{m.get('ts','')}] {m.get('from','?')}: {m.get('msg','')}\")
    except: pass
"
}
