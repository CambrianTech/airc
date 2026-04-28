# Sourced by airc. cmd_send + cmd_ping — outbound message verbs.
#
# Functions exported back to airc's dispatch:
#   cmd_send  — broadcast to current room, or DM via @peer prefix.
#               Handles --room, --to, queueing on host failure (pending.jsonl
#               + [QUEUED] mirror in messages.jsonl), and the "speak as" rewrite
#               for sidecar scopes.
#   cmd_ping  — liveness probe wrapped as a regular signed [PING:] message,
#               so older airc clients without auto-pong support degrade
#               gracefully (they just log it).
#
# External cross-references (resolved at call time): die, ensure_init,
# get_config_val, set_config_val, relay_ssh, AIRC_HOME, MESSAGES,
# resolve_name, get_host, _hash, plus airc_core.* python modules
# (airc_core.message, airc_core.queue) for envelope construction.
#
# Extracted from airc as part of #152 Phase 3 file split. Joel 2026-04-27:
# "1) simplify and modularize 2) build host logic correctly 3) never
# ever again make 5000 line dumbass designs." This pulls outbound-message
# concerns out of the bash monolith. Inbound-message handling stays in
# airc top-level (monitor + relay_ssh) for now.

cmd_send() {
  # Chat-room semantics. Default: broadcast to everyone in the current
  # scope's room. Prefix the first arg with '@' to DM a specific peer.
  #   airc send "hello everyone"           → broadcast to current room
  #   airc send @alice "hey"               → DM alice in current room
  #   airc send --room general "hi lobby"  → broadcast to a SIBLING room
  #   airc send --room general @alice "..."→ DM alice via the sibling room
  #
  # --room <name> route (issue #122 follow-up): the multi-room sidecar
  # model means a tab is in #project-room AND #general simultaneously,
  # but each room has its own scope. Without --room support here, sending
  # to a non-current room required `AIRC_HOME=$cwd/.airc.<room> airc msg`,
  # which is nonobvious (vhsm-Claude attempted `airc msg --room general`
  # on 2026-04-26, the unrecognized flag silently became part of the
  # message body — exactly the evidence-eating shape the project rejects).
  #
  # Implementation: parse --room here. If it names a sibling sidecar scope
  # (e.g. ${AIRC_WRITE_DIR}.<name>), re-exec ourselves with AIRC_HOME
  # pointed at that scope so the rest of the function runs there. Errors
  # loudly when the requested room isn't in the user's subscription set
  # — never silently broadcasts to the wrong place.
  local target_room=""
  # --channel <name> (Phase 2B): post-substrate flag that ONLY stamps
  # the envelope's channel field; no scope re-exec. Same scope, same
  # SSH wire, different channel tag. Coexists with --room for now;
  # Phase 2B.3 deletes --room's re-exec path and makes --room an
  # alias for --channel.
  local channel_override=""
  # --internal: best-effort send for internal informational broadcasts
  # ([rename], etc.) where the monitor-down guard is the wrong UX. Append
  # to the local log + return 0 even when the monitor isn't running.
  # Receivers heal via monitor_formatter's host-fallback / next-traffic
  # passes, so missing one event in a quiet scope isn't a correctness
  # issue. Exposed as a flag (not an env var) so call sites are
  # grep-able and the pattern matches the rest of the airc CLI surface.
  local internal=0
  local positional=()
  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        echo "Usage:"
        echo "  airc send <message>                broadcast to default channel"
        echo "  airc send @peer <message>          DM peer"
        echo "  airc send --channel <name> <msg>   stamp channel field"
        echo "  airc send --room <name> <msg>      same as --channel (Phase 2B alias)"
        echo "  airc send --internal <msg>         system event (skips monitor-down guard)"
        return 0 ;;
      --room|-room)
        target_room="${2:-}"
        [ -z "$target_room" ] && die "Usage: airc send --room <name> <message>"
        shift 2 ;;
      --channel|-c)
        channel_override="${2:-}"
        [ -z "$channel_override" ] && die "Usage: airc send --channel <name> <message>"
        shift 2 ;;
      --internal)
        internal=1
        shift ;;
      *) positional+=("$1"); shift ;;
    esac
  done
  set -- "${positional[@]+"${positional[@]}"}"

  if [ -n "$target_room" ]; then
    # Phase 2B.3: --room becomes equivalent to --channel. The pre-mesh
    # sidecar model required re-exec'ing into a sibling scope because
    # each room had its OWN host process; in the post-mesh world there
    # is ONE host per gh account, so all channels share the same wire.
    # Just stamp the channel field and continue.
    if [ -z "$channel_override" ]; then
      channel_override="$target_room"
    fi
  fi

  local first="${1:-}"
  [ -z "$first" ] && die "Usage: airc send <message>  or  airc send @peer <message>"

  # Multi-target DM: collect leading @-tokens (whitespace-separated)
  # and/or comma-separated peers within a single @-token. All forms
  # collapse to a comma-joined CSV in peer_name. The mesh substrate
  # has every peer tailing the same host's messages.jsonl, so one
  # envelope with `to: "alice,bob,carol"` is visible to all three;
  # display shows the CSV recipient list. Receivers can split on
  # comma to detect "is this addressed to me?".
  #
  # IRC norm: /msg user1,user2 message
  # Also supported: airc msg @user1 @user2 message (whitespace)
  # Mixed:          airc msg @user1,user2 @user3 message
  local peer_name="" msg=""
  local _peer_csv=""
  while [ $# -gt 0 ]; do
    case "$1" in
      @*)
        local _p="${1#@}"
        if [ -z "$_peer_csv" ]; then
          _peer_csv="$_p"
        else
          _peer_csv="${_peer_csv},${_p}"
        fi
        shift
        ;;
      *) break ;;
    esac
  done
  if [ -n "$_peer_csv" ]; then
    peer_name="$_peer_csv"
    msg="$*"
    [ -z "$msg" ] && die "Usage: airc send @peer [@peer2 ...] <message>"
  else
    peer_name="all"
    msg="$*"
  fi
  ensure_init

  local my_name ts_val
  my_name=$(get_name)
  ts_val=$(timestamp)

  local escaped_msg
  escaped_msg=$(printf '%s' "$msg" | "$AIRC_PYTHON" -c "import sys,json; print(json.dumps(sys.stdin.read())[1:-1])")

  # Channel: stamp every outbound envelope with the active channel so the
  # monitor display can route by channel uniformly (Phase 2 mesh
  # substrate). Resolution priority (Phase 2B.1):
  #   1. --channel / -c flag (explicit per-call override)
  #   2. config.json subscribed_channels[0]
  #      (Phase 2B substrate — replaces sidecar scopes)
  #   3. legacy room_name file (back-compat for users mid-rollover)
  #   4. literal "general" fallback
  local active_channel=""
  if [ -n "$channel_override" ]; then
    active_channel="$channel_override"
  fi
  if [ -z "$active_channel" ]; then
    active_channel=$("$AIRC_PYTHON" -m airc_core.config default_channel --config "$CONFIG" 2>/dev/null || true)
  fi
  if [ -z "$active_channel" ] && [ -f "$AIRC_WRITE_DIR/room_name" ]; then
    active_channel=$(cat "$AIRC_WRITE_DIR/room_name" 2>/dev/null || true)
  fi
  [ -z "$active_channel" ] && active_channel="general"

  local payload="{\"from\":\"$my_name\",\"to\":\"$peer_name\",\"ts\":\"$ts_val\",\"channel\":\"$active_channel\",\"msg\":\"$escaped_msg\"}"
  local sig; sig=$(sign_message "$payload")
  local full_msg="{\"from\":\"$my_name\",\"to\":\"$peer_name\",\"ts\":\"$ts_val\",\"channel\":\"$active_channel\",\"msg\":\"$escaped_msg\",\"sig\":\"$sig\"}"

  local host_target
  host_target=$(get_config_val host_target "")

  if [ -n "$host_target" ]; then
    local rhome; rhome=$(remote_home)
    # Always mirror locally FIRST so we have an audit trail regardless of
    # what the wire does. If send succeeds: local + remote both have it.
    # If send fails: local has it (user can see it + retry), remote doesn't.
    # This prevents silent loss where both sides forget a message that
    # never arrived.
    echo "$full_msg" >> "$MESSAGES"

    # Fast-path: when tailscale status already reports this peer offline,
    # don't burn 10s on the ssh ConnectTimeout — queue immediately with a
    # cleaner "peer offline in tailnet" marker. flush_pending_loop +
    # monitor reconnect handle the drain automatically when the peer
    # wakes. Skipped entirely for non-CGNAT targets, LAN peers, or when
    # tailscale CLI is unavailable (falls through to normal ssh attempt).
    if is_peer_offline_in_tailnet "$host_target"; then
      echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
      local queue_marker; queue_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[QUEUED to %s — peer offline in tailnet, auto-delivers on wake]"}' \
        "$(timestamp)" "$active_channel" "$peer_name")
      echo "$queue_marker" >> "$MESSAGES"
      date +%s > "$AIRC_WRITE_DIR/last_sent" 2>/dev/null
      rm -f "$AIRC_WRITE_DIR/reminded" 2>/dev/null
      return 0
    fi

    # Attempt the wire. Trust the remote's __APPENDED__ marker — some shells
    # bubble benign ssh stderr warnings up as non-zero exit, but the append
    # itself succeeded. We check stdout for the marker, not the exit code.
    # `|| true` prevents set -e from aborting when ssh itself fails (exit 255
    # on unreachable host); we want to reach the failure-marker branch below.
    # Pipe message via stdin so apostrophes (or any shell metachar) in the
    # payload cannot break the single-quoted remote echo.
    local out err
    err=$(mktemp -t airc-send-err.XXXXXX)
    out=$(printf '%s\n' "$full_msg" | relay_ssh "$host_target" "cat >> $rhome/messages.jsonl && echo __APPENDED__" 2>"$err" || true)
    if ! echo "$out" | grep -q '^__APPENDED__$'; then
      # Wire failed. Queue the payload for automatic retry by flush_pending_loop
      # in the monitor, then annotate the local log with a [QUEUED] marker so
      # `airc logs` makes the state obvious. Don't die() — queued is a form of
      # success. The user's shell scripts can still check pending.jsonl if
      # they need to block on delivery.
      # Distinguish auth failures (user must re-pair — retrying won't help)
      # from network failures (queue + retry makes sense). Prior behavior
      # silently queued both the same way, hiding auth errors behind a
      # misleading "Host unreachable" message. This bit the cross-mesh
      # coordination: fresh-install joiner's SSH key wasn't in host's
      # authorized_keys, cmd_send queued + returned 0, the joiner thought
      # their send succeeded when the host never saw anything.
      local stderr_raw; stderr_raw=$(cat "$err" 2>/dev/null)
      local stderr; stderr=$(printf '%s' "$stderr_raw" | tr '\n' ' ' | sed 's/"/\\"/g' | cut -c1-300)
      rm -f "$err"

      local is_auth_fail=0
      if echo "$stderr_raw" | grep -qiE 'permission denied|publickey|host key verification|authentication fail|identification has changed|no supported authentication'; then
        is_auth_fail=1
      fi

      if [ "$is_auth_fail" = "1" ]; then
        local fail_marker; fail_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[AUTH FAILED to %s — repair required, NOT queued] %s"}' \
          "$(timestamp)" "$active_channel" "$peer_name" "${stderr:-no stderr}")
        echo "$fail_marker" >> "$MESSAGES"
        echo "  SSH auth to host FAILED. Message NOT queued — every retry would fail identically." >&2
        echo "  SSH stderr: ${stderr}" >&2
        echo "  Fix: airc teardown --flush && airc connect <invite-string>" >&2
        die "Authentication failure — re-pair required"
      fi

      # Network-class wire failure: legitimately transient, queue for retry.
      echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
      local queue_marker; queue_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[QUEUED to %s — network error, will retry] %s"}' \
        "$(timestamp)" "$active_channel" "$peer_name" "${stderr:-no stderr}")
      echo "$queue_marker" >> "$MESSAGES"
      echo "  Network error reaching host — message queued for retry. Monitor will flush when host returns." >&2
      # Surface the actual stderr so the user understands WHY — the old
      # generic "host unreachable" was hiding real errors.
      echo "  SSH stderr: ${stderr:-<none>}" >&2
    else
      rm -f "$err"
    fi
  else
    # Host path: append to OUR messages.jsonl. Joiners' SSH tails will
    # pick it up and route to their monitors. BUT — if our monitor isn't
    # actually running, no joiner is connected (the SSH tail rides on the
    # monitor process tree), and this append goes to a log nobody reads.
    # The send returns 0 and the user thinks it succeeded.
    #
    # That's exactly how Joel hit "I see no communication going on" on
    # 2026-04-26: shell auto-cd'd into a different scope mid-session, that
    # scope's monitor was dead, every `airc msg` returned 0 with zero
    # delivery, and the peer in the actual room waited forever for a
    # reply that never landed.
    #
    # Detect: pidfile exists AND every PID in it is alive. Anything else
    # = monitor dead = broadcasting into a void. Die loudly so the user
    # immediately knows their cwd / scope / monitor state is wrong.
    local _pidfile="$AIRC_WRITE_DIR/airc.pid"
    local _monitor_alive=0
    if [ -f "$_pidfile" ]; then
      local _pids; _pids=$(cat "$_pidfile" 2>/dev/null)
      if [ -n "$_pids" ]; then
        local _all_alive=1 _p
        for _p in $_pids; do
          kill -0 "$_p" 2>/dev/null || { _all_alive=0; break; }
        done
        [ "$_all_alive" = "1" ] && _monitor_alive=1
      fi
    fi
    if [ "$_monitor_alive" = "0" ]; then
      # --internal callers (informational broadcasts: [rename], etc.):
      # append to the local log silently and return 0. The monitor-down
      # die is appropriate UX for explicit `airc send` — it surfaces
      # "you're broadcasting to nobody" loudly so the user doesn't wait
      # for a reply that can't arrive. For [rename] the broadcast is
      # informational; receivers heal via monitor_formatter's host-
      # fallback on next traffic, so noisily failing the rename in any
      # scope whose monitor isn't running today (a perfectly normal
      # multi-scope state) would give the rename feature a worse UX
      # than no-propagation had.
      if [ "$internal" = "1" ]; then
        echo "$full_msg" >> "$MESSAGES"
        date +%s > "$AIRC_WRITE_DIR/last_sent" 2>/dev/null
        rm -f "$AIRC_WRITE_DIR/reminded" 2>/dev/null
        return 0
      fi
      echo "  Send NOT delivered — this scope's monitor isn't running." >&2
      echo "    scope:    $AIRC_WRITE_DIR" >&2
      echo "    identity: $my_name (host)" >&2
      if [ -f "$_pidfile" ]; then
        echo "    pidfile:  $_pidfile (stale — process not alive)" >&2
      else
        echo "    pidfile:  absent (monitor never started in this scope)" >&2
      fi
      echo "  Joiners ride on the monitor's SSH tail; with the monitor down, your message reaches no one." >&2
      echo "  Fix: run 'airc connect' to start (or resume) this scope's monitor, then retry." >&2
      echo "       OR cd into the scope you actually meant to send from." >&2
      die "monitor down — refusing to silently broadcast into a void"
    fi
    echo "$full_msg" >> "$MESSAGES"
  fi

  # Reset reminder — you sent something, clock restarts
  date +%s > "$AIRC_WRITE_DIR/last_sent" 2>/dev/null
  rm -f "$AIRC_WRITE_DIR/reminded" 2>/dev/null

  # Surface a one-line confirmation. QA pass 2026-04-28: silent success
  # was indistinguishable from "command did nothing" — users hit
  # `airc msg ...`, see no output, and have no idea whether the message
  # is in flight, queued, or eaten. --internal callers (cmd_rename's
  # propagation, etc.) stay silent on purpose; the user-invoked surface
  # gets the confirmation.
  if [ "$internal" != "1" ]; then
    if [ "$peer_name" = "all" ]; then
      echo "  → #${active_channel} (broadcast)"
    else
      echo "  → @${peer_name} on #${active_channel}"
    fi
  fi
}

