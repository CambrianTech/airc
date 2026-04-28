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

  if [ -s "$MESSAGES" ]; then
    local last_rx_ts
    last_rx_ts=$(PEERS_DIR="$PEERS_DIR" MY_NAME="$my_name" "$AIRC_PYTHON" -c "
import sys, json, os, calendar, time
name = os.environ.get('MY_NAME', '')
last_ts = None
try:
    with open('$MESSAGES') as f:
        for line in f:
            try:
                m = json.loads(line)
                if m.get('from') and m.get('from') != name and m.get('from') != 'airc':
                    last_ts = m.get('ts')
            except: pass
except: pass
if last_ts:
    # ts is ISO8601 UTC (Z-suffix). Convert to epoch.
    try:
        t = time.strptime(last_ts.replace('Z',''), '%Y-%m-%dT%H:%M:%S')
        print(int(calendar.timegm(t)))
    except: print('')
else:
    print('')
" 2>/dev/null)
    if [ -n "$last_rx_ts" ]; then
      echo "  last recv:   $(( now - last_rx_ts ))s ago"
    else
      echo "  last recv:   never"
    fi
  else
    echo "  last recv:   never"
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
