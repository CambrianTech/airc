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

  # Monitor alive? Use the shared sandbox-robust helper
  # (_monitor_alive_with_bearer_fallback in airc top-level). Phase 1 =
  # kill -0 against airc.pid (canonical, fast); phase 2 = bearer-state
  # freshness fallback (covers Codex sandbox kill -0 blindness — see
  # #370/#371/#372). The helper is read-only (doesn't prune the pidfile
  # the way the older prune_pidfile_and_count did, which would silently
  # corrupt state when phase 1 was wrong).
  local monitor_state="not running"
  local pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ "$(_monitor_alive_with_bearer_fallback "$pidfile")" = "yes" ]; then
    if [ -f "$pidfile" ]; then
      local first_alive; first_alive=$(awk '{print $1}' "$pidfile" 2>/dev/null)
      # Distinguish "alive per kill -0" (we have a verified PID) from
      # "alive per bearer-state-only" (kill -0 blind, but bearer-recv
      # child is provably writing to bearer_state). For the latter,
      # surface the diagnostic so a Carl debugging "why does pid X
      # show running when it's not in ps" has the answer.
      if kill -0 "$first_alive" 2>/dev/null; then
        monitor_state="running (PID $first_alive)"
      else
        # Walk bearer_state to find which channel is freshest, for the
        # informational message. (The helper already proved freshness;
        # we re-check just to extract the age + channel name.)
        local _bs_summary; _bs_summary=$("$AIRC_PYTHON" -c "
import json, glob, time
fresh = []
for path in glob.glob('$AIRC_WRITE_DIR/bearer_state.*.json'):
    try:
        s = json.load(open(path))
    except Exception:
        continue
    ts = s.get('last_recv_ts')
    if ts:
        ch = path.split('bearer_state.', 1)[1].rsplit('.json', 1)[0]
        fresh.append((int(time.time() - float(ts)), ch))
if fresh:
    fresh.sort()
    age, ch = fresh[0]
    print(f'{age}s via #{ch}')
" 2>/dev/null)
        monitor_state="likely-alive (${_bs_summary:-bearer-state fresh}; kill -0 blind in this sandbox — see #370)"
      fi
    fi
  elif [ -f "$pidfile" ]; then
    monitor_state="stale pidfile (no live PIDs — run 'airc connect' to self-heal)"
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

  # gh auth health — surface mid-session token expiry so users have
  # a one-line diagnostic instead of mysterious silent failures.
  # The substrate is gh-as-bearer; when gh's keyring goes invalid,
  # everything stops working but nothing surfaces unless they look here.
  if command -v gh >/dev/null 2>&1; then
    if gh auth status >/dev/null 2>&1; then
      echo "  gh auth:     ok"
    elif gh api rate_limit >/dev/null 2>&1; then
      # Token works (rate_limit reachable); /user got 403'd by secondary
      # rate limit and gh misreports it as 'token invalid'. Issue #341.
      echo "  gh auth:     RATE-LIMITED (secondary; token is fine — wait 5-15 min)"
    else
      echo "  gh auth:     ✗ INVALID — run 'gh auth login -h github.com' to fix"
    fi
  else
    echo "  gh auth:     gh CLI not installed"
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
  # Validate count: positive integer (ideem-local-4bef caught 2026-04-29:
  # 'airc logs 0' and 'airc logs notanumber' silently exited 0 with no
  # output). Tail with N=0 prints nothing; with non-numeric, tail errors
  # and we swallow it.
  case "$count" in
    ''|*[!0-9]*) die "logs count must be a positive integer (got '$count')" ;;
    0)           die "logs count must be ≥ 1 (got '$count')" ;;
  esac
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