# Ping a peer to verify their monitor is alive AND processing traffic.
#
# Sends [PING:<uuid>] to the peer via cmd_send, then tails the local
# messages.jsonl for a [PONG:<uuid>] response from that peer with a
# timeout. Three outcomes the caller can distinguish:
#
#   - PONG arrives within timeout → peer's monitor is alive + running
#     a compatible airc version (one with the auto-pong handler in
#     monitor_formatter).
#   - Timeout, but [PING:<uuid>] IS visible in local log → the ping
#     landed on the wire (SSH append succeeded) but no response. Either
#     (a) peer's monitor is dead, or (b) peer is running an older airc
#     without the auto-pong handler, or (c) peer is a non-airc agent
#     (e.g., Codex) that reads the log but doesn't respond.
#   - Timeout, [PING:<uuid>] NOT visible → the send itself failed or
#     queued (see cmd_send's wire-failure branch). Wire is broken.
#
# Design: ping is a regular signed message with a prefix marker. Clients
# that don't implement auto-pong see it as "a message starting with
# [PING:]" — harmless, logs it, life continues. Forward-compatible +
# gracefully-degrading across airc versions AND across agent types.
#
# Usage:
#   airc ping @peer           # default 10s timeout
#   airc ping @peer 30        # 30s timeout
cmd_ping() {
  local first="${1:-}"
  case "$first" in
    -h|--help)
      echo "Usage:"
      echo "  airc ping @peer                liveness probe (default 10s timeout)"
      echo "  airc ping @peer <timeout>      override timeout (positive integer seconds)"
      return 0 ;;
  esac
  [ -z "$first" ] && die "Usage: airc ping @peer [timeout_secs]"
  case "$first" in
    @*) ;;
    *) die "Usage: airc ping @peer — ping requires an @peer target (broadcast ping not supported)" ;;
  esac
  local peer_name="${first#@}"
  local timeout="${2:-10}"
  # Basic sanity: timeout must be a positive integer. Guards against
  # typos that would make the wait-loop spin forever or exit early.
  case "$timeout" in
    ''|*[!0-9]*) die "timeout must be a positive integer (got '$timeout')" ;;
  esac
  ensure_init

  # uuid from python for format consistency with the regex in monitor_formatter.
  local ping_id
  ping_id=$("$AIRC_PYTHON" -c "import uuid; print(uuid.uuid4())")

  local start_time
  start_time=$(date +%s)

  # Use cmd_send so the ping rides the same signed-message path as
  # normal traffic — guaranteed shape parity with what the receiver's
  # monitor_formatter reads.
  cmd_send "@$peer_name" "[PING:$ping_id]" >/dev/null || die "ping send failed — check SSH/auth state (airc status)"

  echo "ping sent to $peer_name (id=$ping_id) — waiting up to ${timeout}s for pong..."

  # Poll local messages.jsonl for the matching pong. We check the FULL
  # log since the ping was written (cmd_send mirrors locally first).
  # 0.5s poll is responsive without spinning.
  while true; do
    local now elapsed
    now=$(date +%s)
    elapsed=$((now - start_time))
    if grep -q "\[PONG:$ping_id\]" "$MESSAGES" 2>/dev/null; then
      echo "PONG received from $peer_name after ${elapsed}s — monitor alive + auto-responder working."
      return 0
    fi
    if [ "$elapsed" -ge "$timeout" ]; then
      echo "TIMEOUT after ${timeout}s — no pong from $peer_name."
      # Secondary diagnosis: did the ping land on the wire at all?
      if grep -q "\[PING:$ping_id\]" "$MESSAGES" 2>/dev/null; then
        echo "  Ping IS visible in local log (cmd_send mirrored it). That proves our outbound works."
        echo "  No pong likely means: (a) peer's monitor is dead, (b) peer runs older airc without auto-pong, or (c) peer is a non-airc agent."
      else
        echo "  Ping is NOT in local log — cmd_send's mirror may have failed. Check: airc status, airc logs."
      fi
      return 1
    fi
    sleep 0.5
  done
}
