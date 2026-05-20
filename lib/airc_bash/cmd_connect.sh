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

# ── Cold-start phase telemetry ─────────────────────────────────────────
# Codex/Joel-spec'd UX follow-up to #545/#546: emit "→ [t+Ns] phase…"
# lines at the top of slow operations so a user staring at a Monitor
# task can tell "still working" from "hung." Without this, Windows
# Monitor cold-starts produced 30-60s of total silence followed by a
# flood of host-setup output — indistinguishable from a hang.
#
# Design: t0 is anchored via $AIRC_WRITE_DIR/.cold_start_t0 (a unix
# timestamp). The file is created on first phase emission and survives
# across `_reexec_into host` (same scope, no env passing required).
# The end-of-cold-start cleanup ("monitor stream attached") removes
# the marker so the next `airc join` against an already-warm scope
# starts from t0=0 again, not "minutes since the laptop's last boot."
#
# Output goes to BOTH stdout (so Monitor surfaces it as user-facing
# events) AND $AIRC_WRITE_DIR/airc-transport.log (so post-mortem
# traces show where the time went, even if the user closed the tab).
_join_phase() {
  local _t0_file="${AIRC_WRITE_DIR:-}/.cold_start_t0"
  local _now _t0 _elapsed
  _now=$(date +%s 2>/dev/null) || _now=0
  if [ -n "${AIRC_WRITE_DIR:-}" ] && [ ! -f "$_t0_file" ]; then
    mkdir -p "$AIRC_WRITE_DIR" 2>/dev/null || true
    printf '%s\n' "$_now" > "$_t0_file" 2>/dev/null || true
    _t0="$_now"
  elif [ -f "$_t0_file" ]; then
    _t0=$(cat "$_t0_file" 2>/dev/null || echo "$_now")
  else
    _t0="$_now"
  fi
  case "$_t0" in ''|*[!0-9]*) _t0="$_now" ;; esac
  _elapsed=$(( _now - _t0 ))
  [ "$_elapsed" -lt 0 ] 2>/dev/null && _elapsed=0
  printf '  → [t+%ds] %s\n' "$_elapsed" "$*"
  if [ -n "${AIRC_WRITE_DIR:-}" ] && [ -d "$AIRC_WRITE_DIR" ]; then
    printf '%s [t+%ds phase] %s\n' \
      "$(date -u +%FT%TZ 2>/dev/null || echo "?")" \
      "$_elapsed" "$*" \
      >> "$AIRC_WRITE_DIR/airc-transport.log" 2>/dev/null || true
  fi
}

# Clear the cold-start anchor — call once monitor stream is attached
# and the scope is in steady state. Without this, every subsequent
# `airc join` (which re-enters cmd_connect on each tab restart) would
# show "[t+86400s]" instead of fresh phase numbers.
_join_phase_done() {
  local _t0_file="${AIRC_WRITE_DIR:-}/.cold_start_t0"
  [ -f "$_t0_file" ] && rm -f "$_t0_file" 2>/dev/null || true
}

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
#      user's gh account (airc-rs channel-gist resolve
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
  local _first=0
  [ "$mode" = "--first" ] && _first=1
  if ! airc_config_subscribe "$channel" "$CONFIG" "$_first" 2>"$_err"; then
    echo "  ⚠ Could not subscribe to #${channel}:" >&2
    [ -s "$_err" ] && sed 's/^/      /' "$_err" >&2
    return 1
  fi

  # 2. Resolve-or-create the canonical gist on this gh account. If this
  # scope already knows the channel→gist mapping, trust that first: a
  # daemon restart must not block on GitHub discovery just to re-subscribe
  # to a room that is already in config.
  local _gid=""
  # For the primary hosted room, the room marker is stronger local truth
  # than channel_gists. A poisoned/stale channel_gists entry used to make
  # a bounce create a third duplicate even though room_gist_id still
  # pointed at the prior successful room. Prefer the durable room marker
  # first; then fall back to channel_gists; then finally ask GitHub.
  if [ -f "$AIRC_WRITE_DIR/room_name" ] && [ -f "$AIRC_WRITE_DIR/room_gist_id" ]; then
    local _marker_room _marker_gid
    _marker_room=$(cat "$AIRC_WRITE_DIR/room_name" 2>/dev/null || true)
    _marker_gid=$(cat "$AIRC_WRITE_DIR/room_gist_id" 2>/dev/null || true)
    if [ "$_marker_room" = "$channel" ] && printf '%s' "$_marker_gid" | grep -qE '^[0-9a-f]{32}$'; then
      _gid="$_marker_gid"
    fi
  fi
  if [ -z "$_gid" ]; then
    _gid=$(airc_config_get_channel_gist "$channel" "$CONFIG" || true)
  fi
  if [ -n "$_gid" ] && [ "${AIRC_NO_DISCOVERY:-0}" = "1" ] && [ ! -f "$AIRC_WRITE_DIR/room_gist_id" ]; then
    # AIRC_NO_DISCOVERY is a host-election guard, not permission to
    # believe poisoned routing state. If this scope has no durable
    # room marker left and only channel_gists claims a target, re-run
    # the canonical resolver before hosting. Otherwise a stale/bogus
    # channel_gists entry creates a fresh duplicate room on every
    # bounce, which is exactly the split-brain failure join must heal.
    _gid=""
  fi
  if [ -z "$_gid" ]; then
    _gid=$("$(airc_rs_bin)" channel-gist resolve \
           --channel "$channel" --create-if-missing 2>"$_err")
  fi
  if [ -z "$_gid" ]; then
    echo "  ⚠ Could not resolve gist for #${channel}:" >&2
    [ -s "$_err" ] && sed 's/^/      /' "$_err" >&2
    return 1
  fi

  # 3. Persist channel→gist mapping for cmd_send + monitor routing.
  if ! airc_config_set_channel_gist "$channel" "$_gid" "$CONFIG" 2>"$_err"; then
    echo "  ⚠ Could not persist channel→gist mapping for #${channel}:" >&2
    [ -s "$_err" ] && sed 's/^/      /' "$_err" >&2
    return 1
  fi

  printf '%s\n' "$_gid"
  return 0
}

_join_show_status_and_inbox() {
  echo ""
  echo "  Status"
  echo "  ------"
  cmd_status 2>&1 | sed 's/^/  /' || true
  echo ""
  echo "  Inbox"
  echo "  -----"
  cmd_inbox --count 50 2>&1 | sed 's/^/  /' || true
}

_join_transport_health_ok() {
  [ -f "$CONFIG" ] || return 1
  local _channels
  _channels=$("$(airc_rs_bin)" config read-channels --home "$AIRC_WRITE_DIR" --config "$CONFIG" 2>/dev/null || true)
  # Legacy/no-room mode has no gist bearer to heartbeat. If the caller
  # already proved the scope owner is alive, transport_health has no
  # channel rows to evaluate and must not force a duplicate restart.
  [ -n "$_channels" ] || return 0
  "$(airc_rs_bin)" transport health \
    --home "$AIRC_WRITE_DIR" \
    --config "$CONFIG" \
    --quiet \
    --fail >/dev/null 2>&1
}

_join_transport_in_startup_grace() {
  local health_out="${1:-}"
  local pidfile="${2:-$AIRC_WRITE_DIR/airc.pid}"
  [ -f "$pidfile" ] || return 1
  local mtime now age grace
  mtime=$(file_mtime "$pidfile" 2>/dev/null || echo 0)
  now=$(date +%s)
  case "$mtime" in ''|*[!0-9]*) return 1 ;; esac
  age=$((now - mtime))
  grace="${AIRC_STARTUP_GRACE_SEC:-45}"
  [ "$age" -ge 0 ] 2>/dev/null || return 1
  [ "$age" -le "$grace" ] 2>/dev/null || return 1
  printf '%s\n' "$health_out" | grep -q 'starting; no heartbeat yet' || return 1
  printf '%s\n' "$health_out" | grep -Eq 'stale heartbeat|stale bearer pid' && return 1
  return 0
}

