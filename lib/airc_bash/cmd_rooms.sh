# Sourced by airc. Channel/peer cluster — IRC-style channel + peer ops.
#
# Functions exported back to airc's dispatch:
#   cmd_rooms      — list open airc invite gists on this gh account.
#                    The gist namespace IS the room registry; this is
#                    the /list verb. Walks the gist API, filters for
#                    `airc invite for ` description prefix, pretty-prints.
#   cmd_part       — leave the current room. If we're the host, deletes
#                    the room gist (channel dissolves). If we're a
#                    joiner, just local teardown. Records parted_rooms
#                    so re-join doesn't auto-resume.
#   cmd_send_file  — host-mediated file transfer to a peer. Pre-pairing-
#                    aware: writes to the host's files/<peer>/ dir.
#   cmd_invite     — print the long join string for cross-account share
#                    (the historical fallback when gist isn't reachable).
#   cmd_peers      — list paired peers in the current scope, with
#                    last-seen + role/status from peer files.
#
# External cross-references (call-time): die, ensure_init, get_config_val,
# set_config_val, unset_config_keys, get_host, resolve_name, relay_ssh,
# remote_home, AIRC_HOME, AIRC_WRITE_DIR, AIRC_PYTHON, plus cmd_teardown
# (which cmd_part calls to do the actual local kill).
#
# Extracted from airc as part of #152 Phase 3 file split. Bundled because
# in IRC mental model these are all the same conceptual surface: "what
# rooms exist? who's in this one? how do I leave/invite/transfer?" One
# domain = one file.

# ── cmd_rooms: list open airc invite gists on this gh account ────────
# Issue #38. The gist namespace IS the room registry — every airc invite
# pushed via the default gist transport (#37) shows up here. Filter is
# the description prefix `"airc invite for "` that push-image side writes.
#
# The Claude Code skill (/list, /rooms) calls this and lets the AI use
# conversation context to pick. The CLI itself stays orthogonal — it
# emits the menu, doesn't decide.
cmd_rooms() {
  # Parse flags (#142). Default hides items already marked stale (older
  # than the threshold in _is_stale) so an active user with several
  # rooms + several days of test runs doesn't have stale-invite count
  # dominating the active-rooms count. --all / --include-stale shows
  # everything (the pre-#142 behavior); --prune deletes stale gists.
  local include_stale=0
  local prune=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --all|--include-stale) include_stale=1; shift ;;
      --prune) prune=1; include_stale=1; shift ;;
      -h|--help)
        echo "Usage: airc list [--all|--include-stale] [--prune]"
        echo "  --all / --include-stale  show stale items (default: hidden)"
        echo "  --prune                  delete stale gists from your gh account"
        return 0 ;;
      *) echo "  Unknown flag: $1 (try: airc list --help)" >&2; return 1 ;;
    esac
  done

  if ! command -v gh >/dev/null 2>&1; then
    echo "  airc rooms requires the 'gh' CLI: https://cli.github.com" >&2
    echo "  airc IS aIRC — github gist is the coordination layer; gh is mandatory." >&2
    return 1
  fi
  # Match the new mesh gist (one per gh account, description "airc mesh"),
  # plus legacy per-room gists (`airc room:`) for accounts that haven't
  # rolled over yet, plus single-pair invites (`airc invite for`) for
  # cross-account ad-hoc pairing.
  # gh gist list columns: id  description  files  visibility  updated_at
  local raw; raw=$(gh gist list --limit 50 2>/dev/null \
    | awk -F'\t' '
        /airc mesh/         { print "mesh\t"   $1 "\t" $2 "\t" $5 }
        /airc room:/        { print "room\t"   $1 "\t" $2 "\t" $5 }
        /airc invite for/   { print "invite\t" $1 "\t" $2 "\t" $5 }
      ')
  local count; count=$(printf '%s' "$raw" | grep -c . || true)
  if [ "$count" = "0" ]; then
    echo "  No open airc rooms or invites on your gh account."
    echo "  Host the default room:  airc connect"
    echo "  Host a named room:      airc connect --room <name>"
    return 0
  fi
  # First pass: count how many are stale vs fresh, so we can show an
  # accurate header AND a hint about --all when items got hidden.
  local stale_count=0 fresh_count=0
  while IFS=$'\t' read -r _kind _id _desc updated; do
    [ -z "$_kind" ] && continue
    if _is_stale "$updated"; then
      stale_count=$((stale_count + 1))
    else
      fresh_count=$((fresh_count + 1))
    fi
  done <<< "$raw"

  echo ""
  if [ "$include_stale" = "1" ]; then
    echo "  $count open on your gh account ($fresh_count active, $stale_count stale):"
  elif [ "$stale_count" -gt 0 ]; then
    echo "  $fresh_count active on your gh account ($stale_count stale hidden — see 'airc list --all')"
  else
    echo "  $count open on your gh account:"
  fi
  echo ""

  local pruned=0
  while IFS=$'\t' read -r kind id desc updated; do
    [ -z "$kind" ] && continue
    local is_stale=0
    _is_stale "$updated" && is_stale=1
    # Default: skip stale entries. --all/--include-stale shows all.
    if [ "$is_stale" = "1" ] && [ "$include_stale" = "0" ]; then
      continue
    fi
    if [ "$prune" = "1" ] && [ "$is_stale" = "1" ]; then
      if gh gist delete "$id" --yes >/dev/null 2>&1; then
        echo "    pruned: $desc (id: $id)"
        pruned=$((pruned + 1))
      else
        echo "    prune FAILED for $desc (id: $id)" >&2
      fi
      continue
    fi
    local hh; hh=$(humanhash "$id" 2>/dev/null)
    local marker
    case "$kind" in
      mesh)   marker="◆" ;;     # mesh singleton (one per gh account)
      room)   marker="#" ;;     # legacy persistent per-room channel
      invite) marker="(1:1)" ;; # ephemeral cross-account pairing
    esac
    local age_str; age_str=$(_format_relative_time "$updated")
    local stale_marker=""
    [ "$is_stale" = "1" ] && stale_marker="  (stale)"
    printf '    %s %s%s\n      id:       %s\n      mnemonic: %s\n      updated:  %s\n\n' \
      "$marker" "$desc" "$stale_marker" "$id" "$hh" "$age_str"
  done <<< "$raw"

  if [ "$prune" = "1" ]; then
    echo "  pruned $pruned stale gist(s)."
    return 0
  fi
  echo "  Join (auto-resolves on same gh account): airc connect"
  echo "  Join by id (cross-account share):        airc connect <id>"
  echo ""
}

