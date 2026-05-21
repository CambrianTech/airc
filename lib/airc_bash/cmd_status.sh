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
# remote_home, MESSAGES.
#
# Extracted from airc as part of #152 Phase 3 file split.

_airc_monitor_health_report() {
  local mode="${1:-all}"
  local args=(transport health --home "$AIRC_WRITE_DIR" --config "$CONFIG")
  [ "$mode" = "degraded-only" ] && args+=(--degraded-only)
  "$(airc_core_bin)" "${args[@]}" 2>/dev/null | sed 's/^/  /' || true
}

_airc_collaboration_health_report() {
  # Local transport health is not the same as collaboration health. A
  # self-healed host can have fresh bearer heartbeats while nobody else is
  # paired to this mesh. Surface that split-brain shape explicitly.
  local _client_id; _client_id=$(airc_client_id 2>/dev/null || true)
  "$(airc_core_bin)" collaboration status \
    --home "$AIRC_WRITE_DIR" --my-name "$(get_name)" --client-id "$_client_id"
}

_airc_rust_local_status_report() {
  local _rs
  _rs=$(airc_core_bin)

  local _room_out
  if ! _room_out=$("$_rs" --home "$AIRC_WRITE_DIR" room 2>/dev/null); then
    echo "  data-plane:  rust-local unavailable (run: airc-core init)"
    return 0
  fi

  local _room _wire _channel
  _room=$(printf '%s\n' "$_room_out" | sed -n 's/^room:[[:space:]]*//p' | head -1)
  _wire=$(printf '%s\n' "$_room_out" | sed -n 's/^wire:[[:space:]]*//p' | head -1)
  _channel=$(printf '%s\n' "$_room_out" | sed -n 's/^channel:[[:space:]]*//p' | head -1)
  [ -z "$_room" ] && _room="?"
  [ -z "$_channel" ] && _channel="?"

  local _frame_count=0
  if [ -n "$_wire" ] && [ -f "$_wire/frames.jsonl" ]; then
    _frame_count=$(grep -c '^.' "$_wire/frames.jsonl" 2>/dev/null || echo 0)
  fi

  local _peer_count=0
  local _peer_out
  _peer_out=$("$_rs" --home "$AIRC_WRITE_DIR" peer list 2>/dev/null || true)
  _peer_count=$(printf '%s\n' "$_peer_out" | grep -Ec '^[0-9a-fA-F-]{36}[[:space:]]' 2>/dev/null || echo 0)

  echo "  data-plane:  rust-local active (#${_room}, channel ${_channel})"
  if [ -n "$_wire" ]; then
    echo "  rust wire:   ${_wire} (${_frame_count} frame(s), ${_peer_count} peer(s) enrolled)"
  fi
}

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
  local _channels; _channels=$("$(airc_core_bin)" config read-channels --home "$AIRC_WRITE_DIR" --config "$CONFIG" 2>/dev/null)
  if [ -n "$_channels" ]; then
    local _default; _default=$(echo "$_channels" | head -1)
    local _rest; _rest=$(echo "$_channels" | tail -n +2 | tr '\n' ',' | sed 's/,$//' | sed 's/,/, #/g')
    if [ -n "$_rest" ]; then
      echo "  channels:    #${_default} (default), #${_rest}"
    else
      echo "  channels:    #${_default}"
    fi
  fi
  _airc_collaboration_health_report
  _airc_rust_local_status_report

  # Scope monitor alive? Use the shared sandbox-robust helper
  # (_monitor_alive_with_bearer_fallback in airc top-level). Phase 1 =
  # kill -0 against airc.pid (canonical, fast); phase 2 = scope-specific
  # monitor_formatter process evidence (covers Codex sandbox kill -0
  # blindness without treating bearer_state freshness as a Monitor).
  # The helper is read-only (doesn't prune the pidfile the way the older
  # prune_pidfile_and_count did, which would silently corrupt state when
  # phase 1 was wrong).
  local monitor_state="not running"
  local pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ "$(_monitor_alive_with_bearer_fallback "$pidfile")" = "yes" ]; then
    if [ -f "$pidfile" ]; then
      local first_alive; first_alive=$(_airc_pidfile_first_live_monitor_pid "$pidfile")
      # Distinguish "alive per kill -0" (we have a verified PID) from
      # "alive per formatter process only" (kill -0 blind against the
      # pidfile, but the scope's monitor_formatter is visible by argv).
      if [ -n "$first_alive" ] && kill -0 "$first_alive" 2>/dev/null; then
        monitor_state="AIRC background process running for scope (PID $first_alive)"
      else
        local _fmt_pid; _fmt_pid=$(_airc_scope_monitor_formatter_pids "$AIRC_WRITE_DIR" | head -1)
        monitor_state="AIRC formatter running for scope (formatter PID ${_fmt_pid:-?}; pidfile not visible/alive)"
      fi
    fi
  elif [ -f "$pidfile" ]; then
    monitor_state="stale pidfile (no live PIDs — run 'airc join' to self-heal)"
  elif [ -f "$AIRC_WRITE_DIR/.cold_start_t0" ]; then
    # Cold-start anchor exists but no airc.pid yet — airc join is
    # still in the slow-discovery / handshake / takeover phase. Tell
    # the user that explicitly so "not running" doesn't read as
    # "broken." Cleared by _join_phase_done once monitor stream
    # attaches. Stale-marker guard: if the anchor is >10min old we
    # treat it as orphaned (a join crashed without cleanup); fall
    # back to "not running" so the user gets the actionable message
    # instead of a perpetually-rising "starting (t+50000s)".
    local _t0; _t0=$(cat "$AIRC_WRITE_DIR/.cold_start_t0" 2>/dev/null || echo 0)
    case "$_t0" in ''|*[!0-9]*) _t0=0 ;; esac
    local _now _elapsed
    _now=$(date +%s 2>/dev/null) || _now=0
    _elapsed=$(( _now - _t0 ))
    [ "$_elapsed" -lt 0 ] 2>/dev/null && _elapsed=0
    if [ "$_elapsed" -le 600 ] 2>/dev/null; then
      monitor_state="starting (airc join cold-start in progress, t+${_elapsed}s)"
    fi
  fi
  echo "  airc process: $monitor_state"
  _airc_monitor_health_report all

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

  # Legacy bearer state is not the routine data plane anymore. Plain
  # messages and inbox reads use the Rust local substrate above; the gh
  # bearer remains visible here because it still covers invite/remote
  # migration paths and stale pending queues.
  local bearer_state="$AIRC_WRITE_DIR/bearer_state.json"
  if [ -f "$bearer_state" ]; then
    local _bs_summary
    _bs_summary=$("$(airc_core_bin)" bearer-state --summary "$bearer_state" 2>/dev/null)
    echo "  bearer:      ${_bs_summary:-unreadable} (legacy gh invite/remote)"
  elif [ -n "$host_target" ]; then
    echo "  bearer:      no state file ($AIRC_WRITE_DIR/bearer_state.json) — legacy gh invite/remote not yet streaming"
  else
    echo "  bearer:      n/a (rust-local is routine data-plane; gh only invite/remote)"
  fi

  # gh auth health. This is deliberately labelled as invite/remote
  # health so a GitHub secondary limit does not read as "AIRC chat is
  # down" when same-machine Rust-local delivery is working.
  if command -v gh >/dev/null 2>&1; then
    # Use the centralized auth detector instead of raw `gh auth status`
    # so status reads the OK cache and does not turn frequent health
    # checks into /user traffic that trips GitHub's secondary limiter.
    local _gh_state
    _gh_state="$(airc_detect_gh_auth_state 2>/dev/null || echo invalid)"
    case "$_gh_state" in
      ok)
        echo "  gh auth:     ok (invite/remote only; rust-local unaffected)"
        ;;
      rate_limited)
        echo "  gh auth:     RATE-LIMITED (invite/remote only; rust-local unaffected)"
        ;;
      env_token_invalid)
        echo "  gh auth:     ✗ INVALID GH_TOKEN (invite/remote only) — unset/fix GH_TOKEN"
        ;;
      *)
        echo "  gh auth:     ✗ INVALID (invite/remote only) — run 'gh auth login -h github.com'"
        ;;
    esac
  else
    echo "  gh auth:     gh CLI not installed (invite/remote unavailable; rust-local unaffected)"
  fi

  # Pending queue is legacy gh bearer work waiting for remote/invite
  # drain. Plain local sends do not use this queue.
  local pending="$AIRC_WRITE_DIR/pending.jsonl"
  local pending_count=0
  [ -f "$pending" ] && pending_count=$(grep -c '^.' "$pending" 2>/dev/null || echo 0)
  if [ "$pending_count" -gt 0 ]; then
    local _gh_wait=0
    _gh_wait=$("$(airc_core_bin)" gh wait-seconds 2>/dev/null || echo 0)
    if [ "${_gh_wait:-0}" -gt 0 ] 2>/dev/null; then
      echo "  queue:       ${pending_count} pending legacy gh item(s) (paused for ${_gh_wait}s; rust-local unaffected)"
    else
      echo "  queue:       ${pending_count} pending legacy gh item(s) (governed auto-drain; rust-local unaffected)"
    fi
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
  # Parse optional --since <ts|Ns|Nm|Nh> first, then positional count.
  # --since enables incremental polling — agents that run `airc logs`
  # every prompt-cycle (Codex's satellite mode, Claude polling between
  # Monitor events, etc.) re-ingest only NEW messages instead of the
  # full tail. This is the diff between O(N) per-turn context burn
  # and O(delta) per-turn — Codex hit context exhaustion 2026-05-02
  # because polling `logs 50` every turn re-injected ~7K tokens.
  local since=""
  local output_json=0
  local positional=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --json)
        output_json=1; shift ;;
      --since)
        [ -n "${2:-}" ] || die "--since requires an argument (ISO timestamp or relative like 60s/5m/1h)"
        since="$2"; shift 2 ;;
      --since=*)
        since="${1#--since=}"; shift ;;
      -h|--help)
        echo "Usage: airc logs [N] [--since <ts|Ns|Nm|Nh>] [--json]"
        echo "  N           tail this many recent messages (default 20)"
        echo "  --since X   filter to messages newer than X. X can be:"
        echo "              ISO timestamp (2026-05-02T19:30:00Z)"
        echo "              relative offset (60s, 5m, 1h, 2d)"
        echo "              For incremental polling — re-poll using the"
        echo "              ts of the last message you saw."
        echo "  --json      emit now_utc, since, count, and event objects."
        return 0 ;;
      *) positional+=("$1"); shift ;;
    esac
  done
  set -- "${positional[@]+"${positional[@]}"}"
  local count="${1:-20}"
  # Validate count: positive integer (caught 2026-04-29:
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
  set -- render --since "$since" --count "$count"
  if [ "$output_json" -eq 1 ]; then
    set -- "$@" --json
  fi
  echo "$raw" | "$(airc_core_bin)" log "$@"
}

