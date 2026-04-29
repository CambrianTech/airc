# Sourced by airc. cmd_connect — the join/pair/host orchestrator.
#
# Single huge command function (1355 lines) covering all of:
#   * argv flag parsing (~60 flags)
#   * `airc join <gist-id|mnemonic>` joiner path
#   * `airc join` host bootstrap (gh gist publish, ssh keygen, sshd start)
#   * connect-time doctor preflight + Tailscale start
#   * heartbeat thread (15s gist update)
#   * #general sidecar spawn + room gating
#   * monitor loop entry
#
# Self-contained — calls airc top-level helpers (die, ensure_init,
# get_config_val, set_config_val, relay_ssh, _reexec_into,
# _self_heal_stale_host, spawn_general_sidecar_if_wanted, monitor,
# detect_platform, port_listeners, …) but defines no functions
# referenced from outside the connect surface.
#
# Extracted from airc as part of #152 Phase 3 file split, after Joel
# 2026-04-27 push: shell scripts are like classes; the 5200-line bash
# monolith was wrong. cmd_connect was the single largest block.
# Future passes will further decompose this file (host vs joiner vs
# heartbeat are clearly separable), but step 1 is splitting it out of
# the top-level monolith without changing behavior.

# ensure_channel_subscribed_with_gist <channel> [--first]
#
# Single-concern helper: make this scope a fully-functional subscriber
# of <channel>. Three steps that MUST happen together — pre-2026-04-29
# they were inlined at 4+ call sites, the divergent-room path silently
# omitted step 2, and custom rooms became uncreatable. Centralized so
# every call site does the right thing; future channel-add paths just
# call this.
#
#   1. Subscribe in config (subscribed_channels[]).
#      --first: prepend (sets the scope's default channel).
#      default: append.
#   2. Resolve-or-create the canonical gist for the channel on the
#      user's gh account (airc_core.channel_gist resolve
#      --create-if-missing). Idempotent across runs.
#   3. Persist the channel→gist mapping in channel_gists{} so cmd_send's
#      route-by-channel and the multi-channel monitor's per-channel
#      bearer_cli recv both have a destination.
#
# Echoes the gist id on success. Empty (and non-zero exit) on failure;
# caller decides whether that's fatal — the #general sidecar path
# treats it as a warning, the primary-room path treats it as fatal.
#
# Per CLAUDE.md "never swallow errors": stderr from the python
# subprocesses is redirected to a status file, then echoed if non-empty
# on failure. Routine 2>/dev/null suppression would have hidden the
# heartbeat-multifile bug for another sprint.
ensure_channel_subscribed_with_gist() {
  local channel="${1:-}" mode="${2:-append}"
  if [ -z "$channel" ]; then
    echo "ensure_channel_subscribed_with_gist: missing channel arg" >&2
    return 2
  fi

  local _err; _err=$(mktemp -t airc-ensure-ch.XXXXXX)
  trap '[ -n "${_err:-}" ] && rm -f "$_err"' RETURN

  # 1. Subscribe in config.
  local _first_flag=""
  [ "$mode" = "--first" ] && _first_flag="--first"
  if ! "$AIRC_PYTHON" -m airc_core.config subscribe \
       --config "$CONFIG" --channel "$channel" $_first_flag 2>"$_err"; then
    echo "  ⚠ Could not subscribe to #${channel}:" >&2
    [ -s "$_err" ] && sed 's/^/      /' "$_err" >&2
    return 1
  fi

  # 2. Resolve-or-create the canonical gist on this gh account.
  local _gid
  _gid=$("$AIRC_PYTHON" -m airc_core.channel_gist resolve \
         --channel "$channel" --create-if-missing 2>"$_err")
  if [ -z "$_gid" ]; then
    echo "  ⚠ Could not resolve gist for #${channel}:" >&2
    [ -s "$_err" ] && sed 's/^/      /' "$_err" >&2
    return 1
  fi

  # 3. Persist channel→gist mapping for cmd_send + monitor routing.
  if ! "$AIRC_PYTHON" -m airc_core.config set_channel_gist \
       --config "$CONFIG" --channel "$channel" --gist-id "$_gid" 2>"$_err"; then
    echo "  ⚠ Could not persist channel→gist mapping for #${channel}:" >&2
    [ -s "$_err" ] && sed 's/^/      /' "$_err" >&2
    return 1
  fi

  printf '%s\n' "$_gid"
  return 0
}

