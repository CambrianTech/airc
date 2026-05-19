# Sourced by airc. Mesh gist abstraction — ONE gist per gh account.
#
# Architectural shift from the per-room model. Joel 2026-04-27:
#   "B is the god damn correct shift. They all share the same gist,
#   stop being stupid. one goes down first one to resolve, posts to
#   gist. host goes down, new host, first one that posts, becomes it."
#
# Old model: every `airc join --room X` created a gist `airc room: X`,
# with its own host process and own port. A user with 3 projects had
# 3 independent host processes on 3 ports + 3 gists.
#
# New model: ONE gist per gh account, description literal `airc mesh`.
# Every `airc join` on the account converges on it. Channels are
# advisory tags inside the gist envelope (and in Phase 2, on each
# message). One host machine, one sshd, one mesh.
#
# This file holds the primitives — lookup, publish, update, takeover.
# cmd_connect.sh calls these instead of inline `gh gist create -d "airc
# room:..."`. Single source of truth for the gist description literal,
# the singleton-lookup contract, and race-loser semantics.
#
# Conventions:
#   - All functions echo their result to stdout (one line) or stay silent.
#   - Errors print to stderr; non-zero exit only when the operation
#     genuinely failed (gh missing, network down, auth lapsed). Empty
#     stdout + zero exit = "ran cleanly, found nothing."
#   - Functions are pure stateless wrappers around gh + jq/awk; no
#     side effects on local files. cmd_connect.sh keeps state.

# Canonical gist description. Every site that creates, lists, or
# matches the mesh gist routes through this — change once, change
# everywhere.
_mesh_desc() {
  echo "airc mesh"
}

# Singleton lookup: find the mesh gist on the current gh account.
# Echoes the gist id (one line) or empty.
#
# Production invariant: the gist envelope content is the source of
# truth, not the human description. Pre-fix this matched only gists
# whose description was exactly "airc mesh"; meanwhile
# airc_core.channel_gist.resolve found older "airc room: ..." gists
# by envelope content. Two discovery systems, two answers, split-brain.
#
# Delegate lookup to channel_gist.find_existing so connect, subscribe,
# send, and rediscovery all use the same canonical channel→gist rule.
# Optional arg = channel name. Empty falls back to cmd_connect's dynamic
# room_name, then config's default channel, then #general.
_mesh_find() {
  command -v gh >/dev/null 2>&1 || return 0
  local channel="${1:-${room_name:-}}"
  if [ -z "$channel" ] && [ -n "${CONFIG:-}" ] && [ -f "$CONFIG" ]; then
    channel=$(airc_config_default_channel "$CONFIG" || true)
  fi
  [ -z "$channel" ] && channel="general"
  if [ -n "${CONFIG:-}" ] && [ -f "$CONFIG" ]; then
    local configured
    configured=$(airc_config_get_channel_gist "$channel" "$CONFIG" || true)
    if [ -n "$configured" ]; then
      printf '%s\n' "$configured"
      return 0
    fi
  fi
  "$AIRC_PYTHON" -m airc_core.channel_gist find --channel "$channel" --require-invite 2>/dev/null || true
}

# Find the canonical channel gist whether or not it currently has a host
# invite. This is the durable room identity lookup. Zero-arg discovery
# uses it to decide whether to host/adopt the existing chain instead of
# being attracted to a newer invite-bearing solo island.
_mesh_find_any() {
  command -v gh >/dev/null 2>&1 || return 0
  local channel="${1:-${room_name:-}}"
  if [ -z "$channel" ] && [ -n "${CONFIG:-}" ] && [ -f "$CONFIG" ]; then
    channel=$(airc_config_default_channel "$CONFIG" || true)
  fi
  [ -z "$channel" ] && channel="general"
  if [ -n "${CONFIG:-}" ] && [ -f "$CONFIG" ]; then
    local configured
    configured=$(airc_config_get_channel_gist "$channel" "$CONFIG" || true)
    if [ -n "$configured" ]; then
      printf '%s\n' "$configured"
      return 0
    fi
  fi
  "$AIRC_PYTHON" -m airc_core.channel_gist find --channel "$channel" 2>/dev/null || true
}