cmd_inbox() {
  ensure_init

  local cursor_file="$AIRC_WRITE_DIR/inbox_cursor"
  local since=""
  local count="500"
  local peek=0
  local quiet_empty="${AIRC_INBOX_QUIET_EMPTY:-0}"
  local exclude_self="${AIRC_INBOX_EXCLUDE_SELF:-0}"

  while [ $# -gt 0 ]; do
    case "$1" in
      --since)
        [ -n "${2:-}" ] || die "--since requires an argument (ISO timestamp or relative like 60s/5m/1h)"
        since="$2"; shift 2 ;;
      --since=*)
        since="${1#--since=}"; shift ;;
      --count|-n)
        [ -n "${2:-}" ] || die "--count requires a positive integer"
        count="$2"; shift 2 ;;
      --count=*)
        count="${1#--count=}"; shift ;;
      --peek)
        peek=1; shift ;;
      --quiet-empty)
        quiet_empty=1; shift ;;
      --exclude-self)
        exclude_self=1; shift ;;
      --reset)
        "$(airc_core_bin)" log inbox-reset \
          --home "$AIRC_WRITE_DIR" --cursor-file "$cursor_file"
        return 0 ;;
      -h|--help)
        echo "Usage: airc inbox [--peek] [--reset] [--since <ts|Ns|Nm|Nh>] [--count N]"
        echo "  Shows unread messages since this scope's last inbox check."
        echo "  Advances a per-scope cursor unless --peek is set."
        echo "  --quiet-empty suppresses the 'No new airc messages' line."
        echo "  --exclude-self hides messages from this identity."
        echo "  Alias: airc poll, airc codex-poll (quiet + exclude-self by default)"
        return 0 ;;
      *) die "Unknown inbox option: $1" ;;
    esac
  done

  case "$count" in
    ''|*[!0-9]*) die "inbox --count must be a positive integer (got '$count')" ;;
    0)           die "inbox --count must be ≥ 1 (got '$count')" ;;
  esac

  if [ -z "$since" ]; then
    since=""
  fi

  _airc_try_rust_inbox_read() {
    # Plain inbox reads follow plain msg sends onto the Rust local
    # substrate. Legacy relative-time/self-filter modes stay on the
    # old log reader until those filters are represented in the Rust
    # transcript API.
    [ -z "$since" ] || return 1
    [ "$exclude_self" = "0" ] || return 1

    local rust_cursor_file="$AIRC_WRITE_DIR/inbox_cursor.rust"
    local rust_args=(--home "$AIRC_WRITE_DIR" inbox --limit "$count")
    if [ -s "$rust_cursor_file" ]; then
      local cursor_lamport cursor_event_id
      read -r cursor_lamport cursor_event_id < "$rust_cursor_file" || true
      if [ -n "${cursor_lamport:-}" ] && [ -n "${cursor_event_id:-}" ]; then
        rust_args+=(--since-lamport "$cursor_lamport" --since-event-id "$cursor_event_id")
      fi
    fi

    local rust_out
    if ! rust_out=$("$(airc_core_bin)" "${rust_args[@]}" 2>&1); then
      printf '%s\n' "$rust_out" >&2
      return 2
    fi

    local cursor_line
    cursor_line=$(printf '%s\n' "$rust_out" | grep '^cursor: lamport=' | tail -1 || true)
    if [ "$peek" -eq 0 ] && [ -n "$cursor_line" ]; then
      local next_lamport next_event_id
      next_lamport=$(printf '%s\n' "$cursor_line" | sed -n 's/^cursor: lamport=\([0-9][0-9]*\) event_id=.*/\1/p')
      next_event_id=$(printf '%s\n' "$cursor_line" | sed -n 's/^cursor: lamport=[0-9][0-9]* event_id=\([^ ]*\).*/\1/p')
      if [ -n "$next_lamport" ] && [ -n "$next_event_id" ]; then
        printf '%s %s\n' "$next_lamport" "$next_event_id" > "$rust_cursor_file"
      fi
    fi

    if [ "$quiet_empty" = "1" ] && [ "$rust_out" = "(no events)" ]; then
      return 0
    fi
    printf '%s\n' "$rust_out" | sed '/^cursor: lamport=/d'
    return 0
  }

  local _rust_inbox_rc=0
  _airc_try_rust_inbox_read || _rust_inbox_rc=$?
  if [ "$_rust_inbox_rc" -eq 0 ]; then
    return 0
  elif [ "$_rust_inbox_rc" -eq 2 ]; then
    return 1
  fi

  local out
  local inbox_args=(log inbox-read --home "$AIRC_WRITE_DIR" --cursor-file "$cursor_file" --count "$count")
  [ -n "$since" ] && inbox_args+=(--since "$since")
  [ "$peek" -eq 1 ] && inbox_args+=(--peek)
  [ "$quiet_empty" = "1" ] && inbox_args+=(--quiet-empty)
  if [ "$exclude_self" = "1" ]; then
    local _client_id; _client_id=$(airc_client_id 2>/dev/null || true)
    inbox_args+=(--exclude-self --my-name "$(get_name)" --client-id "$_client_id")
  fi
  if ! out=$("$(airc_core_bin)" "${inbox_args[@]}" 2>&1); then
    printf '%s\n' "$out" >&2
    return 1
  fi

  _airc_monitor_health_report degraded-only
  printf '%s\n' "$out"
}