# Convert an ISO 8601 timestamp into a relative-time string ("12m ago",
# "3h ago", "2d ago"). Falls back to the raw timestamp on parse failure.
# Used by cmd_rooms to display gist activity (#82). Date parsing goes
# through iso_to_epoch so the BSD/GNU/python fallback chain is shared.
_format_relative_time() {
  local ts="${1:-}"
  [ -z "$ts" ] && { echo "(unknown)"; return; }
  local epoch; epoch=$(iso_to_epoch "$ts")
  if [ -z "$epoch" ]; then echo "$ts"; return; fi
  local now; now=$(date -u +%s)
  local diff=$((now - epoch))
  if [ "$diff" -lt 0 ]; then echo "$ts"; return; fi
  if [ "$diff" -lt 60 ]; then       echo "${diff}s ago"
  elif [ "$diff" -lt 3600 ]; then   echo "$((diff / 60))m ago"
  elif [ "$diff" -lt 86400 ]; then  echo "$((diff / 3600))h ago"
  else                              echo "$((diff / 86400))d ago"
  fi
}

# Return 0 if the given ISO timestamp is older than AIRC_STALE_HOURS
# (default 24h). Used to mark abandoned rooms in cmd_rooms output (#82).
# Shares iso_to_epoch with _format_relative_time so a future date-parse
# fix lands once.
_is_stale() {
  local ts="${1:-}"
  local threshold_hours="${AIRC_STALE_HOURS:-24}"
  [ -z "$ts" ] && return 1
  local epoch; epoch=$(iso_to_epoch "$ts")
  [ -z "$epoch" ] && return 1
  local now; now=$(date -u +%s)
  local diff=$((now - epoch))
  [ "$diff" -gt $((threshold_hours * 3600)) ]
}