# Publish a new mesh gist. Echoes the new gist id, or empty on failure.
# Caller writes the JSON envelope to a tempfile and passes the path.
# Per CLAUDE.md "never swallow errors": gh's stderr (auth lapsed, rate
# limited, etc) reaches the terminal so failures are diagnosable
# instead of "create returned empty, no idea why."
_mesh_publish() {
  local payload_path="${1:-}"
  [ -f "$payload_path" ] || return 1
  command -v gh >/dev/null 2>&1 || return 1
  local desc; desc=$(_mesh_desc)
  local url; url=$(gh gist create -d "$desc" "$payload_path" | tail -1)
  [ -z "$url" ] && return 1
  printf '%s\n' "${url##*/}"
}

# Update an existing mesh gist with a new payload. Used by the heartbeat
# loop. Returns 0 on success, non-zero if the gist is gone or auth lapsed.
# Caller passes the gist_id and a path to the new JSON envelope. The
# basename of payload_path MUST match the canonical in-gist filename
# (e.g. airc-room-<channel>.json) — gh disambiguates targets by
# basename, and a mismatched basename on a multi-file gist surfaces
# as "unsure what file to edit" with non-zero exit. Per CLAUDE.md
# "never swallow errors", stderr propagates to the terminal so the
# next debugger sees the actual failure.
_mesh_update() {
  local gist_id="${1:-}" payload_path="${2:-}"
  [ -n "$gist_id" ] || return 1
  [ -f "$payload_path" ] || return 1
  command -v gh >/dev/null 2>&1 || return 1
  gh gist edit "$gist_id" "$payload_path" >/dev/null
}

# Echo the seconds since last_heartbeat in the given mesh gist. Empty
# (and zero exit) on any failure — caller treats empty as "can't tell,
# assume stale" or "assume fresh" depending on policy.
_mesh_age_secs() {
  local gist_id="${1:-}"
  local channel="${2:-${room_name:-}}"
  [ -n "$gist_id" ] || return 0
  command -v gh >/dev/null 2>&1 || return 0
  local content
  if [ -n "$channel" ]; then
    content=$(gh api "gists/$gist_id" 2>/dev/null \
      | "$(airc_rs_bin)" gist gist-content --channel "$channel" 2>/dev/null || true)
  else
    content=$(gh api "gists/$gist_id" 2>/dev/null \
      | "$(airc_rs_bin)" gist gist-content 2>/dev/null || true)
  fi
  [ -z "$content" ] && return 0
  local hb; hb=$(printf '%s' "$content" | "$AIRC_PYTHON" -c '
import sys, json
try:
    print(json.loads(sys.stdin.read()).get("last_heartbeat", ""))
except Exception:
    pass
' 2>/dev/null || true)
  [ -z "$hb" ] && return 0
  local hb_epoch; hb_epoch=$(iso_to_epoch "$hb")
  [ -z "$hb_epoch" ] && return 0
  local now_epoch; now_epoch=$(date -u +%s)
  echo $(( now_epoch - hb_epoch ))
}

# Race-aware takeover. Inputs: $1 = stale gist id we want to replace,
# $2 = caller's newly-published gist id, $3 = channel name.
#
# Echoes one of:
#   "winner"   — caller's gist is the canonical mesh; old stale was
#                deleted, no other contenders.
#   "loser:<winner_id>"
#              — somebody else's publish is older; caller should delete
#                their own and rejoin pointed at <winner_id>.
#
# Algorithm:
#   1. Try to delete the stale gist (idempotent — another tab may have
#      gotten there first; treat that as success).
#   2. Light jitter so all racers see the same gh-side state.
#   3. Resolve the canonical gist for THIS channel via _mesh_find. If
#      the resolver says another gist is canonical, yield to it.
_mesh_take_over() {
  local stale_id="${1:-}" my_id="${2:-}" channel="${3:-${room_name:-}}"
  [ -n "$my_id" ] || return 1
  command -v gh >/dev/null 2>&1 || return 1
  if [ -n "$stale_id" ] && [ "$stale_id" != "$my_id" ]; then
    gh gist delete "$stale_id" --yes >/dev/null 2>&1 || true
  fi
  # Jitter: 200..1200ms. Spreads races so all tabs see the same listing.
  local jitter; jitter=$(awk -v r="$RANDOM" 'BEGIN{printf "%.3f", 0.2 + (r%1000)/1000}')
  sleep "$jitter"
  local winner
  winner=$(_mesh_find "$channel" 2>/dev/null || true)
  if [ -z "$winner" ] || [ "$winner" = "$my_id" ]; then
    echo "winner"
    return 0
  fi
  echo "loser:$winner"
}
