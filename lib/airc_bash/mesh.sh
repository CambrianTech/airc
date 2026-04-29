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
# If the listing returns 2+ candidates (race-loser collision, gh
# replication lag, or an old per-room gist incorrectly tagged), keep
# the OLDEST by created_at. The oldest is the legitimate winner of
# any post-publish race because it was created first; any other entry
# is a duplicate that should be reaped on the next takeover cycle.
_mesh_find() {
  command -v gh >/dev/null 2>&1 || return 0
  local desc; desc=$(_mesh_desc)
  # gh gist list output: <id>\t<desc>\t<files>\t<visibility>\t<updated>
  # Filter on EXACT desc match (anchor with ^ and $ in awk).
  local ids
  ids=$(gh gist list --limit 50 2>/dev/null \
    | awk -F'\t' -v d="$desc" '$2 == d { print $1 }')
  local count; count=$(printf '%s\n' "$ids" | grep -c . || true)
  case "$count" in
    0) return 0 ;;
    1) printf '%s\n' "$ids" ;;
    *)
      # Multiple matches — pick the oldest by created_at. Same tiebreaker
      # cmd_connect's race-loser detection uses; centralized here.
      local oldest="" oldest_ts=""
      while IFS= read -r gid; do
        [ -z "$gid" ] && continue
        local ts; ts=$(gh api "gists/$gid" --jq '.created_at' 2>/dev/null || echo "")
        if [ -z "$oldest_ts" ] || [ "$ts" \< "$oldest_ts" ]; then
          oldest="$gid"; oldest_ts="$ts"
        fi
      done <<< "$ids"
      [ -n "$oldest" ] && printf '%s\n' "$oldest"
      ;;
  esac
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
  [ -n "$gist_id" ] || return 0
  command -v gh >/dev/null 2>&1 || return 0
  local content; content=$(gh api "gists/$gist_id" --jq '.files | to_entries[0].value.content' 2>/dev/null || true)
  [ -z "$content" ] && return 0
  local hb; hb=$(printf '%s' "$content" | "$AIRC_PYTHON" -c '
import sys, json
try:
    print(json.loads(sys.stdin.read()).get("last_heartbeat", ""))
except Exception:
    pass
' 2>/dev/null || true)
  [ -z "$hb" ] && return 0
  local hb_epoch; hb_epoch=$("$AIRC_PYTHON" -m airc_core.datetime iso_to_epoch "$hb" 2>/dev/null || true)
  [ -z "$hb_epoch" ] && return 0
  local now_epoch; now_epoch=$(date -u +%s)
  echo $(( now_epoch - hb_epoch ))
}

# Race-aware takeover. Inputs: $1 = stale gist id we want to replace.
# Caller has already PUBLISHED their own replacement (returned id in $2)
# and is checking whether they actually won the race.
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
#   3. List all mesh gists. If only ours is left, we won.
#   4. If multiple, pick the OLDEST by created_at as winner. If that's
#      ours, we won. Else echo "loser:<winner_id>".
_mesh_take_over() {
  local stale_id="${1:-}" my_id="${2:-}"
  [ -n "$my_id" ] || return 1
  command -v gh >/dev/null 2>&1 || return 1
  if [ -n "$stale_id" ] && [ "$stale_id" != "$my_id" ]; then
    gh gist delete "$stale_id" --yes >/dev/null 2>&1 || true
  fi
  # Jitter: 200..1200ms. Spreads races so all tabs see the same listing.
  local jitter; jitter=$(awk -v r="$RANDOM" 'BEGIN{printf "%.3f", 0.2 + (r%1000)/1000}')
  sleep "$jitter"
  local desc; desc=$(_mesh_desc)
  local ids; ids=$(gh gist list --limit 50 2>/dev/null \
    | awk -F'\t' -v d="$desc" '$2 == d { print $1 }')
  local count; count=$(printf '%s\n' "$ids" | grep -c . || true)
  if [ "$count" -le 1 ]; then
    echo "winner"
    return 0
  fi
  # Multiple — pick oldest by created_at.
  local oldest="" oldest_ts=""
  while IFS= read -r gid; do
    [ -z "$gid" ] && continue
    local ts; ts=$(gh api "gists/$gid" --jq '.created_at' 2>/dev/null || echo "")
    if [ -z "$oldest_ts" ] || [ "$ts" \< "$oldest_ts" ]; then
      oldest="$gid"; oldest_ts="$ts"
    fi
  done <<< "$ids"
  if [ "$oldest" = "$my_id" ]; then
    echo "winner"
  else
    echo "loser:$oldest"
  fi
}