cmd_codex_hook() {
  ensure_init

  local _airc_core
  _airc_core=$(airc_core_bin 2>/dev/null || true)
  if [ -z "$_airc_core" ]; then
    die "airc-core is required for codex-hook; build/install the Rust CLI first"
  fi

  local sub="${1:-}"
  shift || true
  case "$sub" in
    user-prompt-submit)
      local _has_cursor_file=0
      local _arg
      for _arg in "$@"; do
        case "$_arg" in
          --cursor-file|--cursor-file=*) _has_cursor_file=1 ;;
        esac
      done
      if [ "$_has_cursor_file" = "1" ]; then
        "$_airc_core" --home "$AIRC_WRITE_DIR" codex-hook user-prompt-submit "$@"
      else
        "$_airc_core" --home "$AIRC_WRITE_DIR" codex-hook user-prompt-submit \
          --cursor-file "$AIRC_WRITE_DIR/codex_hook_cursor.json" \
          "$@"
      fi
      ;;
    install-hooks|uninstall-hooks)
      "$_airc_core" codex-hook "$sub" "$@"
      ;;
    -h|--help|'')
      echo "Usage: airc codex-hook {user-prompt-submit|install-hooks|uninstall-hooks}"
      echo "  Codex lifecycle hook adapter backed by the Rust AIRC event store."
      ;;
    *)
      die "Unknown codex-hook command: $sub" ;;
  esac
}