# ── cmd_part: leave the current room ──────────────────────────────────
# Issue #39. Two paths, distinguished by config.json's host_target:
#   - Host (no host_target): delete the room gist if we created one, then
#     teardown. Joiners watching us will see SSH die — IRC's "ircd
#     restart" — and the next reconnect re-elects a new host.
#   - Joiner (host_target set): just teardown local processes; host's
#     gist stays open for other joiners (we're one of N).
# Either way, local config + identity + peer records persist (use
# `airc teardown --flush` for nuclear).
#
# Detection note: we use config.json::host_target as the host-vs-joiner
# signal, NOT presence of room_gist_id. The gist file may be absent for
# a legitimate host case (`--no-gist`, or gh push failed) — falling back
# to "you're a joiner" would be wrong.
cmd_part() {
  ensure_init

  local gist_id_file="$AIRC_WRITE_DIR/room_gist_id"
  local room_name_file="$AIRC_WRITE_DIR/room_name"
  local room_name="(unnamed)"
  [ -f "$room_name_file" ] && room_name=$(cat "$room_name_file")

  local host_target; host_target=$(get_config_val host_target "")

  if [ -z "$host_target" ]; then
    # ── Host path ──
    if [ -f "$gist_id_file" ]; then
      local gid; gid=$(cat "$gist_id_file")
      if command -v gh >/dev/null 2>&1; then
        echo "  Host of #${room_name} parting — deleting room gist ${gid}..."
        gh gist delete "$gid" --yes 2>/dev/null \
          && echo "  ✓ Room gist deleted." \
          || echo "  ⚠  Couldn't delete gist ${gid} (already gone? gh auth?). Continuing teardown."
      else
        echo "  ⚠  gh CLI not available — can't delete room gist ${gid} automatically."
        echo "     Delete it manually:  gh gist delete ${gid} --yes"
      fi
    else
      # Host but no gist (--no-gist or gh-push failed). Nothing to delete
      # in the gh namespace; just clean local state.
      echo "  Host of #${room_name} parting (no gist was published; nothing to clean up in gh)."
    fi
    rm -f "$gist_id_file" "$room_name_file"
  else
    # ── Joiner path ──
    echo "  Joiner of #${room_name} parting — host's gist stays open for others."
    # Clear our cached gist_id too, matching the comment on the joiner-
    # side cache write site (PR #92 Copilot feedback). Without this, a
    # parted joiner that later reconnects via the same scope would
    # incorrectly trigger the stale-pairing-detect path on the next
    # resume even though they parted intentionally.
    rm -f "$room_name_file" "$gist_id_file"
  fi

  # Issue #136: persist the /part. Record the room into the PRIMARY
  # scope's parted_rooms list so a later `airc join` won't auto-
  # resubscribe. Only meaningful for sidecar rooms (general, future
  # opt-in #repo etc.) — parting your project's primary scope means
  # the whole scope is gone, so persistence there is moot.
  local _primary_scope; _primary_scope=$(_primary_scope_for "$AIRC_WRITE_DIR")
  if [ "$_primary_scope" != "$AIRC_WRITE_DIR" ] && [ "$room_name" != "(unnamed)" ]; then
    _record_parted_room "$_primary_scope" "$room_name"
    echo "  /part persisted — #${room_name} won't auto-resubscribe. Rejoin with: airc join --${room_name}"
  fi

  # Phase 2B.2: also remove the parted channel from this scope's
  # subscribed_channels list. cmd_send won't pick it as default
  # anymore, the monitor display drops the channel prefix from
  # outbound, and a future cmd_join --room <name> re-adds it.
  if [ "$room_name" != "(unnamed)" ]; then
    "$AIRC_PYTHON" -m airc_core.config unsubscribe \
      --config "$CONFIG" --channel "$room_name" 2>/dev/null || true
  fi

  # IRC `/part` semantics — leave THIS room only; the #general sidecar
  # (or any other sibling subscription) keeps running. cmd_teardown
  # respects AIRC_TEARDOWN_PART_ONLY=1 by skipping its sidecar block,
  # so the kill is scope-local. cmd_teardown without this guard remains
  # the "kill everything in this scope tree" command.
  local AIRC_TEARDOWN_PART_ONLY=1
  cmd_teardown
}