_join_restart_scope_processes() {
  # This is a best-effort cleanup function. Pipefail + set -e applied
  # to its multi-source PID assembly is fatal: pgrep (via
  # proc_airc_pids_matching) returns 1 when no matches, the pipefail
  # pipeline propagates the 1, and the simple var assignment then
  # triggers set -e — killing the whole airc process partway through
  # cleanup. The fix is per-line `|| true` shielding around each
  # substitution that may legitimately exit non-zero on a clean tree
  # (no formatters, no bearer pidfiles, no transport pids).
  local _pids=""
  if [ -f "$AIRC_WRITE_DIR/airc.pid" ]; then
    _pids="$_pids $(cat "$AIRC_WRITE_DIR/airc.pid" 2>/dev/null | tr '\n' ' ' || true)"
  fi
  _pids="$_pids $(_airc_scope_monitor_formatter_pids "$AIRC_WRITE_DIR" 2>/dev/null | tr '\n' ' ' || true)"
  local _pidfile
  for _pidfile in "$AIRC_WRITE_DIR"/bearer_gist.*.pid; do
    [ -f "$_pidfile" ] || continue
    _pids="$_pids $(cat "$_pidfile" 2>/dev/null | awk '{print $1}' | tr '\n' ' ' || true)"
  done
  _pids="$_pids $(_join_scope_transport_pids 2>/dev/null | tr '\n' ' ' || true)"
  # Self-kill guard with cmdline verification. Two failure shapes
  # combine to make this function lethal-to-self otherwise:
  #
  #   (1) After `_reexec_into host` the new airc inherits the same
  #       PID as the pre-exec instance, so airc.pid (written by the
  #       pre-exec airc with $$) names US. Filtering $$/$PPID handles
  #       this — same defense already used by _join_scope_transport_pids
  #       at line 211-212.
  #
  #   (2) OS PID recycling. The other PID sources (airc.pid contents
  #       from a crashed predecessor, bearer_gist.*.pid stragglers)
  #       can name a slot the kernel has since reassigned to an
  #       unrelated process — including, on Windows WSL2, the parent
  #       wsl.exe / cmd.exe shell in our launcher chain. Killing one
  #       of those takes Claude Code's Monitor down with us. Joel hit
  #       this exact shape in #97 / #446 in a different code path; the
  #       fix there ("verify cmdline before treating PID as ours") was
  #       never propagated here. Without this guard, _join_restart_scope_processes
  #       was the last self-killer left in the join cold-start path.
  #
  # Filter rules:
  #   - drop non-numeric / empty entries (already a no-op)
  #   - drop $$ and $PPID (case 1)
  #   - drop any PID whose cmdline doesn't look like airc/airc-rs
  #     (case 2). Same regex shape as the stale-pidfile check at
  #     ~line 971 and cmd_teardown's parent-chain reaper.
  local _self_filtered_pids="" _candidate _candidate_cmd
  for _candidate in $_pids; do
    case "$_candidate" in
      ''|*[!0-9]*) continue ;;
    esac
    [ "$_candidate" = "$$" ] && continue
    [ "$_candidate" = "$PPID" ] && continue
    # Live PID? If the slot is empty the candidate is already gone —
    # nothing to kill, no risk of misidentification.
    kill -0 "$_candidate" 2>/dev/null || continue
    _candidate_cmd=$(proc_cmdline "$_candidate" 2>/dev/null || true)
    case "$_candidate_cmd" in
      *airc-rs*monitor*format*|*airc-rs*monitor*attach*|*airc-rs*handshake*|*airc-rs*bearer*recv*) ;;
      *airc[[:space:]]connect*|*airc[[:space:]]join*|*/airc[[:space:]]*) ;;
      *) continue ;;
    esac
    _self_filtered_pids="$_self_filtered_pids $_candidate"
  done
  _pids="$_self_filtered_pids"
  local _p _c
  for _p in $_pids; do
    case "$_p" in ''|*[!0-9]*) continue ;; esac
    kill "$_p" 2>/dev/null || true
    for _c in $(proc_children "$_p" 2>/dev/null); do
      kill "$_c" 2>/dev/null || true
    done
  done
  sleep 1
  for _p in $_pids; do
    case "$_p" in ''|*[!0-9]*) continue ;; esac
    kill -0 "$_p" 2>/dev/null || continue
    kill -9 "$_p" 2>/dev/null || true
    for _c in $(proc_children "$_p" 2>/dev/null); do
      kill -9 "$_c" 2>/dev/null || true
    done
  done
  rm -f "$AIRC_WRITE_DIR/airc.pid" "$AIRC_WRITE_DIR"/bearer_gist.*.pid 2>/dev/null || true
}

_join_scope_transport_pids() {
  # Scope-path catch-all for `airc join` self-heal. Pidfiles are not
  # enough after Monitor restarts: old bearer/formatter
  # children can be reparented to init and keep serving the same scope
  # while the new generation also runs. Match only transport/process
  # owners for THIS scope; leave UI-only attach streams alone.
  local _pids=""
  local _pid _cmd _scope_variant
  while IFS= read -r _scope_variant; do
    [ -n "$_scope_variant" ] || continue
    for _pid in $(proc_airc_pids_matching "$_scope_variant" 2>/dev/null | sort -un || true); do
      case "$_pid" in ''|*[!0-9]*) continue ;; esac
      [ "$_pid" = "$$" ] && continue
      [ "$_pid" = "$PPID" ] && continue
      _cmd=$(proc_cmdline "$_pid" || true)
      case "$_cmd" in
        *airc-rs*monitor*attach*) continue ;;
        *airc-rs*monitor*format*|*airc-rs*handshake*accept-one*|*airc-rs*bearer*recv*)
          _pids="$_pids $_pid"
          ;;
        *) continue ;;
      esac

      # Reap airc wrapper ancestors too. They often do not include
      # AIRC_HOME in argv because the scope is in the environment, but if
      # their Python children are ours, the wrapper is ours.
      local _ancestor _depth _ancestor_cmd
      _ancestor=$(proc_parent "$_pid" || true)
      _depth=0
      while [ -n "$_ancestor" ] && [ "$_ancestor" != "1" ] && [ "$_depth" -lt 6 ]; do
        _ancestor_cmd=$(proc_cmdline "$_ancestor" || true)
        if echo "$_ancestor_cmd" | grep -Eq '(^|[[:space:]])/[^[:space:]]*/airc[[:space:]]+(connect|join)([[:space:]]|$)|(^|[[:space:]])airc[[:space:]]+(connect|join)([[:space:]]|$)|eval .*airc[[:space:]]+(connect|join)'; then
          _pids="$_pids $_ancestor"
          _ancestor=$(proc_parent "$_ancestor" || true)
          _depth=$((_depth + 1))
        else
          break
        fi
      done
    done
  done <<EOF
$(_airc_scope_path_variants "$AIRC_WRITE_DIR")
EOF

  # Include direct children of everything we found. This catches short
  # wrapper trees without relying on one pidfile generation being current.
  local _base _child
  for _base in $_pids; do
    for _child in $(proc_children "$_base" 2>/dev/null); do
      _pids="$_pids $_child"
    done
  done
  for _pid in $_pids; do
    case "$_pid" in ''|*[!0-9]*) continue ;; esac
    [ "$_pid" = "$$" ] && continue
    [ "$_pid" = "$PPID" ] && continue
    printf '%s\n' "$_pid"
  done | sort -un
}

_join_scope_has_duplicate_transport() {
  local _fmt_count
  _fmt_count=$(_airc_scope_monitor_formatter_pids "$AIRC_WRITE_DIR" 2>/dev/null | wc -l | tr -d ' ')
  if [ "${_fmt_count:-0}" -gt 1 ] 2>/dev/null; then
    return 0
  fi

  local _seen_gists="" _pid _cmd _gid
  for _pid in $(proc_airc_pids_matching 'airc-rs.*bearer[[:space:]]+recv' 2>/dev/null | sort -un || true); do
    _cmd=$(proc_cmdline "$_pid" || true)
    _airc_cmdline_mentions_scope "$_cmd" "$AIRC_WRITE_DIR" || continue
    _gid=$(printf '%s\n' "$_cmd" | awk '{
      for (i = 1; i <= NF; i++) {
        if ($i == "--room-gist-id" && (i + 1) <= NF) {
          print $(i + 1); exit
        }
      }
    }')
    [ -n "$_gid" ] || continue
    case " $_seen_gists " in
      *" $_gid "*) return 0 ;;
      *) _seen_gists="$_seen_gists $_gid" ;;
    esac
  done

  return 1
}

_join_attach_local_stream() {
  echo ""
  echo "  Attaching this terminal to the local AIRC stream."
  echo "  Background AIRC owns transport; this process only displays new peer messages."
  local _client_id; _client_id=$(airc_client_id 2>/dev/null || true)
  local _tail_name; _tail_name=$(get_name 2>/dev/null || echo "airc")
  local _airc_rs; _airc_rs=$(airc_rs_bin 2>/dev/null || true)
  if [ -z "$_airc_rs" ]; then
    echo "airc: airc-rs is required for monitor attach" >&2
    return 127
  fi
  if [ -n "$_client_id" ]; then
    AIRC_CLIENT_ID="$_client_id" exec "$_airc_rs" --home "$AIRC_WRITE_DIR" monitor attach --my-name "$_tail_name"
  else
    exec "$_airc_rs" --home "$AIRC_WRITE_DIR" monitor attach --my-name "$_tail_name"
  fi
}

_join_emit_join_events() {
  local _name="$1"
  [ -z "$_name" ] && return 0
  [ -f "$CONFIG" ] || return 0
  local _channels _ch
  _channels=$(airc_config_read_channels "$CONFIG" || true)
  [ -z "$_channels" ] && return 0
  while IFS= read -r _ch; do
    [ -z "$_ch" ] && continue
    local _gid
    _gid=$(airc_config_get_channel_gist "$_ch" "$CONFIG" || true)
    [ -z "$_gid" ] && continue
    cmd_send --internal --system --channel "$_ch" "$_name joined #$_ch" >/dev/null 2>&1 || true
  done <<< "$_channels"
}