cmd_connect() {
  # Flag parsing. Issue #37 — host display shapes:
  #   default (gh installed + authed): gist ID + humanhash mnemonic + long invite
  #   default (no gh OR gh not authed): long invite only (today's behavior)
  #   --no-gist                       : long invite only, even if gh works
  #
  # `--gist` and `-gist` accepted for explicitness/back-compat; both no-ops
  # because gist is now the default when gh is available. Gist push silently
  # falls through to long-invite-only when gh is missing or unauthed, so
  # the host command never fails just because GitHub isn't reachable.
  #
  # Room flags (issue #39 + #121):
  #   --room <name>       : join (or host) a named room (default: auto-scope
  #                         from git org, falling back to 'general')
  #   --no-room           : disable the substrate entirely; legacy 1:1
  #                         invite-string flow (use_room=0). Inherits #38
  #                         single-pair behavior. Aliased --no-general was
  #                         removed for this — those have different meanings.
  #   --no-general        : keep the project room, but DON'T also subscribe
  #                         to the #general lobby. Project-only focus mode.
  #                         (NEW; previously this was an alias for --no-room.)
  #   --room-only <name>  : explicit project room + no general sidecar.
  #                         Equivalent to `--room <name> --no-general`.
  #
  # Default behavior (issue #121): every `airc join` lands in BOTH the
  # auto-scoped project room AND #general. The general sidecar runs in a
  # sibling scope (.general suffix) under the same visible identity, so
  # AIs cross-pollinate between projects via the lobby while keeping
  # focused work in their project room. Set AIRC_GENERAL_SIDECAR=1 to
  # signal "this IS the sidecar, don't recurse" — internal-only.
  local use_gist=1   # default ON; runtime probe later checks gh availability
  local room_name="general"
  local room_explicit=0  # set to 1 when user passes --room explicitly
  local use_room=1   # default ON — auto-#general substrate

  # AIRC_ROOM_INTENT: re-exec env var preserving the user's --room
  # across a stale-host-takeover exec. Pre-fix this was lost on every
  # self-heal: user typed `airc join --room qa-foo`, we exec'd back
  # into `airc connect` with NO ARGS, auto-scope decided based on cwd
  # instead. Treat the env var as if --room was passed (since it was,
  # one process ago).
  if [ -n "${AIRC_ROOM_INTENT:-}" ] && [ "$room_explicit" = "0" ]; then
    room_name="$AIRC_ROOM_INTENT"
    use_room=1
    room_explicit=1
    unset AIRC_ROOM_INTENT  # one-shot — don't pollute child invocations
  fi
  local general_sidecar=1   # default ON (issue #121) — also subscribe to #general
  local _force_general_sidecar=0   # set by --general flag (issue #136 re-opt-in)
  # Recursion guard: when WE are the sidecar (spawned by another airc
  # connect), don't spawn our own sidecar. Otherwise: turtles all the way.
  [ "${AIRC_GENERAL_SIDECAR:-0}" = "1" ] && general_sidecar=0
  # User-facing env opt-out, equivalent to --no-general flag. Useful
  # for test harnesses that don't care about sidecar behavior, and
  # for one-off scoped scripts that want to set it once and forget.
  [ "${AIRC_NO_GENERAL:-0}" = "1" ] && general_sidecar=0
  # Declared at function scope so set -u doesn't bite when JOIN MODE runs
  # without a prior gist parser (inline-invite path skips the parser
  # entirely; resolved_room_name only gets a value when we resolved a
  # kind:room gist envelope).
  local resolved_room_name=""
  # _resolved_gist_id is captured by the gist resolver when discovery resolves
  # a kind:"room" gist. Used by JOIN MODE's self-heal path: if the pair
  # handshake fails because the host listed in the room gist is unreachable
  # (sleep/crash/network), the joiner deletes the stale gist and re-execs
  # itself in host mode — first-agent-back-in becomes the new host.
  local _resolved_gist_id=""
  # Heartbeat freshness vars - parsed by gist resolver in the room
  # case-arm. Must be defaulted here so the JOIN MODE early-takeover
  # check (which runs unconditionally if a target has '@') doesn't trip
  # 'unbound variable' when target came in inline (no gist resolved).
  local _resolved_heartbeat_stale=0
  local _resolved_heartbeat_age=""
  # Multi-address fields parsed from host.addresses[] in the room
  # gist envelope. _resolved_addresses_json is the raw JSON array
  # (or empty if the host published a legacy envelope with only
  # host.address/host.port). _resolved_host_machine_id lets the
  # joiner detect "we're on the same machine" and dial 127.0.0.1.
  local _resolved_addresses_json=""
  local _resolved_host_machine_id=""
  local positional=()
  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        echo "Usage: airc connect [target] [flags]"
        echo "  airc connect                   auto-discover mesh on your gh account"
        echo "  airc connect <gist-id>         join via shared gist id (cross-account)"
        echo "  airc connect <mnemonic>        join via humanhash phrase (same account)"
        echo "  airc connect <invite-string>   join via inline invite (legacy)"
        echo ""
        echo "Flags:"
        echo "  --room <name>                  set channel intent (auto-scoped from cwd if absent)"
        echo "  --room-only <name>             --room + --no-general"
        echo "  --no-room                      disable substrate entirely (legacy 1:1 invite)"
        echo "  --no-general                   keep project room, skip #general subscription"
        echo "  --general                      re-opt-in to #general after a prior /part"
        echo "  --no-gist                      don't publish/discover via gh gist (test mode)"
        echo "  --no-tailscale                 skip Tailscale even if installed"
        return 0 ;;
      --gist|-gist) use_gist=1; shift ;;
      --no-gist|-no-gist) use_gist=0; shift ;;
      --room|-room)
        room_name="${2:-general}"
        use_room=1
        room_explicit=1
        # Stash for re-exec preservation. Read by _self_heal_stale_host
        # in airc top-level when a stale-host-takeover happens mid-flow.
        ROOM_INTENT_FOR_REEXEC="$room_name"
        shift 2 ;;
      --no-room|-no-room) use_room=0; shift ;;
      --no-general|-no-general)
        # NEW semantic (issue #121): keep the project room substrate,
        # just don't ALSO subscribe to the #general lobby sidecar. This
        # used to alias --no-room (disable substrate entirely); the
        # behaviors are now distinct because dual-room presence is
        # default and users need a way to opt out of just the lobby
        # part without dropping back to legacy 1:1 invites.
        general_sidecar=0; shift ;;
      --general|-general)
        # Issue #136: explicit re-opt-in to #general after a prior
        # /part. Clears the room from primary scope's parted_rooms so
        # the sidecar resubscribes. Force general_sidecar=1 too in case
        # AIRC_GENERAL_SIDECAR=1 was set (recursion guard) — the user
        # is explicitly asking for the sidecar, override session env.
        # Symmetric inverse of --no-general.
        _force_general_sidecar=1; shift ;;
      --room-only|-room-only)
        # Combo: explicit project room + skip general sidecar. For
        # focused work where lobby noise would distract.
        room_name="${2:-general}"; use_room=1; room_explicit=1; general_sidecar=0
        ROOM_INTENT_FOR_REEXEC="$room_name"  # preserve across self-heal exec
        shift 2 ;;
      --no-tailscale|-no-tailscale)
        # Opt out of Tailscale entirely: skips the login prompt AND
        # drops the tailscale entry from host_address_set so the
        # gist envelope advertises only localhost+LAN. The flag is
        # the primary user-facing API; AIRC_NO_TAILSCALE=1 stays as
        # an internal toggle for code that already reads it.
        export AIRC_NO_TAILSCALE=1
        shift ;;
      *) positional+=("$1"); shift ;;
    esac
  done
  set -- "${positional[@]+"${positional[@]}"}"

  # Issue #136: --general re-opt-in. Clear parted state on primary
  # scope and force the sidecar back on. Done after arg parsing so we
  # know AIRC_WRITE_DIR (set by ensure_init below) is meaningful — but
  # we have to wait for ensure_init to run, since --general can be
  # called before any prior init. The cleanup happens via a deferred
  # check in spawn_general_sidecar_if_wanted: since _clear_parted_room
  # is idempotent, we can call it eagerly here when config exists, and
  # also force general_sidecar=1 to override any session env opt-out.
  if [ "$_force_general_sidecar" = "1" ]; then
    general_sidecar=1
    if [ -f "$AIRC_WRITE_DIR/config.json" ]; then
      local _primary_now; _primary_now=$(_primary_scope_for "$AIRC_WRITE_DIR")
      _clear_parted_room "$_primary_now" "general"
    fi
  fi

  # Phase 3c: Tailscale login nudge removed. Cross-network mesh now
  # routes via gh-as-bearer (envelope-encrypted gist), no Tailscale
  # daemon required. See project_airc_transport_architecture memory.

  # `airc join` (no args) auto-scopes to the room matching the current cwd.
  # Resolution: git remote org first ('useideem/authenticator' → #useideem),
  # parent-dir basename second (local-only repos). Falls back to #general
  # only when neither signal fires (non-git dir, no remote). The skill
  # /join contract documents this as the default.
  #
  # The trade-off: two tabs in DIFFERENT projects on the same gh account
  # land in different rooms (a #cambriantech tab can't see a #useideem
  # tab). That's intentional — project work shouldn't mix with unrelated
  # project chatter. Cross-project agents who need a shared lobby:
  # `AIRC_NO_AUTO_ROOM=1 airc join` or `airc join --room general`.
  #
  # Two tabs in the SAME project converge automatically: both useideem
  # tabs auto-scope to #useideem, both find each other. That's the case
  # this default optimizes for.
  #
  # History: this was rolled back in PR #104 over the cross-project
  # concern, then re-enabled here after dogfooding showed the converse
  # bug (two same-project tabs both defaulting to #general and never
  # converging on the project room) was the more painful failure mode.
  if [ "$use_room" = "1" ] && [ "$room_explicit" = "0" ] \
     && [ "${AIRC_NO_AUTO_ROOM:-0}" != "1" ]; then
    # Saved room_name (#130): the one piece of cross-restart state worth
    # trusting. If a prior connect landed us in #foo, the next bare
    # `airc connect` should target #foo too — not the auto-scope or the
    # "general" fallback. This replaces the resume code's room-tracking
    # with a single read of the saved file. Cached host_target is still
    # NOT trusted (discovery re-derives that from the gist).
    local _saved_room=""
    [ -f "$AIRC_WRITE_DIR/room_name" ] && _saved_room=$(cat "$AIRC_WRITE_DIR/room_name" 2>/dev/null)
    if [ -n "$_saved_room" ]; then
      room_name="$_saved_room"
      # Phase 2C clarity (continuum-b741's report): the mesh substrate
      # may steer us to a different host channel than our saved
      # preference. State the preference as INTENT, not promise — the
      # post-discovery banner is the authoritative "what you actually
      # joined" signal.
      echo "  Saved channel preference: #${room_name} (mesh may resolve a different host channel; 'airc part' to clear)"
    else
      local _inferred
      _inferred=$(infer_default_room 2>/dev/null || true)
      if [ -n "$_inferred" ]; then
        room_name="${_inferred%|*}"
        local _source="${_inferred#*|}"
        echo "  Auto-scoped: #${room_name} (from git ${_source}; override with --room or AIRC_NO_AUTO_ROOM=1)"
      fi
    fi
  fi

  local target="${1:-}"
  local reminder_interval="${AIRC_REMINDER:-${2:-300}}"  # env > positional > 5min default

  # ── Notification-sink liveness ─────────────────────────────────────
  # `airc connect` is only useful when a CONSUMER is reading our stdout —
  # that's how inbound peer messages reach the AI agent or human. The
  # canonical launcher is Claude Code's Monitor (persistent=true, command=
  # "airc connect ...") which streams every stdout line as a notification.
  #
  # Failure mode this catches: someone runs `airc connect <invite>` via a
  # one-shot Bash tool / nohup / background `&` / detached shell. The
  # python formatter + ssh tail get spawned, the pairing succeeds, the
  # local messages.jsonl fills correctly — but stdout has no reader (the
  # bash that exec'd us already exited and closed the pipe), so inbound
  # NEVER reaches the agent's notification surface. Looks paired, is
  # functionally deaf. Cost a session of debugging on 2026-04-23.
  #
  # Approach: install a SIGPIPE handler that exits LOUDLY (to stderr,
  # which usually survives) the moment any write to stdout fails. Plus a
  # periodic heartbeat line every 60s so SIGPIPE actually fires if there's
  # no reader. With both:
  #   - Monitor reading: heartbeats succeed silently (Monitor surfaces
  #     them as benign notifications, but they're harmless)
  #   - One-shot bash / nohup / background: first heartbeat triggers
  #     SIGPIPE → airc exits with a clear error pointing at the right
  #     launch pattern → no silent deafness
  #
  # Opt out: AIRC_BACKGROUND_OK=1 disables the heartbeat for legitimate
  # background launches (systemd unit + dedicated tail consumer, tests).
  trap '
    {
      echo ""
      echo "❌ airc connect: stdout pipe closed — no notification consumer."
      echo ""
      echo "   Inbound peer messages would have been silently lost. Most"
      echo "   common cause: airc was launched as a one-shot bash exec,"
      echo "   nohup, background \"&\", or detached shell. The pairing"
      echo "   succeeds and messages.jsonl fills, but the AI agent never"
      echo "   sees inbound notifications. That is the worst kind of"
      echo "   silent failure — looks fine, is broken."
      echo ""
      echo "   Right launchers:"
      echo "     • Claude Code skill:   /airc:connect <invite>"
      echo "     • Monitor tool:        Monitor(persistent=true, command=\"airc connect <invite>\")"
      echo "     • Interactive shell:   just type \`airc connect <invite>\` at a TTY"
      echo ""
      echo "   Bypass for legitimate background use (systemd + log tail,"
      echo "   tests): export AIRC_BACKGROUND_OK=1"
      echo ""
    } >&2
    exit 3
  ' PIPE
  # Heartbeat to stdout for SIGPIPE-pipe-death detection. OFF BY DEFAULT
  # as of 2026-04-24 — at 60s it was filling Claude Code chat history
  # with a notification per minute per peer, drowning real peer events.
  # Joel: "I'd rather only see the messages."
  #
  # Real peer traffic still triggers SIGPIPE on pipe death, so we lose
  # detection only when the channel is genuinely silent for a long time.
  # That tradeoff is worth it for the cleaner Monitor surface.
  #
  # Set AIRC_HEARTBEAT_SEC=<seconds> to opt back in (tests, diagnostic
  # sessions, one-shot-bash launchers that need the safety net). 0 or
  # unset = no heartbeat.
  if [ -z "${AIRC_BACKGROUND_OK:-}" ] && [ -n "${AIRC_HEARTBEAT_SEC:-}" ] && [ "$AIRC_HEARTBEAT_SEC" -gt 0 ] 2>/dev/null; then
    (
      while sleep "$AIRC_HEARTBEAT_SEC"; do
        echo "  [airc heartbeat $(date -u +%H:%M:%SZ)]"
      done
    ) &
  fi

  # Auto-teardown any stale airc process in this scope before starting fresh.
  # Previously users had to run `airc teardown` manually before `airc connect`
  # if a prior monitor was still around — easy to forget, often resulted in
  # duplicate monitors or port collisions. Now a single `airc connect` or
  # `airc resume` does the right thing.
  # #292 fix: refuse to stomp a live monitor. Pre-fix this block
  # auto-killed any PIDs in airc.pid before continuing — which silently
  # destroyed a live monitor in a sibling shell when the user ran
  # `airc connect` from a second terminal to verify state. That made
  # multi-tab sanity-checking destructive. Post-fix: detect liveness,
  # print a one-liner pointing to the right tools, exit 0 cleanly.
  # Stale pidfile (no live PIDs) still gets cleaned up + we proceed.
  local stale_pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ -f "$stale_pidfile" ]; then
    local stale_pids; stale_pids=$(cat "$stale_pidfile" 2>/dev/null | tr '\n' ' ')
    local any_alive=0
    for p in $stale_pids; do
      kill -0 "$p" 2>/dev/null && any_alive=1
    done
    if [ "$any_alive" = "1" ]; then
      echo "  airc connect: this scope's monitor is already running (PIDs: $stale_pids)."
      echo "    To stop it:        airc teardown"
      echo "    To restart it:     airc teardown && airc connect"
      echo "    To check it:       airc status"
      return 0
    fi
    # Stale pidfile (no live processes) — safe to clean.
    rm -f "$stale_pidfile"
  fi

  # No resume code path. (#130, 2026-04-26.)
  #
  # The gist is the source of truth for who's hosting which room and at
  # what address. Local state we trust across restarts is identity (ssh
  # key, signing key, name, identity blob) and peer records. We do NOT
  # trust cached host_target / host_port / host_ssh_pub — those describe
  # external substrate that can change behind us (host crashed, port
  # auto-bumped, gist regenerated, ssh key rotated, machine restarted).
  #
  # Every `airc connect` runs discovery. Cost: one `gh gist list`
  # (~200ms). Benefit: every "saved pairing diverged from gist" failure
  # mode is structurally impossible — there's no saved pairing to
  # diverge. Discovery + JOIN MODE below already handle stale-heartbeat
  # takeover, TCP-unreachable self-heal, race-loser detection, multi-
  # address pick, Tailscale-down advisory, and host_target overwrite on
  # successful pair. Removing the parallel resume implementation deletes
  # ~250 lines and an entire bug class:
  #   - "(SSH verified)" printed against an unreachable cached host
  #   - silent-success on stale pair after machine restart
  #   - --room flag silently ignored if it differed from saved pairing
  #   - 404 self-heal gated on a separate code path with its own bugs
  # Cached CONFIG fields like host_target are still WRITTEN by JOIN MODE
  # for monitor() to read at runtime ("am I joiner or host?"), but never
  # READ at connect-time to skip discovery.

  # ── Zero-arg discovery: rooms first, then legacy invites (#38, #39)
  # If we got here with no target AND no saved config, the user just ran
  # `airc connect` cold. The IRC substrate (#39) makes this simple:
  #
  #   1. Look for the named room gist (default `airc room: general`).
  #      Found → auto-join it.
  #   2. Fall back to legacy `airc invite for ...` single-pair gists.
  #      Found 1 → auto-join. Found N → list + exit.
  #   3. Found nothing → become the host and create the room (the
  #      auto-#general default — first agent in is the channel host).
  #
  # Skipped if `gh` isn't available (degraded → host invite-only) or
  # AIRC_NO_DISCOVERY=1 (explicit opt-out). With `--no-general` the room
  # path is skipped and we go straight to single-pair invite host mode.
  #
  # Discovery gate: run only when the user didn't pass an explicit target
  # and gh is available. We deliberately do NOT short-circuit when CONFIG
  # has a saved host_target — that's exactly the cached-pairing path the
  # resume-deletion (#130) is killing. Always discover, always consult
  # the gist; the gist is the truth.
  local _did_room_discovery=0
  if [ -z "$target" ] && \
     [ "${AIRC_NO_DISCOVERY:-0}" != "1" ] && \
     command -v gh >/dev/null 2>&1; then

    # ── Mesh discovery (singleton per gh account) ────────────────
    # Architectural shift from the per-room model: ONE gist per gh
    # account, description literal "airc mesh". Every `airc join` on
    # the account converges on it. _mesh_find returns the singleton
    # (oldest-by-created if multiple are present from a race).
    #
    # The --room flag still records the channel(s) this client wants
    # to subscribe to (Phase 2 will route messages by channel), but it
    # no longer drives gist discovery — every subscriber on the account
    # converges on the same host.
    _did_room_discovery=1
    local _mesh_id; _mesh_id=$(_mesh_find)
    if [ -n "$_mesh_id" ]; then
      echo "  Found mesh on your gh account → joining ($_mesh_id)"
      target="$_mesh_id"
      # fall through to gist resolver below
    else
      echo "  No mesh found on your gh account → becoming the host."
      # Race against a concurrent host attempt is handled POST-publish
      # via _mesh_take_over (see host-publish path below).
    fi

    # ── Legacy single-pair invite discovery ──────────────────────
    # Preserved for cross-account ad-hoc pairing where a friend on a
    # DIFFERENT gh account shares an `airc invite for ...` gist id.
    # Same-account discovery uses the mesh path above; this only
    # fires when the user explicitly opted out of mesh + room.
    if [ -z "$target" ] && [ "$use_room" = "0" ]; then
      local _candidates; _candidates=$(gh gist list --limit 30 2>/dev/null \
        | awk -F'\t' '/airc invite for/ { print $1 "\t" $2 }')
      local _count; _count=$(printf '%s' "$_candidates" | grep -c . || true)
      if [ "$_count" = "1" ]; then
        local _picked_id; _picked_id=$(printf '%s' "$_candidates" | awk -F'\t' '{print $1}')
        local _picked_desc; _picked_desc=$(printf '%s' "$_candidates" | awk -F'\t' '{print $2}')
        echo "  Found 1 open airc invite on your gh account: $_picked_desc"
        echo "  → auto-joining $_picked_id"
        target="$_picked_id"
      elif [ "$_count" -ge 2 ]; then
        echo ""
        echo "  $_count open airc invite(s) on your gh account:"
        echo ""
        printf '%s\n' "$_candidates" | while IFS=$'\t' read -r _id _desc; do
          local _hh; _hh=$(humanhash "$_id" 2>/dev/null)
          printf '    %s   %s\n      mnemonic: %s\n' "$_id" "$_desc" "$_hh"
        done
        echo ""
        echo "  Pick one to join:  airc connect <id>"
        echo "  Host a new mesh:   AIRC_NO_DISCOVERY=1 airc connect --no-general"
        exit 0
      fi
    fi
  fi

  # ── Mnemonic resolver (humanhash → gist id, same gh account) ─────
  # Joel's UX target: a friend (or your own other tab) can type
  #   airc connect oregon-uncle-bravo-eleven
  # instead of pasting a 32-char hex gist id. Humanhash is one-way
  # (XOR-fold of the gist id bytes), so we can't reverse it directly —
  # but we CAN walk gh's gist list, hash each id, and pick the match.
  #
  # Detection: target looks like a hyphen-separated 3+ word phrase of
  # lowercase alphabetic tokens (matches the humanhash dictionary
  # convention — no digits, no underscores). Example acceptable form:
  # `oregon-uncle-bravo-eleven`. Reject `2f6a907224f4...` (it's a hex id),
  # `gist:abc123` (handled below), inline invites with `@`, etc.
  #
  # Scope: same-gh-account only (we list OUR own gists). Cross-account
  # (Friend on a different gh) requires the `user/mnemonic` form which
  # is roadmap. For now the friend pastes the gist id directly when
  # accounts differ.
  if [ -n "$target" ] && echo "$target" | grep -qE '^[a-z]+(-[a-z]+){2,}$'; then
    if ! command -v gh >/dev/null 2>&1; then
      die "Mnemonic '$target' lookup needs the 'gh' CLI. Install gh + 'gh auth login', or use the gist id directly: airc connect <id>"
    fi
    local _matched_gist_id=""
    while IFS=$'\t' read -r _gid _; do
      [ -z "$_gid" ] && continue
      local _hh; _hh=$(humanhash "$_gid" 2>/dev/null)
      if [ "$_hh" = "$target" ]; then
        _matched_gist_id="$_gid"
        break
      fi
    done < <(gh gist list --limit 50 2>/dev/null | awk -F'\t' '/airc mesh|airc room:|airc invite for/ { print $1 "\t" $2 }')
    if [ -n "$_matched_gist_id" ]; then
      echo "  Resolved mnemonic '$target' → gist $_matched_gist_id"
      target="$_matched_gist_id"
    else
      die "Mnemonic '$target' didn't match any airc gist on this gh account. If your friend's gist is on a different gh, paste the gist id directly: airc connect <id>"
    fi
  fi

  # ── Gist transport (issue #37) ───────────────────────────────────
  # If the target doesn't look like an inline invite (no `@`), treat it
  # as a gist ID and fetch the real invite content from there. Three
  # accepted shapes:
  #   gist:<id>   — explicit, unambiguous
  #   <id>        — bare alphanumeric, auto-detected as a gist ID
  #   foo@bar@... — today's inline invite, untouched
  #
  # The whole point: an inline invite is ~200 chars of base64 that gets
  # mangled by chat clients (line wraps, auto-linkification, smart
  # quotes). A 7-char gist ID survives every transport. Host pushes the
  # invite to a secret gist (see `airc connect --gist` below); receiver
  # pastes just the ID. Also: gist works as a coordination layer for
  # cross-tailnet pairing where the two peers don't share a VPN
  # initially.
  #
  # Gist payload format: a versioned JSON envelope (see host-side push
  # below for shape). Receiver parses `{ airc: 1, kind: "invite", invite: "..." }`
  # and dispatches on `kind`. Today only `kind: "invite"` is recognized.
  # Future kinds (cross-tailnet relay, bootstrap, webrtc-mesh) slot in
  # by adding a case below — old peers reject the kind cleanly with a
  # version-mismatch message instead of silently misinterpreting bytes.
  #
  # Backward compat: a gist that contains a raw invite string (no JSON
  # envelope) still parses — we fall through to the raw-string branch
  # if JSON parse fails. Lets pre-envelope gists keep working.
  if [ -n "$target" ] && ! echo "$target" | grep -q '@'; then
    local gist_id="${target#gist:}"
    # Capture for self-heal in JOIN MODE: if the host in this gist turns
    # out to be unreachable, JOIN MODE deletes the gist by this id + takes
    # over as the new host of the same room.
    _resolved_gist_id="$gist_id"
    # Gist IDs are hex strings, typically 20-32 chars but accept any
    # plausible length so future GH ID schemes don't break us.
    if echo "$gist_id" | grep -qE '^[a-zA-Z0-9]{6,40}$'; then
      echo "  Resolving gist $gist_id ..."
      local raw_content=""
      # Each path's `raw_content=$(cmd | filter)` is protected with
      # `|| true` so a non-zero exit on the upstream command does NOT
      # abort the script via `set -euo pipefail`. Pre-fix: when gh
      # rate-limited (HTTP 403), `gh api ...` exited non-zero, pipefail
      # propagated it, set -e aborted the whole script BEFORE the next
      # fallback ran. Net: rate-limit hit = total resolution failure
      # with no diagnostic. Joel 2026-04-27: "this limit will kill
      # people." Fix: per-path `|| true` makes each path advisory; the
      # `[ -z "$raw_content" ]` gates control fallthrough explicitly.
      #
      # Prefer `gh api` over `gh gist view --raw` — the latter prepends
      # the gist description as a header line ("airc room: general\n\n{...}")
      # which breaks JSON parse downstream. `gh api` returns the file
      # content cleanly. This bug bit hard during daemon-install dogfood:
      # parser fell through to the @.*@ regex fallback which captured the
      # malformed JSON `"invite": "..."` line (quotes and all), pair
      # handshake failed on garbage host info, and self-heal didn't fire
      # because resolved_room_name was never extracted via the jq path.
      # #188: Python (stdlib JSON) replaces jq. gh api → gistparse extracts
      # the first file's content. This is the rest-API path; it's preferred
      # over the gh gist view --raw path because the latter prepends the
      # gist description as a header line that we'd then have to strip.
      if command -v gh >/dev/null 2>&1; then
        raw_content=$( (gh api "gists/$gist_id" 2>/dev/null \
                        | "$AIRC_PYTHON" -m airc_core.gistparse gist_content 2>/dev/null) || true )
      fi
      # Fallback path 1: gh raw view (description leak handled by the
      # awk strip below at "head -c 1 | grep '{'" cleanup).
      if [ -z "$raw_content" ] && command -v gh >/dev/null 2>&1; then
        raw_content=$(gh gist view "$gist_id" --raw 2>/dev/null || true)
      fi
      # Fallback path 2: git clone the gist's git remote. CRITICAL — this
      # is the rate-limit-bypass path. The REST API has a tight gist
      # sub-bucket (~60 reads/hr); a busy session blows through it
      # quickly and EVERY `gh api gists/<id>` and `gh gist view <id>`
      # call HTTP 403's. Git transport at gist.github.com uses git HTTP
      # over the same auth but on a separate quota — it keeps working
      # when REST is throttled. The git-clone fallback adds ~1s on the
      # slow path but unblocks discovery completely.
      if [ -z "$raw_content" ] && command -v git >/dev/null 2>&1; then
        local _gist_tmp; _gist_tmp=$(mktemp -d -t airc-gist-resolve.XXXXXX 2>/dev/null || echo "")
        if [ -n "$_gist_tmp" ] && git clone --depth 1 --quiet "https://gist.github.com/$gist_id.git" "$_gist_tmp" 2>/dev/null; then
          # Gists typically contain ONE file (airc envelopes always do).
          # Take the first non-dotfile, non-.git entry. If a future gist
          # shape ships multiple files we'll add an explicit airc-envelope
          # filename convention; for now the single-file assumption is
          # sound across every gist airc has ever published.
          local _gist_file
          _gist_file=$(find "$_gist_tmp" -maxdepth 1 -type f ! -name '.git*' 2>/dev/null | head -1 || true)
          if [ -n "$_gist_file" ] && [ -f "$_gist_file" ]; then
            raw_content=$(cat "$_gist_file" 2>/dev/null || true)
          fi
        fi
        [ -n "$_gist_tmp" ] && rm -rf "$_gist_tmp"
      fi
      # Fallback path 3: anonymous curl + Python for environments
      # without gh OR git. Last resort. (#188 — was curl + jq.)
      if [ -z "$raw_content" ] && command -v curl >/dev/null 2>&1; then
        raw_content=$( (curl -fsSL "https://api.github.com/gists/$gist_id" 2>/dev/null \
                        | "$AIRC_PYTHON" -m airc_core.gistparse gist_content 2>/dev/null) || true )
      fi
      # Last-resort cleanup: if raw_content still has the description-header
      # leak from a degraded gh-view path, strip lines before the first '{'
      # (room/invite envelopes are JSON, always start with '{').
      if [ -n "$raw_content" ] && ! printf '%s' "$raw_content" | head -c 1 | grep -q '{'; then
        raw_content=$(printf '%s' "$raw_content" | awk '/^\{/{flag=1} flag')
      fi
      if [ -z "$raw_content" ]; then
        die "Failed to fetch gist '$gist_id'. Check the ID, network, and (if private) 'gh auth login'."
      fi

      # Try parse as airc JSON envelope first. If it parses + has airc
      # field, dispatch on `kind`. Otherwise, treat raw_content as the
      # legacy raw-invite-string format (backward compat).
      # _resolved_heartbeat_stale + _resolved_heartbeat_age are declared
      # at function-scope above so the JOIN MODE check sees them on the
      # inline-invite path too (where this gist block doesn't run).
      local resolved=""
      # #188: was `if command -v jq`; now Python is the truth-layer
      # (always available since #152 Phase 0). Drop the jq gate.
      local airc_ver kind
      airc_ver=$(printf '%s' "$raw_content" | "$AIRC_PYTHON" -m airc_core.gistparse get .airc 2>/dev/null)
      kind=$(printf '%s' "$raw_content" | "$AIRC_PYTHON" -m airc_core.gistparse get .kind 2>/dev/null)
      if [ -n "$airc_ver" ]; then
          # Versioned envelope — dispatch on kind.
          case "$kind" in
            invite)
              # Single-pair invite (legacy + --no-general flow). Gist is
              # ephemeral; host deletes after pair.
              resolved=$(printf '%s' "$raw_content" \
                         | "$AIRC_PYTHON" -m airc_core.gistparse get .invite 2>/dev/null \
                         | head -1 | tr -d '\r\n ')
              ;;
            mesh|room)
              # Mesh: ONE persistent gist per gh account, shared across
              # all subscribers. Same SSH-pair handshake as invite; the
              # gist persists so additional joiners keep arriving. The
              # `room` kind is the legacy per-room shape — handled here
              # for back-compat with gists that haven't rolled to mesh
              # yet (joiner can read either). The .invite field carries
              # today's name@user@host:port#pubkey string.
              resolved=$(printf '%s' "$raw_content" \
                         | "$AIRC_PYTHON" -m airc_core.gistparse get .invite 2>/dev/null \
                         | head -1 | tr -d '\r\n ')
              # New mesh shape: .channels[]; legacy room shape: .name.
              # Prefer channels[0] if present; fall back to .name.
              resolved_room_name=$(printf '%s' "$raw_content" \
                         | "$AIRC_PYTHON" -m airc_core.gistparse get_first_of '.channels[0]' '.name' 2>/dev/null)
              # Multi-address: capture host.addresses[] + host.machine_id
              # for the joiner's address-picker (peer_pick_address). Empty
              # if the host published a pre-multi-address envelope; in
              # that case JOIN MODE falls back to the parsed-from-invite
              # host:port (legacy single-address path).
              _resolved_addresses_json=$(printf '%s' "$raw_content" \
                         | "$AIRC_PYTHON" -m airc_core.gistparse get_json .host.addresses 2>/dev/null)
              _resolved_host_machine_id=$(printf '%s' "$raw_content" \
                         | "$AIRC_PYTHON" -m airc_core.gistparse get .host.machine_id 2>/dev/null)

              # Heartbeat freshness check — the structural fix for
              # orphan-gist class. Hosts update last_heartbeat every
              # AIRC_HEARTBEAT_SEC (default 30s); if it's older than
              # AIRC_HEARTBEAT_STALE (default 90s = 3 missed beats),
              # the host is dead. We short-circuit the SSH attempt and
              # take over directly — no minute-long timeout, no peer
              # confusion about "is this thing on?". Pre-heartbeat
              # gists (no field) are treated as fresh for backward
              # compat; their hosts will get caught by the existing
              # SSH-failure self-heal path at line ~1850.
              local _hb_iso _hb_ts _now_ts _hb_stale_sec
              _hb_iso=$(printf '%s' "$raw_content" | "$AIRC_PYTHON" -m airc_core.gistparse get .last_heartbeat 2>/dev/null)
              _hb_stale_sec="${AIRC_HEARTBEAT_STALE:-90}"
              if [ -n "$_hb_iso" ]; then
                # Cross-platform ISO→epoch via the iso_to_epoch adapter.
                # Pre-adapter this site had its own BSD/GNU date fallback
                # chain (one of three duplicates that drifted indepen-
                # dently — see commit history before the dedupe).
                _hb_ts=$(iso_to_epoch "$_hb_iso")
                if [ -n "$_hb_ts" ]; then
                  _now_ts=$(date -u +%s)
                  _resolved_heartbeat_age=$(( _now_ts - _hb_ts ))
                  if [ "$_resolved_heartbeat_age" -gt "$_hb_stale_sec" ]; then
                    _resolved_heartbeat_stale=1
                  fi
                fi
              fi
              ;;
            "")
              die "Gist has airc envelope (v$airc_ver) but no 'kind' field — malformed."
              ;;
            *)
              # Unknown kind — fail loud. Old peers should reject
              # rather than silently misinterpret a future kind.
              die "Gist uses unknown kind '$kind' (airc v$airc_ver). This receiver only supports 'invite', 'room', and 'mesh'. Update airc: 'airc update'."
              ;;
          esac
      fi
      if [ -z "$resolved" ]; then
        # Legacy raw-string format (pre-#222 envelope shape) — take the
        # first non-empty line that looks like an invite. Still needed
        # for cross-account paste of a hand-built invite without JSON.
        resolved=$(printf '%s' "$raw_content" | grep -E '@.*@' | head -1 | tr -d '\r\n ')
        # If the matched line is from a JSON envelope (e.g.
        # `"invite": "name@user@host:port#..."`), the grep grabs the
        # whole quoted line including the JSON-key prefix. Strip
        # leading non-name characters: anything before the first letter
        # is JSON syntax (quotes, colons, whitespace). Found by
        # continuum-b69f Win→Mac e2e 2026-04-27 — bash on Git Bash
        # ships without jq, falls through to this path, captured
        # `"invite":"authenticator-fd63@...` as the invite, then the
        # downstream @-split made the displayed peer name include
        # the JSON-key fragment AND prevented resolved_room_name from
        # ever being set (no JSON parse, no .name extraction). Strip
        # everything up to the first letter or hyphen, then re-validate.
        resolved=$(printf '%s' "$resolved" | sed -E 's/^[^a-zA-Z]+//')
        # Fallback room-name extraction when jq is missing: grep the
        # raw_content for `"name": "..."` and capture the value. Same
        # JSON envelope shape as the jq path; sed-only so it works on
        # bare-bones environments. Empty if not present (legacy gist).
        if [ -z "$resolved_room_name" ]; then
          resolved_room_name=$(printf '%s' "$raw_content" \
            | grep -oE '"name"[[:space:]]*:[[:space:]]*"[^"]+"' \
            | head -1 \
            | sed -E 's/^"name"[[:space:]]*:[[:space:]]*"([^"]+)"$/\1/')
        fi
      fi
      if [ -z "$resolved" ] || ! echo "$resolved" | grep -q '@'; then
        die "Failed to resolve gist '$gist_id' to a valid invite (got: $(printf '%s' "$raw_content" | head -c 80)...)"
      fi
      echo "  ✓ Resolved invite from gist."
      target="$resolved"
    fi
  fi

  if [ -n "$target" ] && echo "$target" | grep -q '@'; then
    # ── JOIN MODE ──────────────────────────────────────────────────

    # Stale-heartbeat fast-path takeover. If the gist we resolved had a
    # last_heartbeat older than AIRC_HEARTBEAT_STALE (parsed above), the
    # host is dead. Skip the SSH attempt entirely — no minute-long TCP
    # timeout, no peer wondering "is this thing on" — go straight to
    # take-over. Same operations as the SSH-failure self-heal at the
    # bottom of JOIN MODE (delete stale gist, re-exec as host with
    # AIRC_NO_DISCOVERY=1) but triggered from positive evidence (stale
    # presence signal) rather than negative evidence (TCP timeout).
    #
    # Backward compat: pre-heartbeat gists have no last_heartbeat field,
    # _resolved_heartbeat_stale stays 0, this block is a no-op, and the
    # SSH-failure self-heal still catches the dead host (slower, but
    # correct).
    if [ "$_resolved_heartbeat_stale" = "1" ] && [ -n "$resolved_room_name" ] \
       && [ -n "$_resolved_gist_id" ] && command -v gh >/dev/null 2>&1; then
      echo ""
      echo "  ⚠  Host of #${resolved_room_name} is stale (last heartbeat ${_resolved_heartbeat_age}s ago) — taking over..."
      echo "     (prior host's gist: $_resolved_gist_id)"

      # Same race-loser detection as the SSH-failure self-heal path
      # below. Two tabs concurrently deciding "host is stale" both
      # delete + publish, end up with split-brain — caught only by
      # running two tabs together.
      _self_heal_stale_host "$_resolved_gist_id"
    fi

    # Parse name@user@host[:port]#pubkey
    local host_ssh_pubkey_b64=""
    if echo "$target" | grep -q '#'; then
      host_ssh_pubkey_b64="${target##*#}"
      target="${target%%#*}"
    fi

    local peer_name ssh_target peer_port="7547"
    peer_name="${target%%@*}"
    ssh_target="${target#*@}"
    # Extract :port if present at the end of the host part
    if echo "$ssh_target" | grep -qE ':[0-9]+$'; then
      peer_port="${ssh_target##*:}"
      ssh_target="${ssh_target%:*}"
    fi

    [ -z "$peer_name" ] || [ -z "$ssh_target" ] && die "Format: airc connect name@user@host"

    # Multi-address override: if the gist envelope carried host.addresses[]
    # and host.machine_id, use peer_pick_address to choose the cheapest
    # reachable scope (same-machine localhost > same-LAN > tailscale).
    # This is what makes Tailscale truly optional — same-machine and
    # same-LAN peers connect via 127.0.0.1 / LAN IP regardless of the
    # invite string's host:port (which historically advertised one IP).
    if [ -n "$_resolved_addresses_json" ] && [ "$_resolved_addresses_json" != "null" ]; then
      local _picked; _picked=$(peer_pick_address "$_resolved_addresses_json" "$_resolved_host_machine_id")
      if [ -n "$_picked" ]; then
        local _picked_addr="${_picked%|*}"
        local _picked_port="${_picked#*|}"
        # Reconstruct ssh_target with the user@addr form. Original
        # ssh_target was user@invite-string-host; preserve the user.
        local _ssh_user="${ssh_target%@*}"
        if [ "$_ssh_user" = "$ssh_target" ]; then _ssh_user=""; fi
        ssh_target="${_ssh_user:+${_ssh_user}@}${_picked_addr}"
        peer_port="$_picked_port"
        echo "  ✓ Multi-address pick: ${_picked_addr}:${_picked_port} (from host.addresses)"
      fi
    fi

    local my_name
    my_name=$(resolve_name)
    init_identity "$my_name"

    # Merge into existing config.json instead of clobbering — preserves
    # the `identity` block (issue #34) across re-pairs so a teardown +
    # rejoin keeps pronouns/role/bio/status without requiring users to
    # re-run airc identity set every time.
    # Detect host change: if our saved host_target differs from the new
    # one, the per-host offset (.airc/monitor_offset) is meaningless —
    # line N of host A's log isn't line N of host B's log. Drop the
    # offset so the next monitor cycle starts at -n 0 (current EOF) of
    # the new host's log instead of replaying random history.
    local _prev_host_target; _prev_host_target=$(get_config_val host_target "")
    if [ -n "$_prev_host_target" ] && [ "$_prev_host_target" != "$ssh_target" ]; then
      rm -f "$AIRC_WRITE_DIR/monitor_offset" 2>/dev/null || true
    fi

    set_config_val name        "$my_name"
    set_config_val host        "$(get_host)"
    set_config_val host_target "$ssh_target"
    set_config_val created     "$(timestamp)"

    # Remember which room we joined (issue #39). Lets `airc rooms` and
    # status/diagnostics report channel context, and gives the joiner
    # something to hand to a friend ("airc connect <this-id>"). We don't
    # need the gist_id for cmd_part on joiner side — only the host owns
    # the gist lifecycle — but we save the room name for display.
    if [ -n "$resolved_room_name" ]; then
      # Phase 2B.2.1 (continuum-b741's WART 1): joiner's cwd-derived or
      # explicit --room intent must NOT be overwritten by the host's
      # advertised channel. If the user wanted #cambriantech (cwd) and
      # the mesh host happens to advertise #useideem, the joiner is
      # subscribed to BOTH — cmd_send default = user's intent; the
      # host's channel is tagged on too so their traffic still displays
      # via [#useideem] prefix.
      #
      # The legacy room_name file gets the user's intent when it differs
      # (so cmd_send's third-priority fallback also picks the right
      # default for users still on Phase 1 code).
      local _intent="$room_name"
      if [ -z "$_intent" ] || [ "$_intent" = "$resolved_room_name" ]; then
        echo "$resolved_room_name" > "$AIRC_WRITE_DIR/room_name"
        ensure_channel_subscribed_with_gist "$resolved_room_name" --first >/dev/null \
          || die "Could not bootstrap #${resolved_room_name}; refusing to join with broken state"
        echo "  Joined #${resolved_room_name}"
      else
        # Diverged: user wanted X, host advertises Y. Subscribe to both,
        # X first (default for cmd_send), Y appended (display shows
        # host's channel traffic too). The user's intent gets a real
        # gist (find-or-create) — that's what was missing pre-2026-04-29
        # and turned `airc join --room qa-foo` into a phantom-room.
        echo "$_intent" > "$AIRC_WRITE_DIR/room_name"
        ensure_channel_subscribed_with_gist "$_intent" --first >/dev/null \
          || die "Could not bootstrap #${_intent}; refusing to join with broken state"
        ensure_channel_subscribed_with_gist "$resolved_room_name" >/dev/null \
          || echo "  ⚠ Could not bootstrap host's channel #${resolved_room_name}; subscribed to #${_intent} only" >&2
        echo "  Joined mesh — host primarily labels #${resolved_room_name}; subscribed: #${_intent} (default), #${resolved_room_name}"
      fi
      # Identity bootstrap nudge (#146). Skill /join SKILL.md prompts
      # AIs to set pronouns/role/bio at first join, but users running
      # airc directly (no skill) never get the prompt and end up with
      # all-(unset) whois forever. Code-level one-time nudge here.
      _identity_bootstrap_nudge_if_unset || true
    fi

    # Exchange keys with host via TCP (port 7547) — public keys only
    # Pre-authorize host's pubkey if in join string
    if [ -n "$host_ssh_pubkey_b64" ]; then
      local host_ssh_pubkey
      host_ssh_pubkey=$(echo "$host_ssh_pubkey_b64" | base64 -d 2>/dev/null || echo "$host_ssh_pubkey_b64" | base64 -D 2>/dev/null || true)
      if [ -n "$host_ssh_pubkey" ]; then
        mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"
        grep -qF "$host_ssh_pubkey" "$HOME/.ssh/authorized_keys" 2>/dev/null || {
          echo "$host_ssh_pubkey" >> "$HOME/.ssh/authorized_keys"
          chmod 600 "$HOME/.ssh/authorized_keys"
        }
      fi
    fi

    # Exchange keys with host via TCP
    local peer_host_only="${ssh_target##*@}"

    # Phase 3c: Tailscale-down pre-flight removed. Cross-network mesh
    # routes via gh-as-bearer (envelope-encrypted gist) which doesn't
    # need Tailscale at all.

    echo "  Connecting to $peer_host_only:$peer_port..."
    local my_ssh_pub my_sign_pub my_x25519_pub
    my_ssh_pub=$(cat "$IDENTITY_DIR/ssh_key.pub" 2>/dev/null)
    my_sign_pub=$(cat "$IDENTITY_DIR/public.pem" 2>/dev/null)
    # Phase E.2: include our X25519 pubkey for envelope encryption.
    # bootstrap is idempotent (no-ops if keypair exists). Empty value
    # if cryptography isn't installed — handshake stays compatible
    # with peers running pre-Phase-E airc.
    my_x25519_pub=$("$AIRC_PYTHON" -m airc_core.identity bootstrap --dir "$IDENTITY_DIR" 2>/dev/null || echo "")

    # Read own identity blob to send in handshake (issue #34 v2 — peers
    # cache each other's identity at pair-time so airc whois works fast).
    local my_identity_json; my_identity_json=$(CONFIG="$CONFIG" "$AIRC_PYTHON" -c '
import json, os
try:
    c = json.load(open(os.environ["CONFIG"]))
    print(json.dumps(c.get("identity", {})))
except Exception:
    print("{}")
' 2>/dev/null)
    [ -z "$my_identity_json" ] && my_identity_json="{}"

    local response
    local _pair_ok=1
    # Migrated to airc_core.handshake send with proper --flags (not env
    # vars). MSYS path-translation on Git Bash silently mangles env-var
    # values that look like Unix paths (/Users/... → C:/Program
    # Files/Git/Users/...) when they cross to a Windows-binary subprocess.
    # argparse --flags are per-arg-predictable (callers can //-prefix
    # or set MSYS2_ARG_CONV_EXCL targeted-ly). Continuum-b69f 2026-04-27
    # traced the env-var path-mangling class.
    response=$("$AIRC_PYTHON" -m airc_core.handshake send "$peer_host_only" "$peer_port" \
                  --my-name "$my_name" \
                  --my-host "$(whoami)@$(get_host)" \
                  --my-ssh-pub "$my_ssh_pub" \
                  --my-sign-pub "$my_sign_pub" \
                  --my-x25519-pub "$my_x25519_pub" \
                  --my-airc-home "$AIRC_WRITE_DIR" \
                  --my-identity-json "$my_identity_json" 2>&1) || _pair_ok=0

    if [ "$_pair_ok" = "0" ]; then
      # ── Self-heal: stale-host takeover ─────────────────────────────
      # If discovery handed us a kind:room gist AND the host listed in it
      # is unreachable, the most likely cause is the prior host went away
      # (laptop sleep, crash, network blip). Per Joel: "no claude left
      # behind" — first agent back in becomes the new host of #general.
      #
      # Mechanics:
      #   1. Delete the stale gist (we have gh perms because it's on our
      #      own gh account, same auth as the discovery that found it).
      #   2. Tear down the half-written CONFIG that pointed at the dead
      #      host (else resume on next start would loop into the same
      #      stale pair).
      #   3. exec into a fresh airc connect in HOST mode for the same
      #      room name. AIRC_NO_DISCOVERY=1 so we don't re-find the gist
      #      we just deleted (gh propagation lag).
      #
      # Only fires when ALL three are true:
      #   - We resolved a kind:room gist (resolved_room_name + _resolved_gist_id non-empty)
      #   - gh CLI is available (to delete the stale gist)
      #   - Pair handshake failed (TCP unreachable / timeout)
      # If any condition isn't met, fall through to the original die().
      if [ -n "$resolved_room_name" ] && [ -n "$_resolved_gist_id" ] \
         && command -v gh >/dev/null 2>&1; then
        echo ""
        echo "  ⚠  Host of #${resolved_room_name} unreachable — self-healing as new host..."
        echo "     (prior host's gist: $_resolved_gist_id)"

        # Jittered backoff before takeover. Without this, two tabs that
        # hit the same dead gist concurrently both delete + publish
        # within the same gh API window and you end up with two
        # competing gists for the same room name (split-brain race —
        # caught only by running two tabs against a stale gist
        # simultaneously, NOT by the integration test).
        _self_heal_stale_host "$_resolved_gist_id"
      fi
      # Either not a room flow, or no gh, or no resolved_room_name → original die.
      # Surface the captured pair-handshake stderr (continuum-b69f 2026-04-27:
      # Windows users got "Can't reach ..." with no clue the real cause was
      # a Microsoft Store python3.exe stub returning exit 49). Per the
      # global "never swallow errors" rule — evidence is for the debugger,
      # not the trash. The handshake captured stderr+stdout via 2>&1 into
      # $response just above, so we have the real error in hand.
      if [ -n "${response:-}" ]; then
        echo "" >&2
        echo "  Pair handshake output (captured stderr/stdout):" >&2
        printf '%s\n' "$response" | sed 's/^/    /' >&2
        echo "" >&2
      fi
      die "Can't reach $peer_host_only:$peer_port. Is the host running 'airc connect'?"
    fi

    # Authorize host's SSH pubkey (for the joiner->host auth direction).
    # NOTE: the handshake's ssh_pub is airc's USER identity key — not the
    # sshd server host key used for known_hosts verification. Proper
    # host-key handling relies on ssh's own accept-new mode, plus a
    # targeted ssh-keygen -R when a PRIOR real-sshd host key in known_hosts
    # is known stale (e.g. the server rotated sshd host keys).
    local host_ssh_pub
    host_ssh_pub=$(printf '%s' "$response" | "$AIRC_PYTHON" -m airc_core.handshake get_field ssh_pub "" 2>/dev/null || true)
    if [ -n "$host_ssh_pub" ]; then
      mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"
      grep -qF "$host_ssh_pub" "$HOME/.ssh/authorized_keys" 2>/dev/null || {
        echo "$host_ssh_pub" >> "$HOME/.ssh/authorized_keys"
        chmod 600 "$HOME/.ssh/authorized_keys"
      }
    fi
    # Clear any stale sshd host key for this address before first SSH.
    # Cheap insurance against "REMOTE HOST IDENTIFICATION HAS CHANGED"
    # when the target was a different sshd host some time ago.
    local host_addr="${ssh_target##*@}"
    touch "$HOME/.ssh/known_hosts" 2>/dev/null && chmod 600 "$HOME/.ssh/known_hosts" 2>/dev/null
    ssh-keygen -R "$host_addr" -f "$HOME/.ssh/known_hosts" >/dev/null 2>&1 || true

    # Save host as a peer (with their airc_home so wire paths are correct).
    # Drop any existing peer records with the same host first — stale names
    # from a prior rename chain must not linger alongside the current one.
    local host_airc_home host_x25519_pub
    host_airc_home=$(printf '%s' "$response" | "$AIRC_PYTHON" -m airc_core.handshake get_field airc_home "" 2>/dev/null || true)
    # Phase E.2: capture host's X25519 pubkey from handshake response
    # so cmd_send can encrypt envelopes destined for this peer.
    host_x25519_pub=$(printf '%s' "$response" | "$AIRC_PYTHON" -m airc_core.handshake get_field x25519_pub "" 2>/dev/null || true)
    HOST_X25519_PUB="$host_x25519_pub" "$AIRC_PYTHON" -c "
import json, os
peers_dir = os.path.expanduser('$PEERS_DIR')
os.makedirs(peers_dir, exist_ok=True)
peer_name = '$peer_name'
ssh_target = '$ssh_target'
if os.path.isdir(peers_dir):
    for entry in os.listdir(peers_dir):
        if not entry.endswith('.json'): continue
        if entry == peer_name + '.json': continue
        try:
            d = json.load(open(os.path.join(peers_dir, entry)))
        except Exception:
            continue
        if d.get('host') == ssh_target:
            for ext in ('.json', '.pub'):
                p = os.path.join(peers_dir, entry[:-5] + ext)
                if os.path.isfile(p):
                    try: os.remove(p)
                    except Exception: pass
record = {
    'name': peer_name,
    'host': ssh_target,
    'airc_home': '$host_airc_home',
    'paired': '$(timestamp)'
}
host_x = os.environ.get('HOST_X25519_PUB', '')
if host_x:
    record['x25519_pub'] = host_x
with open(os.path.join(peers_dir, peer_name + '.json'), 'w') as f:
    json.dump(record, f, indent=2)
" 2>/dev/null || true

    # If we resolved this pair via gist discovery (vs. inline-invite),
    # persist the gist id so resume-time freshness checks can detect a
    # gist-deletion / replacement before re-pairing against a stale host
    # (issue #83). Cleared by cmd_part on graceful leave.
    if [ -n "$_resolved_gist_id" ]; then
      echo "$_resolved_gist_id" > "$AIRC_WRITE_DIR/room_gist_id"
      # #283: also map this channel→gist in channel_gists so the
      # multi-channel monitor polls it and cmd_send routes by channel.
      if [ -n "$resolved_room_name" ]; then
        "$AIRC_PYTHON" -m airc_core.config set_channel_gist \
          --config "$CONFIG" --channel "$resolved_room_name" --gist-id "$_resolved_gist_id" 2>/dev/null || true
      fi
    fi

    # Persist host details in own config so `airc invite` can reconstruct
    # the join string for onward sharing without a fresh handshake. Also
    # cache the host's identity blob from the handshake response so
    # `airc whois <host-name>` works locally (issue #34 v2).
    local host_identity_json; host_identity_json=$(printf '%s' "$response" | "$AIRC_PYTHON" -m airc_core.handshake get_field identity "{}" 2>/dev/null || echo "{}")
    [ -z "$host_identity_json" ] && host_identity_json="{}"
    # Pass values as env vars instead of bash-substituted into the
    # python heredoc body. continuum-b69f's PR #164 retest 2026-04-27
    # found host_airc_home / host_name / host_port / host_ssh_pub /
    # host_identity all silently unwritten on Win→Mac join: if ANY of
    # the bash substitutions broke the python source (newline in
    # host_ssh_pub, weird char in host_airc_home, peer_port empty/
    # non-numeric, etc.), the whole heredoc errored out via
    # `2>/dev/null || true` and zero fields landed in config. Switch
    # to env-var pass — python reads from os.environ; bash never
    # touches the python source. Also emit stderr to surface failures
    # for the future debugger (not /dev/null).
    "$AIRC_PYTHON" -m airc_core.config set_host_block \
        --config "$CONFIG" \
        --host-airc-home "$host_airc_home" \
        --host-name "$peer_name" \
        --host-port "${peer_port:-7547}" \
        --host-ssh-pub "$host_ssh_pub" \
        --host-identity-json "$host_identity_json" \
        || echo "  ⚠  config write failed (host_airc_home/host_name/host_port/host_ssh_pub may be unset). airc may still work if subsequent retries refresh." >&2

    # Pick up reminder setting from host
    local host_reminder
    host_reminder=$(printf '%s' "$response" | "$AIRC_PYTHON" -m airc_core.handshake get_field reminder 300 2>/dev/null || echo "300")
    if [ "$host_reminder" -gt 0 ] 2>/dev/null; then
      echo "$host_reminder" > "$AIRC_WRITE_DIR/reminder"
      date +%s > "$AIRC_WRITE_DIR/last_sent"
    fi

    # Verify SSH works
    if relay_ssh "$ssh_target" "echo ok" 2>/dev/null; then
      echo "  Connected to '$peer_name' (SSH verified, reminder: ${host_reminder}s)"
    else
      echo "  Connected to '$peer_name' (SSH not verified — messages may need retry)"
    fi

    # Write PID file so `airc teardown` can find us later.
    echo $$ > "$AIRC_WRITE_DIR/airc.pid"
    # Clean exit on tab close / signal: reap the ssh tail subprocess so the
    # remote doesn't see an orphaned session and the port doesn't linger.
    trap '
      rm -f "$AIRC_WRITE_DIR/airc.pid" 2>/dev/null
      for p in $(proc_children $$); do kill $p 2>/dev/null; done
    ' EXIT INT TERM

    spawn_general_sidecar_if_wanted
    echo "  Monitoring for messages..."
    monitor

  else
    # ── HOST MODE ─────────────────────────────────────────────────
    local name="${target:-}"
    [ -z "$name" ] && name=$(resolve_name)

    init_identity "$name"

    # Merge into existing config.json (preserve identity across re-spawns
    # — same rationale as the joiner branch above).
    set_config_val name    "$name"
    set_config_val host    "$(get_host)"
    set_config_val created "$(timestamp)"
    # Host mode: clear leftover host_* from any prior joiner run in
    # this scope so we don't mis-read ourselves as a joiner.
    unset_config_keys host_target host_name host_port host_airc_home host_ssh_pub host_identity

    local host; host=$(get_host)
    local user; user=$(whoami)
    local ssh_pubkey_b64; ssh_pubkey_b64=$(base64 < "$IDENTITY_DIR/ssh_key.pub" | tr -d '\n')
    # Port selection: start at AIRC_PORT (or 7547) and walk up if already
    # taken. Happens on machines with stale/zombie airc hosts or multiple
    # concurrent scopes. Users don't need to pick a port manually.
    local host_port="${AIRC_PORT:-7547}"
    local original_port="$host_port"
    local tried=0
    while [ -n "$(port_listeners "$host_port")" ]; do
      host_port=$((host_port + 1))
      tried=$((tried + 1))
      if [ "$tried" -ge 20 ]; then
        die "No free port in range ${original_port}-$((original_port + 20)). Close other airc hosts or set AIRC_PORT explicitly."
      fi
    done
    # Only include :port in the join string when non-default, keeping strings compact.
    local port_suffix=""
    [ "$host_port" != "7547" ] && port_suffix=":$host_port"

    # Persist the actual listen port so `airc invite` can reconstruct the
    # join string later without needing to parse the startup banner.
    echo "$host_port" > "$AIRC_WRITE_DIR/host_port"

    # Set reminder interval from host
    if [ "$reminder_interval" -gt 0 ] 2>/dev/null; then
      echo "$reminder_interval" > "$AIRC_WRITE_DIR/reminder"
      date +%s > "$AIRC_WRITE_DIR/last_sent"
    fi

    echo ""
    [ "$host_port" != "$original_port" ] && echo "  Port $original_port was taken; using $host_port."
    echo "  Hosting as '$name' (reminder: ${reminder_interval}s)"
    echo ""
    local _invite_long="${name}@${user}@${host}${port_suffix}#${ssh_pubkey_b64}"
    # When --gist is requested AND succeeds, the short gist ID becomes
    # the primary handoff and the long invite is demoted to a footnote
    # ("if the gist channel fails, fall back to this"). When --gist is
    # NOT requested, we print the long invite as the primary as today.
    local _printed_long=0
    if [ "$use_gist" != "1" ]; then
      echo "  On the other machine:"
      echo "    airc connect $_invite_long"
      _printed_long=1
    fi

    # Record room name + print substrate banner BEFORE the gist push
    # attempt so cmd_part / status / diagnostics know the channel name
    # even when the gist push is skipped (--no-gist) or fails (gh
    # missing/unauthed). The gist_id is recorded only when an actual
    # gist is created (see below). The "Hosting #<name>" banner is the
    # signal both humans and the integration test use to confirm
    # substrate framing took effect — emit unconditionally for room mode.
    if [ "$use_room" = "1" ]; then
      echo "$room_name" > "$AIRC_WRITE_DIR/room_name"
      # Phase 2B.2: also seed subscribed_channels with our hosted channel
      # so cmd_send + future config-driven consumers see it.
      "$AIRC_PYTHON" -m airc_core.config subscribe \
        --config "$CONFIG" --channel "$room_name" --first 2>/dev/null || true
      echo "  Hosting #${room_name} — no existing room on your gh account, fresh start."
      echo "  Other agents on your gh account who run 'airc join' will auto-join."
    fi

    # ── Gist transport (--gist flag, issue #37) ────────────────────
    # Push the long invite to a secret gist + print the short ID. The
    # short ID is robust across chat clients (sms, slack, paste-buffer
    # cross-machine) where the 200-char base64 invite gets line-wrapped
    # or auto-formatted into uselessness. It's also a coordination
    # layer for cross-tailnet pairing where the two peers don't share
    # a VPN initially — the gist is the shared rendezvous point.
    #
    # Payload is a versioned JSON envelope, NOT a raw invite string.
    # Same shape as image file headers: magic + version + typed body.
    # `airc: 1` marks it as ours; `kind` is the dispatch field for
    # future connection kinds (cross-tailnet relay, bootstrap-tailnet,
    # webrtc-mesh, etc.). Receiver reads kind → calls the matching
    # handler; new kinds added without breaking old peers because the
    # version field gates compat.
    if [ "$use_gist" = "1" ]; then
      if ! command -v gh >/dev/null 2>&1; then
        echo ""
        echo "  ⚠  --gist requested but 'gh' CLI not installed."
        echo "     Install: https://cli.github.com  (or: brew install gh)"
        echo "     Skipping gist push; long invite above is the only handoff."
      else
        # Bootstrap basename + description match channel_gist.create_new's
        # canonical shape (airc-room-<channel>.json + "airc room: #X").
        # Pre-fix the host path used a random mktemp basename
        # (airc-invite.XXXXXX) and "airc mesh" description, then
        # heartbeat (and channel_gist.find_existing on subsequent peers)
        # tried to find/edit `airc-room-X.json` which didn't exist —
        # heartbeat 'gh gist edit' silently failed → false eviction
        # loop → gist deleted mid-conversation. Issue #301.
        local _gist_tmpdir; _gist_tmpdir=$(mktemp -d -t airc-bootstrap.XXXXXX)
        local _gist_tmp="$_gist_tmpdir/airc-room-${room_name}.json"
        if [ "$use_room" != "1" ]; then
          # Legacy single-pair invite mode keeps the old basename — it's
          # short-lived (deleted post-pair).
          _gist_tmp="$_gist_tmpdir/airc-invite.json"
        fi
        local _now; _now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        local _gist_kind="invite"
        local _gist_desc="airc invite for $name (delete after pair)"
        local _gist_payload=""

        if [ "$use_room" = "1" ]; then
          # Mesh-singleton discovery (joiner _mesh_find looks for this
          # description literal). Filename is canonical airc-room-<channel>.json
          # so heartbeat's gh-edit basename match works (#297).
          # Migrating fully to per-channel gist shape is a follow-up
          # (#301 doc note); changing description here would break
          # the joiner's _mesh_find call without a paired update.
          _gist_kind="mesh"
          _gist_desc="$(_mesh_desc)"
          # last_heartbeat: host's presence signal, refreshed every
          # AIRC_HEARTBEAT_SEC (default 30s) by the bg loop spawned
          # below. Joiners detect stale → take over deterministically.
          #
          # machine_id + host.addresses[]: multi-address redundancy.
          # Same machine, two tabs → joiner sees machine_id match,
          # uses 127.0.0.1 regardless of network state. Same LAN →
          # joiner picks the LAN entry. Tailscale → joiner picks
          # tailscale ONLY when nothing closer works AND the host is
          # actually signed in (host_address_set drops tailscale from
          # the list when not authed). Tailscale becomes truly
          # optional: if it's down or you're logged out, the gist's
          # localhost+LAN entries still let same-machine and
          # same-LAN peers connect.
          local _addrs_json; _addrs_json=$(host_addresses_json "$host_port")
          local _machine_id; _machine_id=$(host_machine_id)
          _gist_payload=$(cat <<JSON
{
  "airc": 1,
  "kind": "mesh",
  "channels": ["${room_name}"],
  "invite": "$_invite_long",
  "host": {
    "name": "$name",
    "user": "$user",
    "machine_id": "${_machine_id}",
    "address": "$host",
    "port": $host_port,
    "addresses": ${_addrs_json}
  },
  "created": "$_now",
  "updated": "$_now",
  "last_heartbeat": "$_now"
}
JSON
)
        else
          # Single-pair invite (--no-general / legacy). Same envelope
          # shape as before — host deletes the gist after the joiner
          # pairs successfully.
          _gist_payload=$(cat <<JSON
{
  "airc": 1,
  "kind": "invite",
  "invite": "$_invite_long",
  "host": {
    "name": "$name",
    "user": "$user",
    "address": "$host",
    "port": $host_port
  },
  "created": "$_now"
}
JSON
)
        fi

        printf '%s\n' "$_gist_payload" > "$_gist_tmp"
        # Secret gist: URL-only-discoverable, not searchable. The gist
        # ID itself is the secret. Same threat model as the long invite:
        # whoever holds the string can pair. Room gists persist; invite
        # gists should be deleted by the host after the first joiner.
        local _gist_url; _gist_url=$(gh gist create -d "$_gist_desc" "$_gist_tmp" 2>/dev/null | tail -1)
        rm -rf "$_gist_tmpdir"
        if [ -n "$_gist_url" ]; then
          local _gist_id="${_gist_url##*/}"
          local _hh; _hh=$(humanhash "$_gist_id" 2>/dev/null)
          # Persist the gist id locally so cmd_part can manage the
          # mesh gist on graceful host exit (mesh/room mode only —
          # invite mode is one-shot and the joiner-pair flow already
          # prompts cleanup).
          if [ "$_gist_kind" = "mesh" ] || [ "$_gist_kind" = "room" ]; then
            echo "$_gist_id" > "$AIRC_WRITE_DIR/room_gist_id"
            echo "$room_name" > "$AIRC_WRITE_DIR/room_name"
            # #283: also map this channel→gist in channel_gists so
            # the multi-channel monitor polls it and cmd_send routes
            # by channel.
            "$AIRC_PYTHON" -m airc_core.config set_channel_gist \
              --config "$CONFIG" --channel "$room_name" --gist-id "$_gist_id" 2>/dev/null || true

            # Heartbeat loop: keep last_heartbeat fresh in the gist so
            # joiners can deterministically detect a dead host. Without
            # this, a host that dies ungracefully (sleep, kill -9, OOM,
            # crashed bash) leaves a gist pointing at a corpse forever.
            # Every messy state cascade today (memento, my own
            # bash-bg-and-die orphan, the manual gist-delete I had to
            # run by hand) traces to this missing presence signal.
            #
            # Loop runs every AIRC_HEARTBEAT_SEC (default 30s) and dies
            # automatically when its parent (the host airc connect bash)
            # exits — so kill -9 on the host stops heartbeats within one
            # interval. Joiners treat last_heartbeat older than
            # AIRC_HEARTBEAT_STALE (default 90s = 3 missed beats) as
            # stale and self-heal as new host.
            local _heartbeat_sec="${AIRC_HEARTBEAT_SEC:-30}"
            local _hb_parent_pid=$$
            local _hb_invite="$_invite_long"
            local _hb_name="$name"
            local _hb_user="$user"
            local _hb_host="$host"
            local _hb_port="$host_port"
            local _hb_room="$room_name"
            local _hb_created="$_now"
            local _hb_machine_id="$_machine_id"
            local _hb_messages="$MESSAGES"
            local _hb_stderr="$AIRC_WRITE_DIR/heartbeat.stderr"
            local _hb_state_dir="$AIRC_WRITE_DIR"
            (
              # Detach from job control so a parent SIGINT kills the
              # whole tree but normal exit lets us race the trap to
              # delete the gist first.
              local _consec_fail=0
              local _max_consec_fail="${AIRC_HB_MAX_FAIL:-3}"
              while sleep "$_heartbeat_sec"; do
                # Parent died (PID gone) → exit. This is the kill -9
                # / OOM / sleep recovery path.
                if ! kill -0 "$_hb_parent_pid" 2>/dev/null; then
                  exit 0
                fi
                local _hb_now; _hb_now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
                # Refresh addresses each tick. Captures network changes
                # mid-session: laptop moves to a different LAN, Tailscale
                # comes up / goes down / re-auths, interface flapping.
                # The next gist write reflects current reachability;
                # joiners that lose connection re-discover and try the
                # new address set.
                local _hb_addrs; _hb_addrs=$(host_addresses_json "${_hb_port}")
                # Phase 2C: build channels[] from recent message activity
                # so joiners on different cwds can advertise their channels
                # without coordinating with the host. Self-correcting —
                # silent channels age out, active ones surface. Falls back
                # to the host's primary room if no recent activity.
                local _hb_channels
                _hb_channels=$(AIRC_HB_MSGS="$_hb_messages" \
                               AIRC_HB_ROOM="$_hb_room" \
                               "$AIRC_PYTHON" -c '
import json, os, sys
log = os.environ.get("AIRC_HB_MSGS", "")
fallback = os.environ.get("AIRC_HB_ROOM", "general") or "general"
window = int(os.environ.get("AIRC_HB_RECENT", "200"))
chans = []
seen = set()
try:
    with open(log) as f:
        # Read last N lines without slurping the full file.
        lines = f.readlines()[-window:]
    for line in lines:
        try:
            ch = json.loads(line).get("channel", "")
        except Exception:
            continue
        if ch and ch not in seen:
            seen.add(ch); chans.append(ch)
except Exception:
    pass
if not chans:
    chans = [fallback]
elif fallback not in seen:
    chans.append(fallback)
print(json.dumps(chans))
' 2>/dev/null || echo "[\"${_hb_room}\"]")
                local _hb_payload; _hb_payload=$(cat <<JSON
{
  "airc": 1,
  "kind": "mesh",
  "channels": ${_hb_channels},
  "invite": "${_hb_invite}",
  "host": {
    "name": "${_hb_name}",
    "user": "${_hb_user}",
    "machine_id": "${_hb_machine_id}",
    "address": "${_hb_host}",
    "port": ${_hb_port},
    "addresses": ${_hb_addrs}
  },
  "created": "${_hb_created}",
  "updated": "${_hb_now}",
  "last_heartbeat": "${_hb_now}"
}
JSON
)
                # Heartbeat target file basename MUST match the canonical
                # in-gist filename (`airc-room-<channel>.json` per
                # channel_gist.py). When the gist has multiple files
                # (messages.jsonl + the room-metadata JSON) and we pass
                # gh a path with a basename that matches NEITHER, gh
                # errors with "unsure what file to edit; either specify
                # --filename or run interactively" — heartbeat fails N
                # times in a row and the host self-evicts (deletes its
                # own gist + respawns) when nothing was actually wrong.
                # That eviction loop is the surface ideem-local-4bef
                # root-caused 2026-04-29; it's also what nuked the
                # #useideem gist mid-ping-debug. Ensuring the temp
                # basename matches the canonical filename closes the
                # whole convergent class.
                local _hb_tmpdir; _hb_tmpdir=$(mktemp -d -t airc-hb.XXXXXX)
                local _hb_tmp="${_hb_tmpdir}/airc-room-${_hb_room}.json"
                printf '%s\n' "$_hb_payload" > "$_hb_tmp"
                # Rotate the host's messages.jsonl when it exceeds the
                # AIRC_LOG_MAX_LINES threshold (default 5000). Trims
                # in-place via airc_core.log; SSH-tail's -F flag detects
                # the atomic replace and re-opens. Joiners with offsets
                # past the new file's line count are caught by #245.
                # Cheap no-op when under threshold.
                "$AIRC_PYTHON" -m airc_core.log rotate --path "$_hb_messages" \
                  --max-lines "${AIRC_LOG_MAX_LINES:-5000}" \
                  --keep-lines "${AIRC_LOG_KEEP_LINES:-2500}" >/dev/null 2>&1 || true
                # Capture stderr to a state file (per never-swallow-errors
                # rule). Track consecutive failures: after N in a row,
                # detect active-host-evicted (#224) and self-heal — kill
                # the parent so the daemon (or user) respawns into a
                # fresh discovery + rejoin path.
                if gh gist edit "$_gist_id" "$_hb_tmp" >/dev/null 2>"$_hb_stderr"; then
                  _consec_fail=0
                else
                  _consec_fail=$((_consec_fail + 1))
                  if [ "$_consec_fail" -ge "$_max_consec_fail" ]; then
                    local _stderr_tail; _stderr_tail=$(tail -1 "$_hb_stderr" 2>/dev/null | tr -d '\n' | tr '"' "'")
                    local _evict_marker; _evict_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[HOST EVICTED] heartbeat to gist %s failed %d consecutive times — self-healing. last stderr: %s"}' \
                      "$_hb_now" "$_hb_room" "$_gist_id" "$_consec_fail" "${_stderr_tail:-<empty>}")
                    echo "$_evict_marker" >> "$_hb_messages" 2>/dev/null || true
                    # Drop the stale local-state files so the parent's
                    # next discovery re-elects via _mesh_find.
                    rm -f "$_hb_state_dir/host_gist_id" "$_hb_state_dir/room_gist_id" 2>/dev/null
                    # SIGTERM the parent — its EXIT trap will reap
                    # children + clean up. With daemon installed,
                    # launchd/systemd respawns; without daemon, the
                    # parent's reconnect loop catches the EXIT and the
                    # user gets a clean "host evicted" log line in
                    # messages.jsonl.
                    kill -TERM "$_hb_parent_pid" 2>/dev/null
                    exit 0
                  fi
                fi
                rm -rf "$_hb_tmpdir"
              done
            ) &
            local _hb_pid=$!
            # Stash heartbeat-loop PID + gist-id in scope-local files so
            # the canonical exit-trap (set later in cmd_connect, around
            # line 2498) can reap them. We don't set our own EXIT trap
            # here because bash traps are last-set-wins per shell — the
            # later trap would clobber us, leaving the gist orphaned on
            # graceful Ctrl-C. Instead, the canonical trap reads these
            # state files and cleans everything up in one place.
            echo "$_hb_pid"  >  "$AIRC_WRITE_DIR/heartbeat.pid"
            echo "$_gist_id" >  "$AIRC_WRITE_DIR/host_gist_id"

            # Post-publish race-loser detection via _mesh_take_over.
            # Two tabs that ran `airc join` simultaneously can BOTH see
            # empty mesh-gist listing (gh propagation lag) and BOTH
            # publish. Pre-publish recheck doesn't help — neither
            # gist is globally visible yet at this point. _mesh_take_over
            # waits a jitter, lists all "airc mesh" gists, picks the
            # OLDEST by created_at as winner, and reports whether we won
            # or lost. Loser deletes its gist + re-execs as joiner.
            local _race; _race=$(_mesh_take_over "" "$_gist_id")
            case "$_race" in
              winner|"")
                : # we won (or _mesh_take_over couldn't probe — assume winner, heartbeat will sort it)
                ;;
              loser:*)
                local _winner_id="${_race#loser:}"
                echo ""
                echo "  ⚠  Concurrent host detected — yielding to winner ($_winner_id)."
                # Stop our heartbeat, delete our gist, clear state, re-exec as joiner.
                kill "$_hb_pid" 2>/dev/null || true
                gh gist delete "$_gist_id" --yes >/dev/null 2>&1 || true
                rm -f "$AIRC_WRITE_DIR/heartbeat.pid" \
                      "$AIRC_WRITE_DIR/host_gist_id" \
                      "$AIRC_WRITE_DIR/room_gist_id" \
                      "$AIRC_WRITE_DIR/room_name"
                _reexec_into rejoin "$_winner_id"
                ;;
            esac

            echo "  Hosting #${room_name} (gh-account substrate)."
            echo "  Other agents on your gh account auto-join via:  airc connect"
            echo "  Cross-account share (rare):"
            echo "    airc connect $_gist_id"
            [ -n "$_hh" ] && echo "      # mnemonic: $_hh"
            echo "    airc connect $_invite_long"
            echo ""
            echo "  (Room gist: $_gist_url — persistent; deleted on 'airc part'.)"
          else
            echo "  On the other machine (pick whichever is easiest to share):"
            echo ""
            echo "    airc connect $_gist_id"
            [ -n "$_hh" ] && echo "      # mnemonic: $_hh"
            echo "    airc connect $_invite_long"
            echo ""
            echo "  (Gist: $_gist_url — secret, single-use; delete after pairing.)"
          fi
        else
          echo ""
          echo "  ⚠  Gist push failed (gh auth?). Falling back to long invite:"
          if [ "$_printed_long" = "0" ]; then
            echo "    airc connect $_invite_long"
          fi
        fi
      fi
    fi
    echo ""
    echo "  Waiting for peers on port $host_port..."
    # Background: accept peer registrations via TCP (public keys only).
    #
    # Parent-watch (#132): the loop exits when its own parent disappears
    # (PPID=1 = reparented to init = airc parent bash died). Without
    # this, the loop survives terminal close / Monitor tool teardown /
    # kill of the parent, keeps spawning fresh python listeners, and
    # every joiner that hits the cached port gets a real-looking pair
    # handshake against a ghost host. Pair-listener Python has its own
    # 1s parent-watch thread (see airc_core.handshake._start_parent_watch)
    # to catch the in-flight-handshake case; this loop check covers the
    # between-iterations case before the next python is spawned.
    _orphan_parent_pid=$$
    (
      # Loop while the airc parent bash is still alive. kill -0 is the
      # cheapest "is PID still running" probe (no signal sent, just an
      # error if the process is gone). When the parent dies, this exits
      # before the next iteration so no fresh python is spawned.
      #
      # --watch-pid hands the same PID to the python listener, which
      # spawns a 1s polling thread that os._exit()s mid-accept the
      # moment the parent dies — covering the in-flight handshake
      # case that the bash between-iterations check can't see.
      while kill -0 "$_orphan_parent_pid" 2>/dev/null; do
        "$AIRC_PYTHON" -m airc_core.handshake accept_one \
          --host-port "$host_port" \
          --peers-dir "$PEERS_DIR" \
          --identity-dir "$IDENTITY_DIR" \
          --config "$CONFIG" \
          --host-name "$name" \
          --reminder-interval "$reminder_interval" \
          --airc-home "$AIRC_WRITE_DIR" \
          --messages "$MESSAGES" \
          --watch-pid "$_orphan_parent_pid" 2>/dev/null || true
      done
    ) &
    PAIR_PID=$!

    # Write PID file so `airc teardown` can find us later. Record us, the
    # PAIR_PID (TCP-accept loop), and the heartbeat-loop PID (if hosting a
    # room with a gist) so teardown can reap all three.
    _hb_pid_persisted=""
    [ -f "$AIRC_WRITE_DIR/heartbeat.pid" ] && _hb_pid_persisted=$(cat "$AIRC_WRITE_DIR/heartbeat.pid" 2>/dev/null)
    echo "$$ $PAIR_PID $_hb_pid_persisted" > "$AIRC_WRITE_DIR/airc.pid"
    # Clean exit on tab close (SIGTERM/SIGINT from Claude Code's Monitor tool
    # going away, or any other signal): reap the accept loop, its python
    # listener, the heartbeat loop, AND delete our hosted gist if any —
    # don't leave orphans holding the port, the SSH session, or a stale
    # gist pointing at a corpse. Single canonical trap (was previously
    # split between this site + the gist-publish site, but bash traps are
    # last-set-wins per shell so the split lost the gist-cleanup half).
    trap '
      _exit_hb_pid=""
      _exit_gist_id=""
      [ -f "$AIRC_WRITE_DIR/heartbeat.pid" ] && _exit_hb_pid=$(cat "$AIRC_WRITE_DIR/heartbeat.pid" 2>/dev/null)
      [ -f "$AIRC_WRITE_DIR/host_gist_id" ] && _exit_gist_id=$(cat "$AIRC_WRITE_DIR/host_gist_id" 2>/dev/null)
      [ -n "$_exit_hb_pid" ] && kill $_exit_hb_pid 2>/dev/null
      if [ -n "$_exit_gist_id" ] && command -v gh >/dev/null 2>&1; then
        gh gist delete "$_exit_gist_id" --yes >/dev/null 2>&1
      fi
      rm -f "$AIRC_WRITE_DIR/airc.pid" "$AIRC_WRITE_DIR/heartbeat.pid" "$AIRC_WRITE_DIR/host_gist_id" 2>/dev/null
      for p in $PAIR_PID $(proc_children $PAIR_PID) $(proc_children $$); do
        kill $p 2>/dev/null
      done
    ' EXIT INT TERM

    spawn_general_sidecar_if_wanted
    echo "  Monitoring for messages..."
    monitor
    kill $PAIR_PID 2>/dev/null
  fi
}