cmd_codex_start() {
  local _log="$AIRC_WRITE_DIR/codex-airc.log"
  local _pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ "$(_monitor_alive_with_bearer_fallback "$_pidfile")" = "yes" ] \
      && _join_transport_health_ok \
      && ! _join_scope_has_duplicate_transport; then
    echo "airc join: already joined in this scope."
    echo ""
    echo "Status"
    echo "------"
    cmd_status
    echo ""
    echo "Inbox"
    echo "-----"
    AIRC_INBOX_QUIET_EMPTY=1 AIRC_INBOX_EXCLUDE_SELF=1 cmd_inbox --count 10 || true
    return 0
  fi

  local _started_at
  _started_at=$(date +%s)
  local _airc_core
  _airc_core=$(airc_core_bin 2>/dev/null || true)
  if [ -z "$_airc_core" ]; then
    die "airc-core is required for codex-start; build/install the Rust CLI first"
  fi
  "$_airc_core" codex-start \
    --airc "$0" \
    --home "$AIRC_WRITE_DIR" \
    --log "$_log" \
    -- "$@"

  # Wait for the detached child to write fresh scope process evidence
  # before printing status. A fixed sleep was too short when `airc join`
  # had to self-heal duplicate same-scope generations: Codex printed
  # "not running" while the detached child was still reaping/restarting.
  local _wait_sec="${AIRC_CODEX_START_WAIT_SEC:-45}"
  local _i _mtime
  for _i in $(seq 1 "$_wait_sec"); do
    if [ -f "$_pidfile" ]; then
      _mtime=$(file_mtime "$_pidfile" 2>/dev/null || echo 0)
      if [ "${_mtime:-0}" -ge "$_started_at" ] 2>/dev/null \
          && [ -n "$(_airc_pidfile_first_live_monitor_pid "$_pidfile")" ]; then
        break
      fi
    fi
    sleep 1
  done
  echo ""
  echo "Status"
  echo "------"
  cmd_status
  echo ""
  echo "Inbox"
  echo "-----"
  AIRC_INBOX_QUIET_EMPTY=1 AIRC_INBOX_EXCLUDE_SELF=1 cmd_inbox --count 10 || true
}