cmd_send_file() {
  local peer_name="${1:-}" filepath="${2:-}"
  [ -z "$peer_name" ] || [ -z "$filepath" ] && die "Usage: airc send-file <peer> <path>"
  [ -f "$filepath" ] || die "File not found: $filepath"
  ensure_init

  local host_target my_name
  host_target=$(get_config_val host_target "")
  my_name=$(get_name)

  local filename; filename=$(basename "$filepath")
  local target_host="$host_target"
  [ -z "$target_host" ] && target_host="localhost"

  local rhome; rhome=$(remote_home)
  relay_ssh "$target_host" "mkdir -p $rhome/files/${my_name}" 2>/dev/null
  # Use the airc identity key for scp — same key relay_ssh uses. Without -i,
  # scp falls back to system ssh_config (~/.ssh/id_* etc), which doesn't know
  # about isolated AIRC_HOME identities. Surfaced by m5-test's send-file test.
  local ssh_key="$IDENTITY_DIR/ssh_key"
  local scp_out
  if [ -f "$ssh_key" ]; then
    scp_out=$(scp -i "$ssh_key" -o StrictHostKeyChecking=accept-new -q "$filepath" "${target_host}:${rhome}/files/${my_name}/${filename}" 2>&1)
  else
    scp_out=$(scp -o StrictHostKeyChecking=accept-new -q "$filepath" "${target_host}:${rhome}/files/${my_name}/${filename}" 2>&1)
  fi
  if [ $? -ne 0 ]; then
    die "Failed to transfer $filename: $scp_out"
  fi

  local filesize; filesize=$(file_size "$filepath")
  cmd_send "$peer_name" "Sent file: $filename ($filesize bytes)"
  echo "Sent $filename ($filesize bytes)"
}

cmd_invite() {
  ensure_init
  local host_target pubkey_b64 join_string
  host_target=$(get_config_val host_target "")

  if [ -n "$host_target" ]; then
    # Joiner: reconstruct the HOST's join string from stored pairing info.
    # Any connected peer can share the same join string — everyone converges
    # on the same host.
    local host_name host_port host_ssh_pub
    host_name=$(get_config_val host_name "")
    host_port=$(get_config_val host_port 7547)
    host_ssh_pub=$(get_config_val host_ssh_pub "")
    if [ -z "$host_name" ] || [ -z "$host_ssh_pub" ]; then
      die "Host info missing from config. Re-pair with 'airc teardown' then 'airc connect <join-string>'."
    fi
    pubkey_b64=$(printf '%s\n' "$host_ssh_pub" | base64 | tr -d '\n')
    local port_suffix=""
    [ "$host_port" != "7547" ] && port_suffix=":$host_port"
    join_string="${host_name}@${host_target}${port_suffix}#${pubkey_b64}"
  else
    # Host: build own join string from live state.
    local my_name user host port
    my_name=$(get_name)
    user=$(whoami)
    host=$(get_host)
    port=$(cat "$AIRC_WRITE_DIR/host_port" 2>/dev/null || echo 7547)
    local port_suffix=""
    [ "$port" != "7547" ] && port_suffix=":$port"
    pubkey_b64=$(base64 < "$IDENTITY_DIR/ssh_key.pub" | tr -d '\n')
    join_string="${my_name}@${user}@${host}${port_suffix}#${pubkey_b64}"
  fi

  echo "$join_string"
}