_join_spawn_transport_for_attach() {
  local _log="$AIRC_WRITE_DIR/airc-transport.log"
  mkdir -p "$AIRC_WRITE_DIR"
  echo ""
  echo "  Starting scope-local AIRC transport in the background."
  echo "  This terminal will attach to the local message stream."
  # Strip --attach / -attach from the forwarded argv. The child runs with
  # AIRC_NO_ATTACH=1 (set below), so the flag is redundant; worse, leaving
  # it in causes the child's parser to treat --attach as the positional
  # `target` whenever cmd_connect's flag-loop bails early — observed on
  # Windows + Claude Code Monitor where `airc status` then reports
  # `identity: --attach (host)`. The host name and gist label both inherit
  # that, breaking inbox routing. The child's own AIRC_NO_ATTACH=1
  # already prevents the recursion loop, so dropping the flag here is safe
  # in every code path.
  local _spawn_args=()
  local _arg
  for _arg in "$@"; do
    case "$_arg" in
      --attach|-attach) ;;
      *) _spawn_args+=("$_arg") ;;
    esac
  done
  # Detach the transport into its own session+pgroup so SIGHUP from the
  # launcher's session leader exit doesn't cascade. On Windows + Claude
  # Code Monitor (`wsl bash -lc 'airc join --attach'`) the launcher
  # bash is the controlling-terminal session leader; when it returns,
  # kernel SIGHUPs the entire pgroup. `setsid -f` forks the transport
  # into a new session AND a new pgroup AND disconnects it from the
  # controlling terminal — the kill-all-on-launcher-exit semantics no
  # longer apply.
  #
  # `setsid -f` returns immediately after fork (parent doesn't wait),
  # so the captured `$!` is the bash subshell PID; the actual transport
  # is the grandchild. We don't need that PID for the watchdog —
  # `_monitor_alive_with_bearer_fallback` reads airc.pid (written by
  # airc itself once spawned) which is the canonical aliveness signal.
  # The kill-0 fallback used `_transport_pid` to detect catastrophic
  # spawn failures; with setsid -f, the subshell exits cleanly after
  # forking the grandchild, so kill-0 of subshell-PID is no longer a
  # useful signal — the watchdog now relies entirely on airc.pid
  # appearing within the timeout. AIRC_NO_DETACH=1 forces the inline
  # form for harness tests that want process-tree reap semantics.
  if [ "${AIRC_NO_DETACH:-0}" != "1" ] && command -v setsid >/dev/null 2>&1; then
    setsid -f env AIRC_NO_ATTACH=1 AIRC_BACKGROUND_OK=1 \
      "$AIRC_SELF" join \
      ${_spawn_args[@]+"${_spawn_args[@]}"} \
      >>"$_log" 2>&1
  else
    (
      trap '' HUP
      AIRC_NO_ATTACH=1 AIRC_BACKGROUND_OK=1 exec "$AIRC_SELF" join \
        ${_spawn_args[@]+"${_spawn_args[@]}"}
    ) >>"$_log" 2>&1 &
  fi
  # With setsid -f, $! is the parent shell pid which exited cleanly
  # after forking the daemonized grandchild. The kill-0 watchdog can't
  # use it. Set _transport_pid="" to disable that path; the airc.pid
  # file written by the transport is the authoritative liveness check.
  local _transport_pid=""
  if [ "${AIRC_NO_DETACH:-0}" = "1" ] || ! command -v setsid >/dev/null 2>&1; then
    _transport_pid=$!
  fi
  if [ -n "$_transport_pid" ]; then
    echo "  transport PID: $_transport_pid"
  fi
  echo "  transport log: $_log"

  local _pidfile="$AIRC_WRITE_DIR/airc.pid"
  local _i
  for _i in $(seq 1 30); do
    if [ "$(_monitor_alive_with_bearer_fallback "$_pidfile")" = "yes" ]; then
      _join_show_status_and_inbox
      _join_attach_local_stream
      return 0
    fi
    if [ -n "$_transport_pid" ] && ! kill -0 "$_transport_pid" 2>/dev/null; then
      echo "  airc join: transport exited before it became healthy." >&2
      if [ -s "$_log" ]; then
        echo "  last transport log lines:" >&2
        tail -25 "$_log" | sed 's/^/    /' >&2
      fi
      return 1
    fi
    sleep 1
  done

  echo "  airc join: transport did not become healthy within 30s." >&2
  if [ -s "$_log" ]; then
    echo "  last transport log lines:" >&2
    tail -25 "$_log" | sed 's/^/    /' >&2
  fi
  return 1
}

_join_parent_chain_looks_like_claude_monitor() {
  local pid="$$" depth=0 parent="" cmd=""
  while [ -n "$pid" ] && [ "$pid" != "1" ] && [ "$depth" -lt 12 ]; do
    cmd=$(proc_cmdline "$pid" 2>/dev/null || true)
    if printf '%s\n' "$cmd" | grep -Eiq 'claude|anthropic'; then
      return 0
    fi
    parent=$(proc_parent "$pid" 2>/dev/null || true)
    [ -n "$parent" ] || break
    pid="$parent"
    depth=$((depth + 1))
  done
  return 1
}

