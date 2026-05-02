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
  # _explicit_channel: user used --room/--channel to target a specific
  # channel. When set, we MUST find a gist mapping for that channel —
  # silently falling back to the scope's primary room_gist_id has the
  # phantom-room failure mode (banner says "→ #qa-experiment", message
  # actually goes to #general's gist with channel field "qa-experiment",
  # no new gist ever created). User reported 2026-04-29: "I sent to
  # #qa-experiment, #qa-sub-room-test, #x — all 'succeeded' with banner,
  # NONE created a gist."
  local _explicit_channel=0
  # --internal: best-effort send for internal informational broadcasts
  # ([rename], etc.) where the monitor-down guard is the wrong UX. Append
  # to the local log + return 0 even when the monitor isn't running.
  # Receivers heal via monitor_formatter's host-fallback / next-traffic
  # passes, so missing one event in a quiet scope isn't a correctness
  # issue. Exposed as a flag (not an env var) so call sites are
  # grep-able and the pattern matches the rest of the airc CLI surface.
  local internal=0
  # --plaintext: skip envelope-layer encryption even when recipient
  # x25519 pubkey is on file. For control traffic ([PING:uuid] /
  # [PONG:uuid]) where the body is a public uuid with zero secret
  # content. Pre-fix: pings encrypted asymmetrically (sender had
  # recipient's pubkey; recipient lacked sender's pubkey) → recipient
  # silently dropped with "missing pubkey/privkey for decrypt" → no
  # auto-pong → cmd_ping timed out. Plaintext sidesteps the asymmetry
  # entirely for these short-lived control messages.
  local plaintext=0
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
        _explicit_channel=1
        shift 2 ;;
      --channel|-c)
        channel_override="${2:-}"
        [ -z "$channel_override" ] && die "Usage: airc send --channel <name> <message>"
        _explicit_channel=1
        shift 2 ;;
      --internal)
        internal=1
        shift ;;
      --plaintext|-plaintext)
        plaintext=1
        shift ;;
      --)
        # POSIX flag-terminator: anything after this is positional even
        # if it looks like a flag. Lets users send literal --strings in
        # message bodies: `airc msg -- --foo bar`.
        shift
        while [ $# -gt 0 ]; do positional+=("$1"); shift; done
        ;;
      --*|-*)
        # Unknown flag — pre-fix this fell into the *) catch-all and got
        # silently absorbed into the message body, so 'airc msg
        # --invalidflag value body' returned exit 0 broadcast (ideem-
        # local-4bef caught 2026-04-29). Loud failure now; users with a
        # literal --string in the message body can use the -- terminator.
        die "Unknown flag: $1 (use -- to terminate flags if this is part of the message body)" ;;
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
        # Reject empty `@` (continuum-b741 + ideem-local-4bef caught
        # 2026-04-29: `airc msg @ body` silently broadcast). Reject
        # double-@ `@@peer` while we're here (also caught: accepted as
        # DM to literal '@peer'). Reject numeric-only DMs as per the
        # peer-name charset rule (`@12345` is a likely typo, not a
        # real peer).
        case "$_p" in
          "")           die "Empty @ recipient (use 'airc msg <message>' for broadcast, or 'airc msg @<peer> ...' for DM)" ;;
          @*)           die "Double @ recipient '$1' — peer names are bare, not @-prefixed" ;;
          *[!a-z0-9,-]*) die "Invalid peer name in '$1' — must match [a-z0-9-]+ (comma-separated for multi-DM)" ;;
        esac
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
    # #141: if the parsed @-target contains whitespace, it's not a peer
    # name (peer names are [a-z0-9-]); user clearly wanted an in-channel
    # @-mention via something like `airc msg "@bob looks good"`. Treat
    # as broadcast and put the @-prefix back in the message body.
    case "$_peer_csv" in
      *[[:space:]]*)
        msg="@${_peer_csv}"
        if [ -n "$*" ]; then
          msg="${msg} $*"
        fi
        peer_name="all"
        ;;
      *)
        peer_name="$_peer_csv"
        msg="$*"
        if [ -z "$msg" ]; then
          # #147: empty body after leading @-tokens.
          echo "Usage:" >&2
          echo "  airc msg @peer <message>           DM that peer" >&2
          echo "  airc msg @peer1 @peer2 <message>   DM multiple peers" >&2
          echo "  airc msg \"@peer looks good\"        in-channel @-mention" >&2
          echo "                                      (whitespace in @-token = broadcast intent)" >&2
          die "got @-targets but no message body. To @-mention in the channel, include text after the @name in the same quoted string."
        fi
        ;;
    esac
  else
    peer_name="all"
    msg="$*"
  fi
  # Reject empty broadcast bodies — pre-fix `airc msg ""` printed the
  # usage line but exited 0 (continuum-b741 caught 2026-04-29). The
  # other "no message" path already dies above; this one is the
  # explicit-empty-string case that fell through.
  [ "$peer_name" = "all" ] && [ -z "$msg" ] \
    && die "empty message body (use 'airc msg <text>' or omit the empty quotes)"
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
    # what the wire does. Local mirror stays PLAINTEXT (the user's own
    # audit log of what they sent — the alternative would force `airc
    # logs` to decrypt own outbound, which is silly). Wire form may be
    # encrypted below if the recipient has a stored x25519_pub.
    echo "$full_msg" >> "$MESSAGES"

    # Phase E.3: wrap the wire envelope with envelope-layer encryption
    # if we have the recipient's X25519 pubkey on file. Empty pubkey =
    # peer is on pre-Phase-E airc (or never paired with us under E),
    # in which case the wrap CLI passes through unchanged. Failures
    # also pass through (loud stderr, no silent encryption skip).
    # Phase E.3: look up recipient's X25519 pubkey for envelope encryption.
    # Empty result = peer hasn't paired under E2E (or our cryptography
    # package isn't installed). The wrap CLI passes through plaintext in
    # that case, transparently.
    local recipient_pub=""
    if [ "$peer_name" != "all" ] && [ "$plaintext" != "1" ]; then
      recipient_pub=$("$AIRC_PYTHON" -m airc_core.identity peer_pub \
        --peers-dir "$PEERS_DIR" --peer-name "$peer_name" 2>/dev/null || true)
    fi
    local wire_msg="$full_msg"
    if [ -n "$recipient_pub" ]; then
      # Stderr unredirected — wrap failures surface loud (CLAUDE.md "never swallow").
      wire_msg=$(printf '%s' "$full_msg" | "$AIRC_PYTHON" -m airc_core.envelope wrap \
        --recipient-pub "$recipient_pub" \
        --identity-dir "$IDENTITY_DIR" || printf '%s' "$full_msg")
      # Diagnostic: emit the wire shape to stderr so test logs surface
      # whether encryption actually engaged. Phase E debug aid; remove
      # once production has verified the path. (Doubles as a sanity
      # check for `airc doctor`.)
      if [ -n "${AIRC_E2E_DEBUG:-}" ]; then
        echo "[airc:e2e] wire_msg: $wire_msg" >&2
      fi
    fi

    # Hand the wire to the bearer abstraction. ALL transport-specific
    # knowledge lives in lib/airc_core/bearer_*.py. cmd_send only:
    #   1. Builds the signed envelope (above)
    #   2. Wraps with envelope-layer crypto if recipient supports it
    #   3. Hands payload + peer_meta to bearer_cli
    #   4. Branches on the structured SendOutcome.kind
    # Adding a new transport doesn't touch this file — only the
    # resolver registers it. (Phases 1-3 of bearer rewrite; #269 #270.)
    # Phase 3c+ (#283): route by channel. The active_channel's gist
    # comes from channel_gists in config (populated at subscribe time).
    # If the channel has no mapping, fall back to the scope's primary
    # room_gist_id so single-room scopes keep working unchanged.
    local room_gist_id=""
    room_gist_id=$("$AIRC_PYTHON" -m airc_core.config get_channel_gist \
      --config "$CONFIG" --channel "$active_channel" 2>/dev/null || true)
    # Phantom-room guard: when --room/--channel was used explicitly,
    # NEVER fall back to the scope's primary room_gist_id — that publishes
    # to the wrong gist with the right channel-tag, creating an invisible
    # mis-route ("phantom success"). Only the no-override path falls back
    # for back-compat with single-channel scopes.
    if [ -z "$room_gist_id" ]; then
      if [ "$_explicit_channel" = "1" ]; then
        die "no gist mapping for channel '$active_channel' — run 'airc join --room $active_channel' first to create the room"
      fi
      if [ -f "$AIRC_WRITE_DIR/room_gist_id" ]; then
        room_gist_id=$(cat "$AIRC_WRITE_DIR/room_gist_id" 2>/dev/null || true)
      fi
    fi

    local outcome
    outcome=$(printf '%s' "$wire_msg" | "$AIRC_PYTHON" -m airc_core.bearer_cli send \
      "$peer_name" "$active_channel" \
      --host-target "$host_target" \
      --remote-home "$rhome" \
      --room-gist-id "$room_gist_id")
    local kind detail
    kind=$(printf '%s' "$outcome" | "$AIRC_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("kind",""))' 2>/dev/null)
    detail=$(printf '%s' "$outcome" | "$AIRC_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("detail",""))' 2>/dev/null)

    case "$kind" in
      delivered)
        # Wire success. Local mirror already happened above; nothing else to do.
        :
        ;;
      queued_unreachable)
        # Bearer chose to short-circuit a predictable miss (e.g. tailnet-
        # offline peer). Queue + marker; monitor's flush_pending_loop drains
        # when the peer wakes.
        echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
        local queue_marker; queue_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[QUEUED to %s — %s]"}' \
          "$(timestamp)" "$active_channel" "$peer_name" "${detail:-peer offline}")
        echo "$queue_marker" >> "$MESSAGES"
        date +%s > "$AIRC_WRITE_DIR/last_sent" 2>/dev/null
        rm -f "$AIRC_WRITE_DIR/reminded" 2>/dev/null
        return 0
        ;;
      auth_failure)
        # Don't queue — every retry will fail identically until the
        # underlying auth is fixed. airc IS the instigator: trigger
        # browser self-heal flow via the centralized state machine,
        # then retry the send ONCE if heal succeeded.
        # Joel: "gh logouts are FREQUENT" / "script needs to self-heal".
        if airc_ensure_gh_auth_or_heal "airc send → $peer_name (#$active_channel)"; then
          # Auth restored. Retry once. We don't loop — if the retry
          # fails too, something else is going on (scope missing? gist
          # deleted?) and the user needs to see the original error.
          echo "  ↻ Retrying send post-heal..." >&2
          local retry_outcome retry_kind retry_detail
          # Pre-fix called undefined `send_via_bearer` (Copilot caught
          # this on PR #422 review — would crash at runtime under
          # set -euo pipefail). Re-invoke the same bearer_cli pipeline
          # the initial send used (lines ~316-320) so the retry path is
          # exactly the original send path replayed against fresh auth.
          retry_outcome=$(printf '%s' "$wire_msg" | "$AIRC_PYTHON" -m airc_core.bearer_cli send \
            "$peer_name" "$active_channel" \
            --host-target "$host_target" \
            --remote-home "$rhome" \
            --room-gist-id "$room_gist_id" 2>&1) || true
          retry_kind=$(printf '%s' "$retry_outcome" | "$AIRC_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("kind",""))' 2>/dev/null)
          retry_detail=$(printf '%s' "$retry_outcome" | "$AIRC_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("detail",""))' 2>/dev/null)
          if [ "$retry_kind" = "delivered" ]; then
            echo "  ✓ Sent post-heal." >&2
            return 0
          fi
          echo "  ✗ Retry post-heal also failed (kind=$retry_kind detail=$retry_detail)" >&2
          # Fall through to the failure-marker block so the message is
          # recorded as failed in the local log + user sees the error.
        fi
        local fail_marker; fail_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[GH AUTH FAILED to %s — re-auth required, NOT queued] %s"}' \
          "$(timestamp)" "$active_channel" "$peer_name" "${detail:-no detail}")
        echo "$fail_marker" >> "$MESSAGES"
        echo "" >&2
        echo "  ✗ gh auth check failed (post-heal-attempt) — bearer detail: ${detail}" >&2
        echo "    Re-run 'airc send' to retry the self-heal flow," >&2
        echo "    or fix manually: gh auth login -h github.com -s gist" >&2
        echo "    No state lost; no re-pair needed — gh's keyring expired." >&2
        die "gh auth failure — re-run 'airc send' or 'gh auth login -h github.com -s gist'"
        ;;
      gone)
        # Permanent destination loss (#381 layer A — HTTP 404 on the
        # gist). Joiner-side mirror of the host-branch handler below:
        # clear stale mapping, surface [GONE] marker, do NOT queue
        # (retries would all 404 too). Recovery: airc join --room
        # rediscovers / re-hosts.
        "$AIRC_PYTHON" -m airc_core.config set_channel_gist \
          --config "$CONFIG" --channel "$active_channel" --gist-id "" \
          >/dev/null 2>&1 || true
        local gone_marker; gone_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[GONE: room gist 404 — channel #%s dissolved, message NOT delivered. Re-host with: airc join --room %s] %s"}' \
          "$(timestamp)" "$active_channel" "$active_channel" "$active_channel" "${detail:-no detail}")
        echo "$gone_marker" >> "$MESSAGES"
        echo "" >&2
        echo "  ✗ Gist for #${active_channel} returned 404 — channel dissolved." >&2
        echo "    Stale channel_gists[${active_channel}] cleared." >&2
        echo "    Re-host:  airc join --room ${active_channel}" >&2
        ;;
      secondary_rate_limit)
        # gh secondary throttle (#381 layer A). Queue + distinct marker.
        echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
        local rate_marker; rate_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[RATE-LIMITED on gh secondary throttle — queued, will retry once gh window clears (60-180s)] %s"}' \
          "$(timestamp)" "$active_channel" "${detail:-no detail}")
        echo "$rate_marker" >> "$MESSAGES"
        echo "  ⏳ gh secondary rate limit — message queued. Drain loop retries when window clears (~60-180s)." >&2
        echo "  Avoid sending more for ~2 minutes." >&2
        echo "  Bearer: ${detail:-<none>}" >&2
        ;;
      transient_failure|"")
        # Network-class failure or empty/malformed outcome → treat as
        # transient + queue. Empty kind defends against bearer_cli
        # crashing or outputting nothing — the message goes to pending
        # rather than disappearing silently.
        echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
        local queue_marker; queue_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[QUEUED to %s — network error, will retry] %s"}' \
          "$(timestamp)" "$active_channel" "$peer_name" "${detail:-no detail}")
        echo "$queue_marker" >> "$MESSAGES"
        echo "  Network error reaching host — message queued for retry. Monitor will flush when host returns." >&2
        echo "  Bearer: ${detail:-<none>}" >&2
        ;;
      *)
        # Unknown kind. The bearer.py SendOutcome contract enumerates the
        # valid kinds — if we hit this, the bearer added a kind without
        # updating callers. Queue defensively + log loudly so it's caught.
        echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
        echo "  Unknown bearer outcome kind '${kind}' (detail: ${detail}). Queued defensively. Update cmd_send.sh." >&2
        ;;
    esac
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
    # Detect monitor liveness via the shared sandbox-robust helper
    # (_monitor_alive_with_bearer_fallback in airc top-level). Same
    # contract as cmd_status post-#371. Phase 1 = kill -0 (canonical);
    # phase 2 = bearer-state freshness (covers Codex's sandbox where
    # kill -0 is process-tree-blind even when bearer-recv is provably
    # writing to bearer_state.<channel>.json). Pre-fix this used the
    # naked prune_pidfile_and_count helper which would ALSO actively
    # delete the pidfile when phase 1 was wrong about death — silently
    # corrupting state inside Codex's sandbox. The new helper is read-
    # only + sandbox-aware. #370/#371/#372 root cause cluster.
    local _pidfile="$AIRC_WRITE_DIR/airc.pid"
    local _monitor_alive=0
    if [ "$(_monitor_alive_with_bearer_fallback "$_pidfile")" = "yes" ]; then
      _monitor_alive=1
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
      echo "  Fix: run 'airc connect' to start (or resume) this scope's monitor, then retry." >&2
      echo "       OR cd into the scope you actually meant to send from." >&2
      die "monitor down — refusing to silently broadcast into a void"
    fi

    # Phase 3c critical fix (#285): host-side cmd_send must ALSO publish
    # to the room gist so joiners (who poll the gist via GhBearer) see
    # broadcasts and DMs from the host. Pre-3c, joiners tailed the host's
    # local messages.jsonl over SSH; with SSH gone, joiners poll the gist
    # — local-only append disappears into a void. Worst silent-loss
    # class until this fix landed.
    #
    # Same wrap-if-recipient-known logic as the joiner branch above:
    # encrypt msg field with recipient's X25519 pub when it's a DM and
    # we have their pubkey on file; broadcasts go plaintext (group
    # encryption is a future Phase E.4).
    local _host_recipient_pub=""
    if [ "$peer_name" != "all" ] && [ "$plaintext" != "1" ]; then
      _host_recipient_pub=$("$AIRC_PYTHON" -m airc_core.identity peer_pub \
        --peers-dir "$PEERS_DIR" --peer-name "$peer_name" 2>/dev/null || true)
    fi
    local _host_wire_msg="$full_msg"
    if [ -n "$_host_recipient_pub" ]; then
      _host_wire_msg=$(printf '%s' "$full_msg" | "$AIRC_PYTHON" -m airc_core.envelope wrap \
        --recipient-pub "$_host_recipient_pub" \
        --identity-dir "$IDENTITY_DIR" || printf '%s' "$full_msg")
    fi

    # Route by channel via channel_gists (post-#283); fall back to the
    # scope's primary room_gist_id so single-room hosts keep working —
    # but only when the user did NOT explicitly target a different room
    # (otherwise the fallback turns --room into a phantom success that
    # silently mis-routes to the primary gist; see _explicit_channel).
    local _host_room_gist_id=""
    _host_room_gist_id=$("$AIRC_PYTHON" -m airc_core.config get_channel_gist \
      --config "$CONFIG" --channel "$active_channel" 2>/dev/null || true)
    if [ -z "$_host_room_gist_id" ]; then
      if [ "$_explicit_channel" = "1" ]; then
        die "no gist mapping for channel '$active_channel' — run 'airc join --room $active_channel' first to create the room"
      fi
      if [ -f "$AIRC_WRITE_DIR/room_gist_id" ]; then
        _host_room_gist_id=$(cat "$AIRC_WRITE_DIR/room_gist_id" 2>/dev/null || true)
      fi
    fi

    if [ -n "$_host_room_gist_id" ]; then
      local _host_outcome
      _host_outcome=$(printf '%s' "$_host_wire_msg" | "$AIRC_PYTHON" -m airc_core.bearer_cli send \
        "$peer_name" "$active_channel" \
        --room-gist-id "$_host_room_gist_id")
      local _host_kind _host_detail
      _host_kind=$(printf '%s' "$_host_outcome" | "$AIRC_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("kind",""))' 2>/dev/null)
      _host_detail=$(printf '%s' "$_host_outcome" | "$AIRC_PYTHON" -c 'import json,sys; print(json.load(sys.stdin).get("detail",""))' 2>/dev/null)
      case "$_host_kind" in
        delivered)
          # Append to local audit log only on confirmed delivery. Pre-fix
          # this happened unconditionally before the publish attempt, so
          # the user's own monitor would echo their message back even when
          # joiners never saw it — false success surface (#381 RCA).
          echo "$full_msg" >> "$MESSAGES"
          ;;
        auth_failure)
          # Hard failure mirror of joiner branch above. Don't queue —
          # every retry hits the same dead token. Surface re-auth path
          # loudly + die so the caller knows to fix gh before retrying.
          local _host_fail_marker; _host_fail_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[GH AUTH FAILED on host gist publish — re-auth required, NOT queued] %s"}' \
            "$(timestamp)" "$active_channel" "${_host_detail:-no detail}")
          echo "$_host_fail_marker" >> "$MESSAGES"
          echo "" >&2
          echo "  ✗ gh auth check failed during host gist publish — your GitHub token is dead." >&2
          echo "    Bearer detail: ${_host_detail}" >&2
          echo "" >&2
          echo "    Fix:  gh auth login -h github.com -s gist" >&2
          echo "" >&2
          echo "    The -s gist scope is required — token without it can authenticate but can't publish to gists." >&2
          echo "    After re-authenticating, retry. No state lost — it's just gh's keyring that expired." >&2
          die "gh auth failure on host gist publish — run 'gh auth login -h github.com -s gist' and retry"
          ;;
        gone)
          # Permanent destination loss (#381 layer A — HTTP 404 on the
          # gist). The room dissolved: peer ran `airc part`, gh deleted
          # the gist, or the gist was wiped. Retrying is futile — the
          # next send + every send after will keep returning 404.
          #
          # Recovery: drop the stale channel_gists entry so the next
          # `airc join --room <name>` (by us or another peer) creates a
          # fresh canonical gist instead of polling the dead one. Also
          # surface a [GONE] marker so the user knows their message was
          # NOT delivered AND why — pre-fix the substrate had no signal
          # for "this channel is gone forever" so users sent into a void
          # for hours not knowing.
          #
          # Do NOT queue: pending.jsonl drains via the SAME publish path
          # which would also return 404 every retry, just consuming gh
          # API budget for nothing. Drop the message on the floor with
          # loud surface (the [GONE] marker) — same UX as auth_failure
          # but a different remedy.
          "$AIRC_PYTHON" -m airc_core.config set_channel_gist \
            --config "$CONFIG" --channel "$active_channel" --gist-id "" \
            >/dev/null 2>&1 || true
          local _host_gone_marker; _host_gone_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[GONE: room gist 404 — channel #%s dissolved, message NOT delivered. Re-host with: airc join --room %s] %s"}' \
            "$(timestamp)" "$active_channel" "$active_channel" "$active_channel" "${_host_detail:-no detail}")
          echo "$_host_gone_marker" >> "$MESSAGES"
          echo "" >&2
          echo "  ✗ Gist for #${active_channel} returned 404 — channel dissolved (peer ran 'airc part', gh deleted, or wiped)." >&2
          echo "    Bearer detail: ${_host_detail}" >&2
          echo "    Stale channel_gists[${active_channel}] cleared from config." >&2
          echo "" >&2
          echo "    To re-host this channel: airc join --room ${active_channel}" >&2
          echo "    To wait for a peer to take over: leave it; next 'airc join' that resolves the mesh-singleton will publish a fresh gist." >&2
          ;;
        secondary_rate_limit)
          # gh per-burst write throttle (#381 layer A — HTTP 403 with
          # "rate limit exceeded" / "secondary rate limit" body). Every
          # peer on this gh account writing concurrently can trip this;
          # primary rate stays nominal but the secondary clamps for
          # 60-180s. Re-auth does not help; only waiting does.
          #
          # Queue + emit a DISTINCT marker so users see this is the
          # rate-limit case (not generic transient/network) and know to
          # back off their own sends until it clears. flush_pending_host_loop
          # will drain on the next cycle when bearer_gh.send returns
          # delivered again.
          echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
          local _host_rate_marker; _host_rate_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[RATE-LIMITED on gh secondary throttle — %d msg(s) queued, will retry once gh window clears (60-180s)] %s"}' \
            "$(timestamp)" "$active_channel" "$(wc -l < "$AIRC_WRITE_DIR/pending.jsonl" | tr -d " ")" "${_host_detail:-no detail}")
          echo "$_host_rate_marker" >> "$MESSAGES"
          echo "  ⏳ gh secondary rate limit hit — broadcast queued. Drain loop will retry once the burst window clears (60-180s typical)." >&2
          echo "  Avoid sending more for ~2 minutes to let the throttle clear." >&2
          echo "  Bearer: ${_host_detail:-<none>}" >&2
          ;;
        transient_failure|"")
          # Mirror joiner-side queue/drain symmetry (#381 layer B). Pre-fix
          # the host branch printed a warning + dropped the message on
          # the floor: exit 0 with the broadcast never reaching peers.
          # Now the message goes to pending.jsonl + a [QUEUED] marker
          # surfaces in the user's own log so the chat widget reads
          # 'queued for retry' instead of 'sent successfully'.
          # flush_pending_host_loop (in airc top-level) drains this on
          # the same 5s tick joiners use.
          echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
          local _host_queue_marker; _host_queue_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[QUEUED to gist %s — network error, will retry] %s"}' \
            "$(timestamp)" "$active_channel" "$_host_room_gist_id" "${_host_detail:-no detail}")
          echo "$_host_queue_marker" >> "$MESSAGES"
          echo "  Network error reaching gist — broadcast queued for retry. Monitor will flush when gist returns." >&2
          echo "  Bearer: ${_host_detail:-<none>}" >&2
          ;;
        *)
          # Unknown kind — bearer_cli added an outcome without updating
          # callers. Queue defensively + log loudly so it gets caught.
          echo "$full_msg" >> "$AIRC_WRITE_DIR/pending.jsonl"
          echo "  Unknown bearer outcome kind '${_host_kind}' on host send (detail: ${_host_detail}). Queued defensively. Update cmd_send.sh." >&2
          ;;
      esac
    else
      echo "  ⚠ No room_gist_id set ($AIRC_WRITE_DIR/room_gist_id missing) — host send is local-only." >&2
      echo "$full_msg" >> "$MESSAGES"
    fi
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

  # Route via #general (universal lobby) and force PLAINTEXT. Encryption
  # for [PING:uuid] adds zero security value (the body is a public uuid)
  # and was the actual cause of #308: pair handshake was asymmetric, so
  # one side dropped encrypted control traffic with "missing pubkey/
  # privkey for decrypt" and the auto-pong silently never propagated.
  # Plaintext sidesteps the asymmetry; the bigger pubkey-symmetry fix
  # is its own follow-up.
  cmd_send --channel general --plaintext "@$peer_name" "[PING:$ping_id]" >/dev/null \
    || die "ping send failed (airc status)"

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