cmd_peers() {
  ensure_init
  # `airc peers --prune` — remove stale records that share a host with a
  # newer record (cruft left from rename chain-breaks before the stable-host
  # matching logic landed).
  if [ "${1:-}" = "--prune" ]; then
    "$AIRC_PYTHON" -c "
import json, os, sys
peers_dir = os.path.expanduser('$PEERS_DIR')
if not os.path.isdir(peers_dir):
    sys.exit(0)
# Group records by host; keep the most-recently-paired, remove the rest.
by_host = {}
for entry in sorted(os.listdir(peers_dir)):
    if not entry.endswith('.json'): continue
    p = os.path.join(peers_dir, entry)
    try:
        d = json.load(open(p))
    except Exception:
        continue
    host = d.get('host', '')
    if not host: continue
    by_host.setdefault(host, []).append((d.get('paired', ''), entry, d.get('name', entry[:-5])))
removed = []
for host, records in by_host.items():
    if len(records) < 2: continue
    records.sort(reverse=True)  # newest paired first
    for _, entry, name in records[1:]:
        for ext in ('.json', '.pub'):
            f = os.path.join(peers_dir, entry[:-5] + ext)
            if os.path.isfile(f):
                try: os.remove(f)
                except Exception: pass
        removed.append((name, host))
if removed:
    for name, host in removed:
        print(f'  pruned: {name} -> {host}')
else:
    print('  No stale records to prune.')
"
    return
  fi

  # Walk scopes that count as "subscribed rooms" for this tab: primary
  # (current AIRC_WRITE_DIR) plus any sibling sidecar scopes (.airc.<room>
  # pattern under the project scope's parent). For each, read peers/
  # records and annotate with the scope's room_name. Same peer in both
  # scopes folds into one line with both room tags.
  #
  # Intent (issue #121 follow-up): multi-room presence shouldn't fragment
  # the operator's view of "who am I connected to" into separate per-scope
  # listings. From the user's perspective they're in N rooms; airc peers
  # should reflect that as one unified roster with room context per peer.
  "$AIRC_PYTHON" -c "
import json, os, sys, re

primary_scope = os.path.expanduser('$AIRC_WRITE_DIR')
parent = os.path.dirname(primary_scope)
self_basename = os.path.basename(primary_scope)

# Prefix detection: a sidecar scope is named like \`<prefix>.<room>\`
# (e.g. .airc.general). Strip a trailing .<word> to recover the
# primary scope's basename. Works for both production layout
# (.airc / .airc.general) and test ad-hoc paths (state / state.general)
# without baking in the .airc literal.
prefix_match = re.match(r'(.+?)\.[a-z0-9-]+\$', self_basename)
prefix = prefix_match.group(1) if prefix_match else self_basename

# Collect: the primary scope itself, plus every sibling whose name is
# <prefix>.<something>. We additionally require room_name + peers/ on
# each candidate so unrelated dirs in the same parent (e.g. .airc-old,
# .airc.bak) don't pollute the listing.
candidates = []
if os.path.isdir(parent):
    for entry in sorted(os.listdir(parent)):
        if entry == prefix or entry.startswith(prefix + '.'):
            candidates.append(os.path.join(parent, entry))
scopes = [s for s in candidates
          if os.path.isfile(os.path.join(s, 'room_name'))
          and os.path.isdir(os.path.join(s, 'peers'))]
# Always include primary even if it doesn't have room_name yet — that's
# the legacy 1:1 invite mode case (use_room=0).
if primary_scope not in scopes and os.path.isdir(os.path.join(primary_scope, 'peers')):
    scopes.insert(0, primary_scope)

# Build {(name, host): [room1, room2, ...]} by walking each scope's peers/.
peers_by_id = {}
for scope in scopes:
    peers_dir = os.path.join(scope, 'peers')
    if not os.path.isdir(peers_dir):
        continue
    rn_file = os.path.join(scope, 'room_name')
    room = '(?)'
    if os.path.isfile(rn_file):
        try: room = open(rn_file).read().strip()
        except Exception: pass
    for f in sorted(os.listdir(peers_dir)):
        if not f.endswith('.json'): continue
        try:
            d = json.load(open(os.path.join(peers_dir, f)))
        except Exception:
            continue
        key = (d.get('name', f[:-5]), d.get('host', ''))
        peers_by_id.setdefault(key, []).append(room)

if not peers_by_id:
    print('  No peers yet.')
    sys.exit(0)

# Render. Each peer once, with room annotations sorted + deduped.
for (name, host), rooms in sorted(peers_by_id.items()):
    seen = set(); ordered = []
    for r in rooms:
        if r not in seen:
            ordered.append(r); seen.add(r)
    tags = ', '.join('#' + r for r in ordered)
    print(f'  {name} → {host}   [{tags}]')
"
}