cmd_connect() {
  local _orig_args=("$@")
  # Stale cold-start anchor cleanup. If a previous airc join crashed
  # before _join_phase_done could run, the .cold_start_t0 marker
  # would linger forever and `airc status` would show a perpetually-
  # rising "starting (t+86400s)". Clear stale (>10min) anchors at the
  # top of each new cmd_connect so a fresh phase clock starts cleanly.
  if [ -n "${AIRC_WRITE_DIR:-}" ] && [ -f "$AIRC_WRITE_DIR/.cold_start_t0" ]; then
    local _stale_t0; _stale_t0=$(cat "$AIRC_WRITE_DIR/.cold_start_t0" 2>/dev/null || echo 0)
    case "$_stale_t0" in ''|*[!0-9]*) _stale_t0=0 ;; esac
    local _stale_now; _stale_now=$(date +%s 2>/dev/null) || _stale_now=0
    if [ $((_stale_now - _stale_t0)) -gt 600 ] 2>/dev/null; then
      rm -f "$AIRC_WRITE_DIR/.cold_start_t0" 2>/dev/null || true
    fi
  fi
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
  local attach=0

  # AIRC_ROOM_INTENT: re-exec env var preserving the user's --room
  # across a stale-host-takeover exec. Pre-fix this was lost on every
  # self-heal: user typed `airc join --room qa-foo`, we exec'd back
  # into `airc join` with NO ARGS, auto-scope decided based on cwd
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
  # handshake fails because the host listed in the room gist is unreachable,
  # the joiner rewrites that same durable gist in host mode.
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
        echo "Usage: airc join [target] [flags]"
        echo "  airc join                      join or create the room for this scope"
        echo "  airc join <gist-id>            join via shared gist id"
        echo "  airc join <mnemonic>           join via humanhash phrase"
        echo "  airc join <invite-string>      join via inline invite"
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
      --takeover|-takeover)
        echo "  note: --takeover is no longer needed; stale hosts are recovered in-place." >&2
        shift ;;
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
      --attach|-attach)
        # UI attach mode: if a daemon/background airc process already
        # serves this scope, keep that single transport owner and attach
        # this terminal/Claude Monitor to the local messages log.
        attach=1; shift ;;
      -*)
        # Reject any unrecognized flag loudly. Pre-fix the catch-all
        # arm (now below) silently accepted `--anything` as a positional,
        # which downstream became `target` → host-mode `name` → config
        # `name`. Observed in the wild: `airc join --background` set
        # config.name to literal "--background", surfacing as
        # "Hosting as '--background'" and corrupting peer identity until
        # manually edited. See #511 (related: #521 belt for --attach).
        # Inline invites, gist ids, mnemonics never start with `-`, so
        # this rejector cannot eat a legitimate positional.
        echo "ERROR: unknown flag '$1'. See: airc join --help" >&2
        return 2 ;;
      *) positional+=("$1"); shift ;;
    esac
  done
  # Belt for the suspenders: even if the case arm above failed to match
  # `--attach` for a hidden-CR / NUL / encoding reason (only observed via
  # Claude Code Monitor on Windows + WSL2 — the foreground bash path
  # consumed it correctly), make sure it never lands in positional and
  # poisons `target`. Symptom we're guarding against: `airc status`
  # reporting `identity: --attach (host)` after the Monitor invocation,
  # config.json's name field persisted as `--attach`. See #511.
  if [ "${#positional[@]}" -gt 0 ]; then
    local _kept_positional=()
    local _p
    for _p in "${positional[@]}"; do
      case "$_p" in
        --attach|-attach) attach=1 ;;
        *) _kept_positional+=("$_p") ;;
      esac
    done
    positional=("${_kept_positional[@]+"${_kept_positional[@]}"}")
  fi
  set -- "${positional[@]+"${positional[@]}"}"
  [ "${AIRC_NO_ATTACH:-0}" = "1" ] && attach=0

  # Plain `airc join` is the public UX. If the parent chain is Claude
  # Code, treat it as UI attach mode so a Monitor invocation remains a
  # visible event stream when transport is already alive. Codex/non-
  # Monitor runtimes keep the quick-return behavior unless they
  # explicitly set AIRC_ATTACH=1.
  if [ "$attach" = "0" ] && [ "${AIRC_NO_ATTACH:-0}" != "1" ]; then
    if [ "${AIRC_ATTACH:-0}" = "1" ] || _join_parent_chain_looks_like_claude_monitor; then
      attach=1
    fi
  fi

  # One-shot marker used by child watchdogs to tell the parent "exit
  # with restart semantics", not "fatal crash". Clear stale markers
  # before this connect attempt starts.
  rm -f "$AIRC_WRITE_DIR/airc.restart-request" 2>/dev/null || true

  # Trust-existing-monitor short-circuit (#369, sandbox-aware via
  # _monitor_alive_with_bearer_fallback per #372). If a live airc
  # process is already in this scope, the user's intent ("airc join")
  # is satisfied — there's nothing to do, and the gh-auth probe below
  # would only generate noise (or false-positive failures from flaky
  # gh probes in environments like Codex's sandbox; #367) on a scope
  # that's already working.
  #
  # Pre-#372 this used naked kill -0 inline, which returned false on
  # Codex (sandbox process-tree blindness) even when the monitor pipeline
  # was alive. The shared helper checks for a scope-owned
  # monitor_formatter process as a fallback; it deliberately does NOT
  # accept fresh bearer_state as proof that this tab has a Monitor.
  local _early_pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ "$(_monitor_alive_with_bearer_fallback "$_early_pidfile")" = "yes" ]; then
    # 2026-05-02 QA caught (B5): if user passed --room NEWNAME and that
    # name is NOT in subscribed_channels yet, the user's intent is
    # "subscribe to a NEW room" — NOT "check if I'm already in mesh".
    # Pre-fix the short-circuit always returned 0, blocking multi-room
    # workflow. Now: if the requested room is fresh, fall through to
    # the subscribe path so it gets added.
    local _add_subscription=0
    if [ "$room_explicit" = "1" ] && [ -n "$room_name" ] && [ -f "$CONFIG" ]; then
      local _existing_subs; _existing_subs=$(airc_config_read_channels "$CONFIG" || true)
      if ! printf '%s\n' "$_existing_subs" | grep -qFx "$room_name"; then
        _add_subscription=1
      fi
    fi
    if [ "$_add_subscription" = "1" ]; then
      echo "  airc join: AIRC process already running; subscribing to additional room #${room_name}..."
      # Add #room_name to subscribed_channels + resolve its gist
      # (create if missing). The bearer for this channel will be
      # picked up on the next _monitor_multi_channel cycle (which
      # re-reads channel_map at top of each outer poll).
      airc_config_subscribe "$room_name" "$CONFIG" 0 2>/dev/null || true
      # Resolve --create-if-missing: returns the gist id (find existing
      # or create new gist named "airc room: #<channel>").
      local _new_gist; _new_gist=$("$(airc_rs_bin)" channel-gist resolve \
          --channel "$room_name" --create-if-missing 2>&1)
      if [ -n "$_new_gist" ] && printf '%s' "$_new_gist" | grep -qE '^[0-9a-f]{32}$'; then
        # Save the channel→gist mapping in config so cmd_send can route to it.
        airc_config_set_channel_gist "$room_name" "$_new_gist" "$CONFIG" 2>/dev/null || true
        echo "  ✓ Subscribed to #${room_name} (gist $_new_gist). Bearer respawn picks it up within ~30s."
      else
        echo "  ⚠ Subscribed to #${room_name} but gist resolve failed: $_new_gist"
        echo "  Bearer may not pick up new room until next cycle. Try: airc list to verify gist."
      fi
      _join_show_status_and_inbox
      [ "$attach" = "1" ] && _join_attach_local_stream
      return 0
    fi

    # A live monitor is not automatically a correct monitor. If this
    # scope is still mapped to a non-canonical duplicate gist, the
    # short-circuit would strand the tab on a solo island forever:
    # `airc join` says "already joined" even though discovery would
    # now converge on the durable room gist. Repair that locally by
    # stopping only this scope's recorded monitor PIDs, updating the
    # stale channel_gists entries, and falling through to normal
    # discovery. This is intentionally narrower than `airc teardown`:
    # no gist deletion, no identity/peer/message wipe, no cross-scope
    # process matching.
    local _repair_running_monitor=0
    if [ -f "$CONFIG" ] && command -v gh >/dev/null 2>&1; then
      local _map_lines _line _ch _gid _canonical_gid
      _map_lines=$(airc_config_list_channel_gists "$CONFIG" || true)
      while IFS=$'\t' read -r _ch _gid; do
        [ -z "$_ch" ] && continue
        [ -z "$_gid" ] && continue
        _canonical_gid=$(_mesh_find_any "$_ch")
        if [ -n "$_canonical_gid" ] && [ "$_canonical_gid" != "$_gid" ]; then
          echo "  airc join: running monitor is on stale #${_ch} gist $_gid; canonical is $_canonical_gid."
          airc_config_set_channel_gist "$_ch" "$_canonical_gid" "$CONFIG" 2>/dev/null || true
          _repair_running_monitor=1
        fi
      done <<< "$_map_lines"
    fi
    if [ "$_repair_running_monitor" = "1" ]; then
      echo "  airc join: restarting this scope's AIRC process to leave the solo island."
      _join_restart_scope_processes
      sleep 1
    elif _join_scope_has_duplicate_transport; then
      echo "  airc join: duplicate same-scope transport generation detected; restarting this scope's AIRC process."
      _join_restart_scope_processes
      sleep 1
    else
      local _health_out _health_rc=0
      _health_out=$("$(airc_rs_bin)" transport health \
        --home "$AIRC_WRITE_DIR" \
        --config "$CONFIG" \
        --fail 2>/dev/null) || _health_rc=$?
      if [ "$_health_rc" != "0" ] && _join_transport_in_startup_grace "$_health_out" "$_early_pidfile"; then
        local _early_pids; _early_pids=$(cat "$_early_pidfile" 2>/dev/null | tr '\n' ' ')
        echo "  airc join: AIRC process is still starting in this scope (AIRC PIDs: $_early_pids)."
        printf '%s\n' "$_health_out" | sed 's/^/    /' || true
        _join_show_status_and_inbox
        [ "$attach" = "1" ] && _join_attach_local_stream
        return 0
      elif [ "$_health_rc" != "0" ]; then
        echo "  airc join: AIRC process exists but transport is degraded; restarting this scope's AIRC process."
        printf '%s\n' "$_health_out" | sed 's/^/    /' || true
        _join_restart_scope_processes
        sleep 1
      else
        local _early_pids; _early_pids=$(cat "$_early_pidfile" 2>/dev/null | tr '\n' ' ')
        echo "  airc join: already joined in this scope (AIRC PIDs: $_early_pids)."
        _join_show_status_and_inbox
        [ "$attach" = "1" ] && _join_attach_local_stream
        return 0
      fi
    fi
  fi
  # Stale or absent pidfile — leave for the canonical cleanup block
  # below to remove + proceed normally with the connect flow.

  # Pre-flight: gh auth check. The gh keyring can silently invalidate
  # (token revoked / 2FA flow expired / brew upgrade replaced gh
  # without re-auth) and EVERY downstream gh API call then fails
  # silently — bearer.send returns auth_failure, bearer recv polls
  # forever getting nothing, peers see "AIRC process running, no traffic"
  # which is the exact freeze pattern Joel kept hitting. Catch this
  # at connect time so the user gets a clear error instead of a
  # mystery timeout.
  #
  # Skip cases (gh isn't needed):
  #   1. --no-room → user opted into legacy 1:1 invite mode (no
  #      substrate). Pre-#338 the unconditional check killed CI's
  #      clean-install smoke test which exercises this path.
  #   2. Inline invite-string positional arg (`name@user@host[:port]#pubkey`)
  #      → JOIN MODE legacy direct-pair, also no substrate. The
  #      integration suite's spawn_joiner uses this; pre-fix the
  #      check fired and CI runners (no PAT) failed every joiner.
  #      Pattern matches what JOIN MODE itself parses at line ~862.
  #   3. Live monitor exists in this scope (trust-existing-monitor
  #      short-circuit above already returned).
  local _looks_like_invite=0
  if [ "$#" -ge 1 ] && [[ "$1" == *@*@*#* ]]; then
    _looks_like_invite=1
  fi
  if [ "$use_room" = "1" ] && [ "$_looks_like_invite" = "0" ] \
     && command -v gh >/dev/null 2>&1; then
    # Pre-flight via the centralized state machine (lib_auth.sh).
    # ok → proceed; rate_limited → proceed degraded so the monitor can
    # start and use cached/local transport while GH's burst throttle
    # clears; invalid → airc instigates the browser self-heal
    # in-process; not_installed → caller's outer guard already handled
    # this.
    AIRC_GH_RATE_LIMIT_NONFATAL=1 airc_ensure_gh_auth_or_heal "airc join" \
      || die "gh auth not OK — see message above for next step"
  fi

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
  # Resolution: git remote org first ('acme/api' → #acme),
  # parent-dir basename second (local-only repos). Falls back to #general
  # only when neither signal fires (non-git dir, no remote). The skill
  # /join contract documents this as the default.
  #
  # The trade-off: two tabs in DIFFERENT projects on the same gh account
  # land in different rooms (an #acme tab can't see an #example
  # tab by default). That's intentional — project work shouldn't mix with unrelated
  # project chatter. Cross-project agents who need a shared lobby:
  # `AIRC_NO_AUTO_ROOM=1 airc join` or `airc join --room general`.
  #
  # Two tabs in the SAME project converge automatically: both acme
  # tabs auto-scope to #acme, both find each other. That's the case
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
      # Phase 2C clarity: the mesh substrate
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
      echo "❌ airc join: stdout pipe closed — no notification consumer."
      echo ""
      echo "   Inbound peer messages would have been silently lost. Most"
      echo "   common cause: airc was launched as a one-shot bash exec,"
      echo "   nohup, background \"&\", or detached shell. The pairing"
      echo "   succeeds and messages.jsonl fills, but the AI agent never"
      echo "   sees inbound notifications. That is the worst kind of"
      echo "   silent failure — looks fine, is broken."
      echo ""
      echo "   Right launchers:"
      echo "     • Claude Code skill:   /airc:join <invite>"
      echo "     • Monitor tool:        Monitor(persistent=true, description=\"airc\", command=\"airc join <invite>\")"
      echo "     • Interactive shell:   just type \`airc join <invite>\` at a TTY"
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
  # Previously users had to run `airc teardown` manually before `airc join`
  # if a prior monitor was still around — easy to forget, often resulted in
  # duplicate monitors or port collisions. Now a single `airc join` or
  # `airc resume` does the right thing.
  # #292 fix: refuse to stomp a live monitor. Pre-fix this block
  # auto-killed any PIDs in airc.pid before continuing — which silently
  # destroyed a live monitor in a sibling shell when the user ran
  # `airc join` from a second terminal to verify state. That made
  # multi-tab sanity-checking destructive. Post-fix: detect liveness,
  # print a one-liner pointing to the right tools, exit 0 cleanly.
  # Stale pidfile (no live PIDs) still gets cleaned up + we proceed.
  #
  # 2026-05-03 (#97 self-heal): bare `kill -0 $pid` returns true for ANY
  # live process at that PID, including processes the OS has REUSED the
  # PID for after sleep/wake. Joel hit this — slept laptop, airc died,
  # OS reused PIDs, this block then saw "alive" against zombie PIDs and
  # refused to self-heal. Verify cmdline shapes like airc before treating
  # the PID as ours. Same regex shape as cmd_teardown's parent-chain
  # reaper (#446) and the helper in airc::_monitor_alive_with_bearer_fallback.
  local stale_pidfile="$AIRC_WRITE_DIR/airc.pid"
  if [ -f "$stale_pidfile" ]; then
    local stale_pids; stale_pids=$(cat "$stale_pidfile" 2>/dev/null | tr '\n' ' ')
    local any_alive=0
    local alive_pids=""
    for p in $stale_pids; do
      if kill -0 "$p" 2>/dev/null; then
        local _cmd
        _cmd=$(proc_cmdline "$p" 2>/dev/null || true)
        if echo "$_cmd" | grep -Eq '(^|[[:space:]])/[^[:space:]]*/airc[[:space:]]+(connect|join)([[:space:]]|$)|(^|[[:space:]])airc[[:space:]]+(connect|join)([[:space:]]|$)|eval .*airc[[:space:]]+(connect|join)'; then
          any_alive=1
          alive_pids="$alive_pids $p"
        fi
      fi
    done
    if [ "$any_alive" = "1" ]; then
      echo "  airc join: already joined in this scope (AIRC PIDs:$alive_pids)."
      _join_show_status_and_inbox
      [ "$attach" = "1" ] && _join_attach_local_stream
      return 0
    fi
    # Stale pidfile (no live airc processes — either dead, or PIDs were
    # reused by the OS for unrelated procs). Safe to clean.
    rm -f "$stale_pidfile"
  fi

  # UI attach mode should not make the Claude/WSL Monitor shell own the
  # transport lifetime. On Windows WSL2, Claude Code launches Monitor
  # commands through `wsl bash -lc ...`; that wrapper can disappear and
  # take foreground shell subprocesses with it even after airc printed
  # "Monitoring for messages...". Start the transport as a scope-local
  # background owner, verify it, then attach this UI process to the
  # local message stream. This is not an OS daemon; it is the same
  # project-scope airc process `airc quit`/`airc teardown` manage.
  if [ "$attach" = "1" ] && [ "${AIRC_NO_ATTACH:-0}" != "1" ]; then
    _join_spawn_transport_for_attach ${_orig_args[@]+"${_orig_args[@]}"}
    return $?
  fi

  # Mark transport ownership before expensive discovery/bootstrap work.
  # Host/joiner mode rewrites this later with child PIDs once those
  # loops exist; until then, the parent shell itself is the live
  # transport startup owner. Without this early marker, attach-mode
  # launchers can report "not running" for a process that is alive but
  # still doing gh discovery, stale-state repair, or first-host setup.
  mkdir -p "$AIRC_WRITE_DIR"
  : >> "$MESSAGES"
  echo "$$" > "$AIRC_WRITE_DIR/airc.pid"
  trap '
    _airc_startup_rc=$?
    rm -f "$AIRC_WRITE_DIR/airc.pid" 2>/dev/null
    exit $_airc_startup_rc
  ' EXIT INT TERM
  if [ -n "${AIRC_TEST_STARTUP_DELAY_SEC:-}" ]; then
    sleep "$AIRC_TEST_STARTUP_DELAY_SEC"
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

    # ── Mesh discovery (canonical channel gist) ──────────────────
    # Every `airc join` resolves the requested/default channel through
    # the same content-based channel_gist rule used by subscribe/send.
    # Do NOT match only the human description "airc mesh": stale
    # "airc room:" gists can still carry the live envelope, and using a
    # different resolver here is exactly how #general split-brained.
    #
    # The --room flag still records the channel(s) this client wants
    # to subscribe to (Phase 2 will route messages by channel), but it
    # no longer drives gist discovery — every subscriber on the account
    # converges on the same host.
    _did_room_discovery=1
    _join_phase "querying gh for mesh on this account (#${room_name})"
    local _mesh_id; _mesh_id=$(_mesh_find_any "$room_name")
    if [ -n "$_mesh_id" ]; then
      local _mesh_invite_id; _mesh_invite_id=$(_mesh_find "$room_name")
      if [ -n "$_mesh_invite_id" ] && [ "$_mesh_invite_id" = "$_mesh_id" ]; then
        echo "  Found mesh on your gh account → joining ($_mesh_id)"
        target="$_mesh_id"
        # fall through to gist resolver below
      else
        echo "  Found canonical room gist for #${room_name} → becoming host on that existing gist ($_mesh_id)."
        export AIRC_ADOPT_GIST="$_mesh_id"
        # Host branch below will rewrite the same gist with a fresh
        # invite/host lease. Do not join a newer invite-bearing duplicate:
        # that is the solo-island trap.
      fi
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
        echo "  Pick one to join:  airc join <id>"
        echo "  Host a new mesh:   AIRC_NO_DISCOVERY=1 airc join --no-general"
        exit 0
      fi
    fi
  fi

  # ── Mnemonic resolver (humanhash → gist id, same gh account) ─────
  # Joel's UX target: a friend (or your own other tab) can type
  #   airc join oregon-uncle-bravo-eleven
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
      die "Mnemonic '$target' lookup needs the 'gh' CLI. Install gh + 'gh auth login', or use the gist id directly: airc join <id>"
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
      die "Mnemonic '$target' didn't match any airc gist on this gh account. If your friend's gist is on a different gh, paste the gist id directly: airc join <id>"
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
    # out to be unreachable, JOIN MODE takes over this same gist as the
    # new host of the same room.
    _resolved_gist_id="$gist_id"
    # Gist IDs are hex strings, typically 20-32 chars but accept any
    # plausible length so future GH ID schemes don't break us.
    if echo "$gist_id" | grep -qE '^[a-zA-Z0-9]{6,40}$'; then
      _join_phase "resolving room gist contents ($gist_id)"
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
      # gh api → airc-rs extracts
      # the first file's content. This is the rest-API path; it's preferred
      # over the gh gist view --raw path because the latter prepends the
      # gist description as a header line that we'd then have to strip.
      if command -v gh >/dev/null 2>&1; then
        raw_content=$( (gh api "gists/$gist_id" 2>/dev/null \
                        | "$(airc_rs_bin)" gist gist-content --channel "$room_name" 2>/dev/null) || true )
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
          # Prefer the requested channel's envelope; fall back to the
          # first non-dotfile for legacy single-file invite gists.
          local _gist_file
          _gist_file="$_gist_tmp/airc-room-${room_name}.json"
          if [ ! -f "$_gist_file" ]; then
            _gist_file=$(find "$_gist_tmp" -maxdepth 1 -type f ! -name '.git*' 2>/dev/null | head -1 || true)
          fi
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
                        | "$(airc_rs_bin)" gist gist-content --channel "$room_name" 2>/dev/null) || true )
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
      airc_ver=$(printf '%s' "$raw_content" | "$(airc_rs_bin)" gist get .airc 2>/dev/null)
      kind=$(printf '%s' "$raw_content" | "$(airc_rs_bin)" gist get .kind 2>/dev/null)
      if [ -n "$airc_ver" ]; then
          # Versioned envelope — dispatch on kind.
          case "$kind" in
            invite)
              # Single-pair invite (legacy + --no-general flow). Gist is
              # ephemeral; host deletes after pair.
              resolved=$(printf '%s' "$raw_content" \
                         | "$(airc_rs_bin)" gist get .invite 2>/dev/null \
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
                         | "$(airc_rs_bin)" gist get .invite 2>/dev/null \
                         | head -1 | tr -d '\r\n ')
              # New mesh shape: .channels[]; legacy room shape: .name.
              # Prefer channels[0] if present; fall back to .name.
              resolved_room_name=$(printf '%s' "$raw_content" \
                         | "$(airc_rs_bin)" gist get-first-of '.channels[0]' '.name' 2>/dev/null)
              # Multi-address: capture host.addresses[] + host.machine_id
              # for the joiner's address-picker (peer_pick_address). Empty
              # if the host published a pre-multi-address envelope; in
              # that case JOIN MODE falls back to the parsed-from-invite
              # host:port (legacy single-address path).
              _resolved_addresses_json=$(printf '%s' "$raw_content" \
                         | "$(airc_rs_bin)" gist get-json .host.addresses 2>/dev/null)
              _resolved_host_machine_id=$(printf '%s' "$raw_content" \
                         | "$(airc_rs_bin)" gist get .host.machine_id 2>/dev/null)

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
              _hb_iso=$(printf '%s' "$raw_content" | "$(airc_rs_bin)" gist get .last_heartbeat 2>/dev/null)
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
        # Win→Mac e2e 2026-04-27 — bash on Git Bash
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

    # Stale-heartbeat fast-path. The gist is the durable room; the host is
    # replaceable. If the host is stale, take over the SAME gist in place
    # so every peer polling that room converges instead of getting a new
    # solo island.
    #
    # Backward compat: pre-heartbeat gists have no last_heartbeat field,
    # _resolved_heartbeat_stale stays 0, this block is a no-op, and the
    # SSH-failure self-heal still catches the dead host (slower, but
    # correct).
    if [ "$_resolved_heartbeat_stale" = "1" ] && [ -n "$resolved_room_name" ] \
       && [ -n "$_resolved_gist_id" ]; then
      echo ""
      _join_phase "taking over stale host (re-exec into host mode)"
      echo "  ⚠  Host of #${resolved_room_name} is stale (last heartbeat ${_resolved_heartbeat_age}s ago) — taking over existing mesh..."
      echo "     (prior host's gist: $_resolved_gist_id)"
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

    [ -z "$peer_name" ] || [ -z "$ssh_target" ] && die "Format: airc join name@user@host"

    # Multi-address override: if the gist envelope carried host.addresses[]
    # and host.machine_id, use peer_pick_address to choose the cheapest
    # reachable scope (same-machine localhost > same-LAN > tailscale).
    # This is what makes Tailscale truly optional — same-machine and
    # same-LAN peers connect via 127.0.0.1 / LAN IP regardless of the
    # invite string's host:port (which historically advertised one IP).
    #
    # `_addr_picker_state` tracks what happened so the self-heal block
    # below can decide whether nuking the host's gist is justified:
    #   "no_addrs"    — host published no addresses[] (legacy gist or
    #                   pre-multi-address protocol). We tried only the
    #                   invite-string ssh_target. Self-heal allowed —
    #                   we have no other reachability info to act on.
    #   "picked"      — picker returned a believed-reachable address
    #                   AND we used it. If THAT failed, the host really
    #                   does seem dead. Self-heal allowed.
    #   "no_match"    — host published addresses[] BUT picker found
    #                   no scope this peer can reach (e.g. Mac without
    #                   tailscale + Windows host whose only non-
    #                   localhost entry is tailscale). Falling through
    #                   to invite-string ssh_target is no more reachable
    #                   than what the picker rejected. Self-heal here
    #                   would nuke the gist for OTHER peers who CAN
    #                   reach the host — destructive cross-peer
    #                   damage from one peer's network mismatch.
    local _addr_picker_state="no_addrs"
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
        _addr_picker_state="picked"
      else
        _addr_picker_state="no_match"
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
      # Phase 2B.2.1: joiner's cwd-derived or
      # explicit --room intent must NOT be overwritten by the host's
      # advertised channel. If the user wanted #acme (cwd) and the
      # mesh host happens to advertise #example, the joiner is
      # subscribed to BOTH — cmd_send default = user's intent; the
      # host's channel is tagged on too so their traffic still displays
      # via [#example] prefix.
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
        #
        # Test hook: AIRC_TEST_FAIL_ENSURE_CHANNEL — when set, treat
        # ensure_channel_subscribed_with_gist as failed for that exact
        # channel name. Lets the regression scenario exercise the
        # intent-failed-but-host-reachable fallback path deterministically
        # without needing a real gh rate-limit / missing-permissions repro.
        echo "$_intent" > "$AIRC_WRITE_DIR/room_name"
        local _intent_ok=1 _host_ok=1
        if [ "${AIRC_TEST_FAIL_ENSURE_CHANNEL:-}" = "$_intent" ] \
           || ! ensure_channel_subscribed_with_gist "$_intent" --first >/dev/null; then
          _intent_ok=0
        fi
        # We already resolved the host's room gist to get here. Persist that
        # mapping before subscribing to the host channel so the fallback path
        # does not immediately hit GitHub discovery again during the exact
        # transient/rate-limited condition it is meant to survive.
        if [ -n "${_resolved_gist_id:-}" ]; then
          airc_config_set_channel_gist "$resolved_room_name" "$_resolved_gist_id" "$CONFIG" 2>/dev/null || true
        fi
        if [ "${AIRC_TEST_FAIL_ENSURE_CHANNEL:-}" = "$resolved_room_name" ] \
           || ! ensure_channel_subscribed_with_gist "$resolved_room_name" >/dev/null; then
          _host_ok=0
        fi
        if [ "$_intent_ok" = "1" ]; then
          if [ "$_host_ok" = "1" ]; then
            echo "  Joined mesh — host primarily labels #${resolved_room_name}; subscribed: #${_intent} (default), #${resolved_room_name}"
          else
            echo "  ⚠ Could not bootstrap host's channel #${resolved_room_name}; subscribed to #${_intent} only" >&2
            echo "  Joined #${_intent}"
          fi
        else
          # Intent bootstrap failed. Pre-fix this die'd the whole join,
          # which made plain `airc join` (auto-scope intent) inexplicably
          # exit on a working mesh whenever the intent gist couldn't be
          # resolved (gh rate-limit, missing scope on token, transient
          # API error). When the host's channel IS reachable the join
          # has a viable subscription — fall back to it as primary,
          # warn the user, and keep going. Only die when BOTH are gone:
          # at that point there's no channel to land in.
          if [ "$_host_ok" = "1" ]; then
            echo "  ⚠ Could not bootstrap intended #${_intent}; falling back to host's channel #${resolved_room_name} as primary." >&2
            echo "$resolved_room_name" > "$AIRC_WRITE_DIR/room_name"
            echo "  Joined #${resolved_room_name} (your intent #${_intent} could not bootstrap; rerun 'airc join --room ${_intent}' once gh is healthy)"
          else
            die "Could not bootstrap #${_intent} OR host's channel #${resolved_room_name}; no viable subscription. Check 'gh auth status' + retry."
          fi
        fi
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
    my_x25519_pub=$("$(airc_rs_bin)" identity bootstrap --home "$AIRC_WRITE_DIR" --dir "$IDENTITY_DIR" 2>/dev/null || echo "")

    # Read own identity blob to send in handshake (issue #34 v2 — peers
    # cache each other's identity at pair-time so airc whois works fast).
    local my_identity_json; my_identity_json=$("$(airc_rs_bin)" config get --home "$AIRC_WRITE_DIR" --config "$CONFIG" identity "{}" 2>/dev/null || echo "{}")
    [ -z "$my_identity_json" ] && my_identity_json="{}"

    local response
    local _pair_ok=1
    response=$("$(airc_rs_bin)" handshake send "$peer_host_only" "$peer_port" \
                  --my-name "$my_name" \
                  --my-host "$(whoami)@$(get_host)" \
                  --my-ssh-pub "$my_ssh_pub" \
                  --my-sign-pub "$my_sign_pub" \
                  --my-x25519-pub "$my_x25519_pub" \
                  --my-airc-home "$AIRC_WRITE_DIR" \
                  --my-identity-json "$my_identity_json" 2>&1) || _pair_ok=0

    if [ "$_pair_ok" = "0" ]; then
      # Pair failure recovers by taking over the SAME gist in place.
      # Deleting the old gist and publishing a new one split-brained the
      # bus; preserving and rewriting the durable room gist makes all
      # pollers converge.
      if [ -n "$resolved_room_name" ] && [ -n "$_resolved_gist_id" ] \
         && command -v gh >/dev/null 2>&1 \
         && [ "$_addr_picker_state" != "no_match" ]; then
        echo ""
        echo "  ⚠  Host of #${resolved_room_name} unreachable — taking over existing mesh..."
        echo "     (prior host's gist: $_resolved_gist_id)"
        _self_heal_stale_host "$_resolved_gist_id"
      elif [ "$_addr_picker_state" = "no_match" ]; then
        # Picker found no scope this peer can reach. Surface the situation
        # but do NOT nuke the gist. The host may be perfectly reachable
        # for peers on the other matching scope (e.g. peers on the same
        # tailnet when WE lack tailscale). Per the global "evidence is
        # for the debugger, not the trash" rule — print explicit reason
        # so users debugging "why didn't I auto-pair" know it's a network
        # topology mismatch rather than a host-down event.
        echo "" >&2
        echo "  ⚠  Host of #${resolved_room_name} published no scope this peer can reach." >&2
        echo "     Skipping self-heal (gist preserved for peers who CAN reach the host)." >&2
        echo "     Direct pair unavailable; gh-bearer broadcasts still work via gist." >&2
        echo "" >&2
      fi
      # Either not a room flow, or no gh, or no resolved_room_name → original die.
      # Surface the captured pair-handshake stderr. Per the global
      # "never swallow errors" rule — evidence is for the debugger,
      # not the trash. The handshake captured stderr+stdout via 2>&1 into
      # $response just above, so we have the real error in hand.
      if [ -n "${response:-}" ]; then
        echo "" >&2
        echo "  Pair handshake output (captured stderr/stdout):" >&2
        printf '%s\n' "$response" | sed 's/^/    /' >&2
        echo "" >&2
      fi
      die "Can't reach $peer_host_only:$peer_port. Is the host running 'airc join'?"
    fi

    # Authorize host's SSH pubkey (for the joiner->host auth direction).
    # NOTE: the handshake's ssh_pub is airc's USER identity key — not the
    # sshd server host key used for known_hosts verification. Proper
    # host-key handling relies on ssh's own accept-new mode, plus a
    # targeted ssh-keygen -R when a PRIOR real-sshd host key in known_hosts
    # is known stale (e.g. the server rotated sshd host keys).
    local host_ssh_pub
    host_ssh_pub=$(printf '%s' "$response" | "$(airc_rs_bin)" gist get .ssh_pub 2>/dev/null || true)
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
    host_airc_home=$(printf '%s' "$response" | "$(airc_rs_bin)" gist get .airc_home 2>/dev/null || true)
    # Phase E.2: capture host's X25519 pubkey from handshake response
    # so cmd_send can encrypt envelopes destined for this peer.
    host_x25519_pub=$(printf '%s' "$response" | "$(airc_rs_bin)" gist get .x25519_pub 2>/dev/null || true)
    "$(airc_rs_bin)" identity write-peer-record --home "$AIRC_WRITE_DIR" \
        --peers-dir "$PEERS_DIR" \
        --peer-name "$peer_name" \
        --host "$ssh_target" \
        --airc-home "$host_airc_home" \
        --x25519-pub "$host_x25519_pub" \
        --paired "$(timestamp)" \
        2>/dev/null || true

    # If we resolved this pair via gist discovery (vs. inline-invite),
    # persist the gist id so resume-time freshness checks can detect a
    # host-lease refresh or gist rotation before re-pairing against a
    # stale host (issue #83). Cleared by cmd_part on graceful leave.
    if [ -n "$_resolved_gist_id" ]; then
      echo "$_resolved_gist_id" > "$AIRC_WRITE_DIR/room_gist_id"
      # #283: also map this channel→gist in channel_gists so the
      # multi-channel monitor polls it and cmd_send routes by channel.
      if [ -n "$resolved_room_name" ]; then
        airc_config_set_channel_gist "$resolved_room_name" "$_resolved_gist_id" "$CONFIG" 2>/dev/null || true
      fi
    fi

    # Persist host details in own config so `airc invite` can reconstruct
    # the join string for onward sharing without a fresh handshake. Also
    # cache the host's identity blob from the handshake response so
    # `airc whois <host-name>` works locally (issue #34 v2).
    local host_identity_json; host_identity_json=$(printf '%s' "$response" | "$(airc_rs_bin)" gist get .identity "{}" 2>/dev/null || echo "{}")
    [ -z "$host_identity_json" ] && host_identity_json="{}"
    # Pass values as env vars instead of bash-substituted into the
    # python heredoc body. PR #164 retest 2026-04-27
    # found host_airc_home / host_name / host_port / host_ssh_pub /
    # host_identity all silently unwritten on Win→Mac join: if ANY of
    # the bash substitutions broke the python source (newline in
    # host_ssh_pub, weird char in host_airc_home, peer_port empty/
    # non-numeric, etc.), the whole heredoc errored out via
    # `2>/dev/null || true` and zero fields landed in config. Switch
    # to env-var pass — python reads from os.environ; bash never
    # touches the python source. Also emit stderr to surface failures
    # for the future debugger (not /dev/null).
    "$(airc_rs_bin)" config set-host-block --home "$AIRC_WRITE_DIR" \
        --config "$CONFIG" \
        --host-airc-home "$host_airc_home" \
        --host-name "$peer_name" \
        --host-port "${peer_port:-7547}" \
        --host-ssh-pub "$host_ssh_pub" \
        --host-identity-json "$host_identity_json" \
        || echo "  ⚠  config write failed (host_airc_home/host_name/host_port/host_ssh_pub may be unset). airc may still work if subsequent retries refresh." >&2

    # Pick up reminder setting from host
    local host_reminder
    host_reminder=$(printf '%s' "$response" | "$(airc_rs_bin)" gist get .reminder 300 2>/dev/null || echo "300")
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

    _join_phase "subscribing to #general (sidecar)"
    spawn_general_sidecar_if_wanted
    _join_emit_join_events "$my_name"
    _join_phase "monitor stream attached — cold start complete"
    _join_phase_done
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
    _join_phase "hosting as '$name' — bootstrapping room gist"
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
      echo "    airc join $_invite_long"
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
      airc_config_subscribe "$room_name" "$CONFIG" 1 2>/dev/null || true
      if [ -n "${AIRC_ADOPT_GIST:-}" ]; then
        echo "  Hosting #${room_name} — recovering existing room gist ${AIRC_ADOPT_GIST}."
      else
        echo "  Hosting #${room_name} — creating or adopting the canonical room gist."
      fi
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
        # Convergence-first (#321 follow-up): before bootstrapping a NEW
        # gist for this channel, consult channel_gist.find_existing.
        # If a canonical gist for this room name already exists on the
        # gh account, USE IT — don't create yet another duplicate. This
        # was the pre-fix bug that produced multiple #general gists on
        # the same account: every --as-host bootstrap created its own
        # gist regardless of what was already there. With find-first,
        # all hosts on the gh account converge on the oldest canonical.
        local _existing_room_gid="${AIRC_ADOPT_GIST:-}"
        if [ -z "$_existing_room_gid" ] \
           && [ -f "$AIRC_WRITE_DIR/room_name" ] \
           && [ -f "$AIRC_WRITE_DIR/room_gist_id" ]; then
          local _marker_room _marker_gid
          _marker_room=$(cat "$AIRC_WRITE_DIR/room_name" 2>/dev/null || true)
          _marker_gid=$(cat "$AIRC_WRITE_DIR/room_gist_id" 2>/dev/null || true)
          if [ "$_marker_room" = "$room_name" ] && printf '%s' "$_marker_gid" | grep -qE '^[0-9a-f]{32}$'; then
            _existing_room_gid="$_marker_gid"
          fi
        fi
        if [ "$use_room" = "1" ]; then
          # Use full retry so gh's gist-listing eventual consistency
          # (a just-created gist may not appear in `gh gist list` for
          # several seconds) doesn't cause the host to create a
          # duplicate of a gist that already exists. Cost: up to
          # ~4.5s on a fresh-account first-spawn (no existing gist
          # ever); accepted as a one-time cost on bootstrap to
          # guarantee convergence on every later restart.
          #
          # Exception: AIRC_NO_DISCOVERY=1 (explicit opt-out) — the
          # caller said "don't go looking." Half-honoring that flag
          # (skip early mesh-find but still consult find_existing
          # here) was a real footgun: on accounts with many gists
          # find_existing's `gh api gists --paginate` takes ~30s per
          # call, retried 3× = ~90s before falling through to
          # create_new. Tests + CI scenarios that explicitly opt out
          # would block on it. When AIRC_NO_DISCOVERY=1, skip the
          # resolve and go straight to create_new — same as the
          # early mesh-find gate at line ~568.
          if [ -z "$_existing_room_gid" ] && [ "${AIRC_NO_DISCOVERY:-0}" = "1" ]; then
            local _configured_gid
            _configured_gid=$(airc_config_get_channel_gist "$room_name" "$CONFIG" || true)
            if [ -n "$_configured_gid" ] && [ ! -f "$AIRC_WRITE_DIR/room_gist_id" ]; then
              _existing_room_gid=$("$(airc_rs_bin)" channel-gist find \
                                   --channel "$room_name" 2>/dev/null || true)
            fi
          fi
          if [ -z "$_existing_room_gid" ] && [ "${AIRC_NO_DISCOVERY:-0}" != "1" ]; then
            local _host_preflight_rc=0
            _existing_room_gid=$("$(airc_rs_bin)" channel-gist host-preflight \
                                 --channel "$room_name" --config "$CONFIG" 2>/dev/null) || _host_preflight_rc=$?
            if [ "${_host_preflight_rc:-0}" = "2" ]; then
              die "GitHub room discovery is unavailable for #${room_name}; refusing to create a new solo room. Retry after the GitHub backoff clears."
            fi
          fi
        fi
        if [ -n "$_existing_room_gid" ]; then
          echo "  ✓ Found canonical gist for #${room_name} on this gh account → using existing ($_existing_room_gid)"
          local _gist_id="$_existing_room_gid"
          local _gist_url="https://gist.github.com/$_gist_id"
          local _gist_kind="room"
          # Persist the canonical mapping. Heartbeat + sends route here
          # automatically; first send creates messages.jsonl in the
          # existing gist.
          echo "$_gist_id" > "$AIRC_WRITE_DIR/room_gist_id"
          echo "$room_name" > "$AIRC_WRITE_DIR/room_name"
          airc_config_set_channel_gist "$room_name" "$_gist_id" "$CONFIG" 2>/dev/null || true
          : >"$AIRC_WRITE_DIR/.using_existing_room_gist"
        fi

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
        if [ -n "${_existing_room_gid:-}" ] && [ "$use_room" = "1" ]; then
          _join_phase "publishing host lease to existing room gist"
        else
          _join_phase "creating new room gist on this gh account"
        fi
        local _gist_url=""
        if [ -n "${_existing_room_gid:-}" ] && [ "$use_room" = "1" ]; then
          if "$(airc_rs_bin)" gh patch-gist-file \
               --gist-id "$_existing_room_gid" \
               --filename "airc-room-${room_name}.json" \
               --content-file "$_gist_tmp" >/dev/null 2>/dev/null \
             || gh gist edit "$_existing_room_gid" -a "$_gist_tmp" >/dev/null 2>/dev/null; then
            _gist_url="https://gist.github.com/$_existing_room_gid"
          fi
        else
          _gist_url=$(gh gist create -d "$_gist_desc" "$_gist_tmp" 2>/dev/null | tail -1)
        fi
        if [ -n "$_gist_url" ]; then
          local _gist_id="${_gist_url##*/}"
          local _hh; _hh=$(humanhash "$_gist_id" 2>/dev/null)
          if [ "$use_room" = "1" ]; then
            "$(airc_rs_bin)" channel-gist remember-created \
              --channel "$room_name" \
              --gist-id "$_gist_id" \
              --description "$_gist_desc" \
              --payload-file "$_gist_tmp" 2>/dev/null || true
          fi
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
            airc_config_set_channel_gist "$room_name" "$_gist_id" "$CONFIG" 2>/dev/null || true

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
            # exits, so kill -9 on the host stops heartbeats within one
            # interval. Joiners treat last_heartbeat older than
            # AIRC_HEARTBEAT_STALE (default 90s = 3 missed beats) as
            # stale and self-heal in-place as the new host.
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
              # whole tree. The room gist itself is durable and is not
              # deleted by normal host exit.
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
                # One gist is the durable wire for one channel. Keep
                # the host lease envelope single-channel even if this
                # scope is subscribed to multiple channels; otherwise
                # the resolver can stop treating the actual
                # airc-room-<channel>.json gist as canonical and drift
                # toward a newer solo invite duplicate.
                local _hb_channels="[\"${_hb_room}\"]"
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
                # That eviction loop is the surface QA
                # root-caused 2026-04-29; it's also what nuked the
                # #example gist mid-ping-debug. Ensuring the temp
                # basename matches the canonical filename closes the
                # whole convergent class.
                local _hb_tmpdir; _hb_tmpdir=$(mktemp -d -t airc-hb.XXXXXX)
                local _hb_tmp="${_hb_tmpdir}/airc-room-${_hb_room}.json"
                printf '%s\n' "$_hb_payload" > "$_hb_tmp"
                # Rotate the host's messages.jsonl when it exceeds the
                # AIRC_LOG_MAX_LINES threshold (default 5000). Trims
                # in-place via airc-rs log rotate; SSH-tail's -F flag detects
                # the atomic replace and re-opens. Joiners with offsets
                # past the new file's line count are caught by #245.
                # Cheap no-op when under threshold.
                "$(airc_rs_bin)" log rotate --path "$_hb_messages" \
                  --max-lines "${AIRC_LOG_MAX_LINES:-5000}" \
                  --keep-lines "${AIRC_LOG_KEEP_LINES:-2500}" >/dev/null 2>&1 || true
                # Capture stderr to a state file (per never-swallow-errors
                # rule). Try edit-replace first; if that fails with the
                # multi-file-disambiguation error (basename not yet in
                # gist after a take-over / fresh-host race — bearer_gh.py
                # has the same defense for #285), retry as add. Track
                # consecutive failures: after N in a row, detect
                # active-host-evicted (#224) and self-heal.
                _hb_tried_add=0
                if "$(airc_rs_bin)" gh patch-gist-file \
                     --gist-id "$_gist_id" \
                     --filename "airc-room-${_hb_room}.json" \
                     --content-file "$_hb_tmp" >/dev/null 2>"$_hb_stderr"; then
                  _consec_fail=0
                elif grep -qE 'unsure what file to edit|file does not exist|no such file' "$_hb_stderr" 2>/dev/null \
                     && gh gist edit "$_gist_id" -a "$_hb_tmp" >/dev/null 2>"$_hb_stderr"; then
                  # Add-as-new succeeded — gist now has the canonical
                  # heartbeat file; subsequent edits will hit the replace
                  # path. Treat as success.
                  _consec_fail=0
                  _hb_tried_add=1
                else
                  _consec_fail=$((_consec_fail + 1))
                  if [ "$_consec_fail" -ge "$_max_consec_fail" ]; then
                    local _stderr_tail; _stderr_tail=$(tail -1 "$_hb_stderr" 2>/dev/null | tr -d '\n' | tr '"' "'")
                    # Classify the gh error into airc-vocabulary so the
                    # event log doesn't leak gh CLI internals to the
                    # user. Issue #348.
                    local _classified
                    case "$_stderr_tail" in
                      *'rate limit'*|*'abuse detection'*|*'secondary rate'*|*'API rate limit exceeded'*)
                        _classified="rate-limit (gh secondary; back off 5-15 min before retry)" ;;
                      *'unsure what file to edit'*|*'file does not exist'*|*'no such file'*)
                        _classified="multi-file gist disambiguation (#348 — airc bug, please report)" ;;
                      *'401'*|*'Unauthorized'*|*'token'*|*'keyring'*|*'auth'*)
                        _classified="gh auth failure (run 'gh auth login -h github.com')" ;;
                      *'network'*|*'connection refused'*|*'timeout'*|*'DNS'*|*'temporary failure'*|*'unreachable'*)
                        _classified="network error" ;;
                      '')
                        _classified="unknown (no stderr captured)" ;;
                      *)
                        _classified="$_stderr_tail" ;;
                    esac
                    case "$_classified" in
                      rate-limit*)
                        # GitHub explicitly warns that continuing to
                        # retry while secondary-limited can extend the
                        # throttle or get the integration banned. This
                        # is degraded control-plane health, not proof
                        # that our local host is dead. Do NOT self-
                        # evict or SIGTERM the parent; that was the
                        # monitor death spiral Joel hit on canary
                        # 2026-05-04. Keep the host process alive, let
                        # local/LAN transport continue, and back off
                        # heartbeat writes before the next attempt.
                        local _backoff_sec="${AIRC_GH_SECONDARY_BACKOFF_SEC:-60}"
                        printf '[%s] airc: HOST HEARTBEAT DEGRADED for #%s on gist %s — gh secondary rate limit; keeping host alive and backing off %ss.\n' \
                          "$(timestamp)" "$_hb_room" "$_gist_id" "$_backoff_sec" >> "$_hb_messages" 2>/dev/null || true
                        _consec_fail=0
                        sleep "$_backoff_sec" || exit 0
                        continue
                        ;;
                    esac
                    local _evict_marker; _evict_marker=$(printf '{"from":"airc","ts":"%s","channel":"%s","msg":"[HOST EVICTED] heartbeat to gist %s failed %d consecutive times — self-healing. cause: %s"}' \
                      "$_hb_now" "$_hb_room" "$_gist_id" "$_consec_fail" "$_classified")
                    echo "$_evict_marker" >> "$_hb_messages" 2>/dev/null || true
                    # Drop the stale local-state files so the parent's
                    # next discovery re-elects via _mesh_find.
                    rm -f "$_hb_state_dir/host_gist_id" "$_hb_state_dir/room_gist_id" 2>/dev/null
                    printf 'heartbeat failure: %s\n' "$_classified" > "$_hb_state_dir/airc.restart-request" 2>/dev/null || true
                    # SIGTERM the parent — its EXIT trap will reap
                    # children + clean up. The user-facing recovery is
                    # to run `airc join` again in the same scope.
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
            # waits a jitter, then resolves the canonical gist for this
            # channel using the same content-based resolver as connect.
            # Description-only winner election can yield to unrelated
            # live test gists and split the mesh.
            local _race="winner"
            if [ -z "${_existing_room_gid:-}" ]; then
              _race=$(_mesh_take_over "" "$_gist_id" "$room_name")
            fi
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
            echo "  Other agents on your gh account auto-join via:  airc join"
            echo "  Cross-account share (rare):"
            echo "    airc join $_gist_id"
            [ -n "$_hh" ] && echo "      # mnemonic: $_hh"
            echo "    airc join $_invite_long"
            echo ""
            echo "  (Room gist: $_gist_url — persistent; deleted on 'airc part'.)"
          else
            echo "  On the other machine (pick whichever is easiest to share):"
            echo ""
            echo "    airc join $_gist_id"
            [ -n "$_hh" ] && echo "      # mnemonic: $_hh"
            echo "    airc join $_invite_long"
            echo ""
            echo "  (Gist: $_gist_url — secret, single-use; delete after pairing.)"
          fi
        else
          echo ""
          echo "  ⚠  Gist push failed (gh auth?). Falling back to long invite:"
          if [ "$_printed_long" = "0" ]; then
            echo "    airc join $_invite_long"
          fi
        fi
        rm -rf "$_gist_tmpdir"
      fi
    fi
    echo ""
    echo "  Catch up unread messages with: airc inbox"
    echo "  Waiting for peers on port $host_port..."
    # Background: accept peer registrations via TCP (public keys only).
    #
    # Parent-watch (#132): the loop exits when its own parent disappears
    # (PPID=1 = reparented to init = airc parent bash died). Without
    # this, the loop survives terminal close / Monitor tool teardown /
    # kill of the parent, keeps spawning fresh listeners, and
    # every joiner that hits the cached port gets a real-looking pair
    # handshake against a ghost host. The Rust listener also receives
    # --watch-pid, so in-flight accepts exit when the parent dies.
    _orphan_parent_pid=$$
    (
      # Loop while the airc parent bash is still alive. kill -0 is the
      # cheapest "is PID still running" probe (no signal sent, just an
      # error if the process is gone). When the parent dies, this exits
      # before the next iteration so no fresh listener is spawned.
      #
      # --watch-pid hands the same PID to the Rust listener, which exits
      # during accept polling the moment the parent dies.
      while kill -0 "$_orphan_parent_pid" 2>/dev/null; do
        "$(airc_rs_bin)" handshake accept-one \
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
    # listener and the heartbeat loop. The hosted room gist is durable
    # channel identity; stale host leases are recovered in-place. Single
    # canonical trap (was previously
    # split between this site + the gist-publish site, but bash traps are
    # last-set-wins per shell so the split lost the gist-cleanup half).
    trap '
      _exit_hb_pid=""
      _exit_gist_id=""
      [ -f "$AIRC_WRITE_DIR/heartbeat.pid" ] && _exit_hb_pid=$(cat "$AIRC_WRITE_DIR/heartbeat.pid" 2>/dev/null)
      [ -f "$AIRC_WRITE_DIR/host_gist_id" ] && _exit_gist_id=$(cat "$AIRC_WRITE_DIR/host_gist_id" 2>/dev/null)
      [ -n "$_exit_hb_pid" ] && kill $_exit_hb_pid 2>/dev/null
      _exit_restart=0
      if [ -f "$AIRC_WRITE_DIR/airc.restart-request" ]; then
        _exit_restart=99
        _exit_reason=$(cat "$AIRC_WRITE_DIR/airc.restart-request" 2>/dev/null | head -1)
        echo "airc: restart requested (${_exit_reason:-internal transition})" >&2
        rm -f "$AIRC_WRITE_DIR/airc.restart-request" 2>/dev/null
      fi
      # Room gists are durable channel identity. Normal host exit must
      # leave the gist in place so another peer can refresh the host
      # lease in-place. `airc part` is the explicit deletion path.
      rm -f "$AIRC_WRITE_DIR/airc.pid" "$AIRC_WRITE_DIR/heartbeat.pid" "$AIRC_WRITE_DIR/host_gist_id" 2>/dev/null
      for p in $PAIR_PID $(proc_children $PAIR_PID) $(proc_children $$); do
        kill $p 2>/dev/null
      done
      [ "$_exit_restart" = "99" ] && exit 99
    ' EXIT INT TERM

    _join_phase "subscribing to #general (host-mode sidecar)"
    spawn_general_sidecar_if_wanted
    _join_emit_join_events "$name"
    _join_phase "monitor stream attached — cold start complete"
    _join_phase_done
    echo "  Monitoring for messages..."
    monitor
    kill $PAIR_PID 2>/dev/null
  fi
}
