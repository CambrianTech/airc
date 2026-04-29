#!/usr/bin/env bash
# integration_smoke.sh — basic substrate end-to-end tests against REAL gh.
#
# These cover the gaps Joel called out 2026-04-29: "not stupid unit, not
# faked with some half assed local equivalent — what is THE most basic
# things, ping, new rooms, leaving, joining."
#
# Hard rules for scenarios in this file:
#   1. Use the REAL gh substrate (no --no-gist, no --no-room).
#   2. Spawn TWO real airc processes (one host + one joiner).
#   3. Assert user-visible outcomes (gist content, peer's local log,
#      bearer state file ticking) — not internal-call counts.
#   4. Clean up every created gist on exit (trap), even on failure.
#   5. Skip cleanly if `gh` is missing/unauthed (CI without secrets).
#
# Run all:  bash test/integration_smoke.sh
# Run one:  bash test/integration_smoke.sh passive_recv

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
AIRC="$REPO_ROOT/airc"

PASS=0; FAIL=0; SKIP=0
RED=$'\033[31m'; GRN=$'\033[32m'; YLO=$'\033[33m'; RST=$'\033[0m'
pass() { echo "  ${GRN}✓${RST} $1"; PASS=$((PASS+1)); }
fail() { echo "  ${RED}✗${RST} $1"; FAIL=$((FAIL+1)); }
skip() { echo "  ${YLO}↷${RST} $1"; SKIP=$((SKIP+1)); }
section() { echo; echo "${YLO}── $1 ──${RST}"; }

require_gh() {
  command -v gh >/dev/null 2>&1 || { skip "gh not installed"; return 1; }
  gh auth status >/dev/null 2>&1 || { skip "gh not authenticated"; return 1; }
  return 0
}

# Spawn a real airc instance. Args: home, name, port, extra-flags.
#
# Two modes (caller picks via the --as-host flag at end of args):
#   spawn_real $home $name $port [flags...] --as-host
#       AIRC_NO_DISCOVERY=1, becomes its own host of the named room.
#       Use for the FIRST peer in a test pair.
#   spawn_real $home $name $port [flags...]
#       Normal discovery; will find any existing mesh on this gh
#       account and join it. Use for the SECOND peer in a test pair.
#
# Waits up to 12s for either "Hosting as" / "Connected to" / "Joined".
# Cleanup is via cleanup_homes which reads channel_gists from the
# resulting state.
spawn_real() {
  local home="$1" name="$2" port="$3"; shift 3
  local as_host=0
  local args=()
  for a in "$@"; do
    if [ "$a" = "--as-host" ]; then as_host=1; else args+=("$a"); fi
  done
  mkdir -p "$home/state"
  if [ "$as_host" = "1" ]; then
    (
      cd "$home" \
        && AIRC_HOME="$home/state" AIRC_NAME="$name" AIRC_PORT="$port" \
           AIRC_NO_DISCOVERY=1 AIRC_NO_AUTO_ROOM=1 AIRC_NO_GENERAL=1 \
           AIRC_NO_IDENTITY_PROMPT=1 \
           "$AIRC" connect "${args[@]}" > "$home/out.log" 2>&1 &
    )
  else
    (
      cd "$home" \
        && AIRC_HOME="$home/state" AIRC_NAME="$name" AIRC_PORT="$port" \
           AIRC_NO_AUTO_ROOM=1 AIRC_NO_GENERAL=1 AIRC_NO_IDENTITY_PROMPT=1 \
           "$AIRC" connect "${args[@]}" > "$home/out.log" 2>&1 &
    )
  fi
  local i
  for i in $(seq 1 20); do
    sleep 1
    grep -qE 'Hosting as|Connected to|Joined' "$home/out.log" 2>/dev/null || continue
    # For hosts: also wait until config.json has a channel_gists entry,
    # i.e. the gist was actually published. Without this the next peer
    # spawned right after sees no mesh on the account and bootstraps as
    # its own host of the same room → two parallel gists, test fails.
    if [ "$as_host" = "1" ]; then
      python3 -c "
import json,sys
try:
    c = json.load(open('$home/state/config.json'))
    sys.exit(0 if c.get('channel_gists') else 1)
except Exception:
    sys.exit(1)
" 2>/dev/null && return 0
    else
      return 0
    fi
  done
  return 1
}

# Tear down all spawned airc instances + delete any gists they created.
cleanup_homes() {
  local home gids
  for home in "$@"; do
    [ -d "$home" ] || continue
    AIRC_HOME="$home/state" "$AIRC" teardown >/dev/null 2>&1 || true
    # Delete any gist this scope owned. config.channel_gists + room_gist_id.
    gids=$(python3 -c "
import json, sys
try:
    c = json.load(open('$home/state/config.json'))
    out = set(c.get('channel_gists', {}).values())
    for f in ['room_gist_id']:
        try: out.add(open('$home/state/'+f).read().strip())
        except Exception: pass
    print(' '.join(g for g in out if g))
except Exception:
    pass
" 2>/dev/null)
    local gid
    for gid in $gids; do
      gh gist delete "$gid" --yes 2>/dev/null || true
    done
    rm -rf "$home"
  done
}

# ─────────────────────────────────────────────────────────────────────
# scenario: passive_recv
#
# THE missing test (Joel 2026-04-29). Host A sends a message; joiner B's
# monitor receives via polling without B sending anything. Pre-fix this
# was the failure mode of every "monitor seems frozen" report — the
# polling pipeline was alive-but-stuck and nothing arrived in B's log.
# ─────────────────────────────────────────────────────────────────────
scenario_passive_recv() {
  section "passive_recv: A sends, B receives via polling (no send from B)"
  require_gh || return

  local rname="smoke-passive-$$"
  local A_HOME B_HOME
  A_HOME=$(mktemp -d -t airc-smoke-A.XXXXXX)
  B_HOME=$(mktemp -d -t airc-smoke-B.XXXXXX)
  trap "cleanup_homes '$A_HOME' '$B_HOME'" RETURN

  # A hosts a fresh custom room.
  spawn_real "$A_HOME" "smoke-A-$$" 7591 --room "$rname" --as-host \
    || { fail "A failed to start hosting"; return; }
  sleep 1
  pass "A hosting #$rname"

  local A_gid
  A_gid=$(python3 -c "import json;print(json.load(open('$A_HOME/state/config.json')).get('channel_gists',{}).get('$rname',''))" 2>/dev/null)
  [ -n "$A_gid" ] || { fail "A: no channel_gists['$rname']"; return; }

  # B joins via mesh discovery (same gh account auto-resolves).
  spawn_real "$B_HOME" "smoke-B-$$" 7592 --room "$rname" \
    || { fail "B failed to join"; return; }
  sleep 2
  pass "B joined the mesh"

  # A sends a marker. B should see it WITHOUT B sending anything.
  local marker="passive-marker-$(date +%s%N)"
  AIRC_HOME="$A_HOME/state" "$AIRC" msg --room "$rname" "$marker" >/dev/null 2>&1
  pass "A sent marker"

  # Wait for B's bearer to poll + monitor_formatter to mirror to local log.
  # Default poll is 15s; with bearer warmup + GET round-trip allow up to 60s.
  local i seen=0
  for i in $(seq 1 30); do
    sleep 2
    if grep -qF "$marker" "$B_HOME/state/messages.jsonl" 2>/dev/null; then
      seen=1; break
    fi
  done

  if [ "$seen" = "1" ]; then
    pass "B's local log received marker via polling (no send required)"
  else
    fail "B never saw marker in 30s — polling pipeline broken (the original bug class)"
    echo "    A out: $(tail -3 "$A_HOME/out.log")"
    echo "    B out: $(tail -3 "$B_HOME/out.log")"
    echo "    B bearer state: $(cat "$B_HOME/state/bearer_state."*.json 2>/dev/null | head -2)"
  fi
}

# ─────────────────────────────────────────────────────────────────────
# scenario: round_trip
#
# Both directions work. A → B then B → A, both via polling.
# ─────────────────────────────────────────────────────────────────────
scenario_round_trip() {
  section "round_trip: A→B and B→A both arrive via polling"
  require_gh || return

  local rname="smoke-rt-$$"
  local A_HOME B_HOME
  A_HOME=$(mktemp -d -t airc-smoke-rtA.XXXXXX)
  B_HOME=$(mktemp -d -t airc-smoke-rtB.XXXXXX)
  trap "cleanup_homes '$A_HOME' '$B_HOME'" RETURN

  spawn_real "$A_HOME" "smoke-rtA-$$" 7593 --room "$rname" --as-host \
    || { fail "A failed to host"; return; }
  spawn_real "$B_HOME" "smoke-rtB-$$" 7594 --room "$rname" \
    || { fail "B failed to join"; return; }
  sleep 2

  local m_a="rtA-$(date +%s%N)"
  local m_b="rtB-$(date +%s%N)"
  AIRC_HOME="$A_HOME/state" "$AIRC" msg --room "$rname" "$m_a" >/dev/null 2>&1
  AIRC_HOME="$B_HOME/state" "$AIRC" msg --room "$rname" "$m_b" >/dev/null 2>&1

  local i a_to_b=0 b_to_a=0
  for i in $(seq 1 30); do
    sleep 2
    grep -qF "$m_a" "$B_HOME/state/messages.jsonl" 2>/dev/null && a_to_b=1
    grep -qF "$m_b" "$A_HOME/state/messages.jsonl" 2>/dev/null && b_to_a=1
    [ "$a_to_b" = "1" ] && [ "$b_to_a" = "1" ] && break
  done

  [ "$a_to_b" = "1" ] && pass "A→B delivered" || fail "A→B not seen by B in 30s"
  [ "$b_to_a" = "1" ] && pass "B→A delivered" || fail "B→A not seen by A in 30s"
}

# ─────────────────────────────────────────────────────────────────────
# scenario: idle_then_recv
#
# Bearer-stuck detection (#312). Peer connects, sits idle for 45s
# (>1.5x default poll cadence), then peer-A sends — B must STILL
# receive. Pre-#312 stuck bearers silently broke after sleep / network
# blip; heartbeats keep the watchdog armed during legitimate idle.
# ─────────────────────────────────────────────────────────────────────
scenario_idle_then_recv() {
  section "idle_then_recv: bearer survives 45s idle, still receives"
  require_gh || return

  local rname="smoke-idle-$$"
  local A_HOME B_HOME
  A_HOME=$(mktemp -d -t airc-smoke-idleA.XXXXXX)
  B_HOME=$(mktemp -d -t airc-smoke-idleB.XXXXXX)
  trap "cleanup_homes '$A_HOME' '$B_HOME'" RETURN

  spawn_real "$A_HOME" "smoke-idleA-$$" 7595 --room "$rname" --as-host \
    || { fail "A failed to host"; return; }
  spawn_real "$B_HOME" "smoke-idleB-$$" 7596 --room "$rname" \
    || { fail "B failed to join"; return; }
  sleep 2

  pass "both peers up; idling 45s..."
  sleep 45

  # B should still be receiving. Send from A and wait.
  local marker="post-idle-$(date +%s%N)"
  AIRC_HOME="$A_HOME/state" "$AIRC" msg --room "$rname" "$marker" >/dev/null 2>&1

  local i seen=0
  for i in $(seq 1 30); do
    sleep 2
    if grep -qF "$marker" "$B_HOME/state/messages.jsonl" 2>/dev/null; then
      seen=1; break
    fi
  done

  if [ "$seen" = "1" ]; then
    pass "B received post-idle send (heartbeat kept pipeline alive)"
  else
    fail "B silent after 45s idle — pipeline went stuck (original Joel-observed bug)"
  fi
}

# ─────────────────────────────────────────────────────────────────────
# scenario: part_deletes_host_gist
#
# `airc part` from the host should delete the room gist on gh.
# Joiners parting just teardown locally (host's gist persists).
# ─────────────────────────────────────────────────────────────────────
scenario_status_agrees_with_send() {
  # Today's bug Joel called out: 'airc status' said monitor: not running
  # while 'airc msg' worked + landed in gist. The two diagnostics
  # disagreed, which is exactly the silent-broken class CLAUDE.md
  # forbids. If sends work, status MUST report monitor running.
  section "status_agrees_with_send: if msg lands in gist, status must say running"
  require_gh || return

  local rname="smoke-statusagree-$$"
  local A_HOME
  A_HOME=$(mktemp -d -t airc-statusagree.XXXXXX)
  trap "cleanup_homes '$A_HOME'" RETURN

  spawn_real "$A_HOME" "smoke-sa-$$" 7607 --room "$rname" --as-host \
    || { fail "host failed to start"; return; }
  sleep 2

  local marker="status-agree-$(date +%s%N)"
  AIRC_HOME="$A_HOME/state" "$AIRC" msg --room "$rname" "$marker" >/dev/null 2>&1
  sleep 3

  local gid; gid=$(python3 -c "import json;print(json.load(open('$A_HOME/state/config.json')).get('channel_gists',{}).get('$rname',''))")
  local landed=0
  gh api "gists/$gid" --jq '.files["messages.jsonl"].content // ""' 2>/dev/null | grep -qF "$marker" && landed=1
  [ "$landed" = "1" ] || { fail "msg didn't land in gist — substrate broken, can't test status"; return; }

  local status_out; status_out=$(AIRC_HOME="$A_HOME/state" "$AIRC" status 2>&1)
  if printf '%s' "$status_out" | grep -qE "monitor: *running"; then
    pass "msg landed AND status says monitor running (diagnostics agree)"
  else
    fail "msg LANDED but status reports monitor not running — diagnostics lie"
    printf '%s' "$status_out" | sed 's/^/    /'
  fi
}

scenario_stale_config_auto_resyncs() {
  # Pre-seed channel_gists with a bogus gist id (simulates a peer
  # who paired with a non-canonical dup pre-#321). After bounce,
  # the host's subscription must self-heal to the actual gist on
  # disk. Pre-fix peers stuck on stale mappings forever.
  section "stale_config_auto_resyncs: bogus channel_gists is replaced on bounce"
  require_gh || return

  local rname="smoke-resync-$$"
  local A_HOME
  A_HOME=$(mktemp -d -t airc-resync.XXXXXX)
  trap "cleanup_homes '$A_HOME'" RETURN

  spawn_real "$A_HOME" "smoke-rs-$$" 7608 --room "$rname" --as-host \
    || { fail "first spawn failed"; return; }
  local good_gid; good_gid=$(python3 -c "import json;print(json.load(open('$A_HOME/state/config.json')).get('channel_gists',{}).get('$rname',''))")
  [ -n "$good_gid" ] || { fail "first spawn didn't write channel_gists"; return; }
  pass "first spawn: channel_gists['$rname']=$good_gid"

  AIRC_HOME="$A_HOME/state" "$AIRC" teardown >/dev/null 2>&1

  # Poison: replace channel_gists[$rname] with a bogus id
  python3 -c "
import json
p = '$A_HOME/state/config.json'
c = json.load(open(p))
c['channel_gists'] = {'$rname': 'deadbeefcafebabe000000000000beef'}
json.dump(c, open(p,'w'), indent=2)
"

  ( cd "$A_HOME" && AIRC_HOME="$A_HOME/state" AIRC_NAME="smoke-rs-$$" AIRC_PORT=7609 \
      AIRC_NO_DISCOVERY=1 AIRC_NO_AUTO_ROOM=1 AIRC_NO_GENERAL=1 AIRC_NO_IDENTITY_PROMPT=1 \
      "$AIRC" connect --room "$rname" > "$A_HOME/out2.log" 2>&1 & )
  local i
  for i in $(seq 1 15); do
    sleep 1
    grep -qE "Hosting as|Connected to|Joined" "$A_HOME/out2.log" 2>/dev/null && break
  done
  sleep 3

  local final_gid; final_gid=$(python3 -c "import json;print(json.load(open('$A_HOME/state/config.json')).get('channel_gists',{}).get('$rname',''))")
  if [ "$final_gid" = "$good_gid" ]; then
    pass "auto-resync replaced bogus 'deadbeef...' with the canonical gist"
  elif [ "$final_gid" = "deadbeefcafebabe000000000000beef" ]; then
    fail "STILL bogus after bounce — channel_gists was trusted blindly, no auto-resync"
  else
    fail "channel_gists['$rname']='$final_gid' (neither canonical nor bogus — created a 3rd duplicate?)"
  fi
}

scenario_part_deletes_host_gist() {
  section "part: host's airc part deletes the gist; joiner's part doesn't"
  require_gh || return

  local rname="smoke-part-$$"
  local A_HOME
  A_HOME=$(mktemp -d -t airc-smoke-partA.XXXXXX)
  trap "cleanup_homes '$A_HOME'" RETURN

  spawn_real "$A_HOME" "smoke-partA-$$" 7597 --room "$rname" --as-host \
    || { fail "host failed to start"; return; }
  sleep 1

  local gid
  gid=$(python3 -c "import json;print(json.load(open('$A_HOME/state/config.json')).get('channel_gists',{}).get('$rname',''))" 2>/dev/null)
  [ -n "$gid" ] || { fail "no channel_gists for #$rname"; return; }

  # Confirm gist exists pre-part.
  if gh api "gists/$gid" --jq '.id' >/dev/null 2>&1; then
    pass "gist exists pre-part: $gid"
  else
    fail "gist missing before part — bootstrap broken"; return
  fi

  AIRC_HOME="$A_HOME/state" "$AIRC" part >/dev/null 2>&1 || true
  sleep 2

  if gh api "gists/$gid" --jq '.id' 2>/dev/null >/dev/null; then
    fail "gist still exists post-part — host should have deleted it"
    gh gist delete "$gid" --yes 2>/dev/null || true
  else
    pass "gist deleted on host's airc part"
  fi
}

# ─────────────────────────────────────────────────────────────────────
# Dispatch
# ─────────────────────────────────────────────────────────────────────
case "${1:-all}" in
  passive_recv)              scenario_passive_recv ;;
  round_trip)                scenario_round_trip ;;
  idle_then_recv)            scenario_idle_then_recv ;;
  part_deletes_host_gist)    scenario_part_deletes_host_gist ;;
  status_agrees_with_send)   scenario_status_agrees_with_send ;;
  stale_config_auto_resyncs) scenario_stale_config_auto_resyncs ;;
  all)
    scenario_passive_recv
    scenario_round_trip
    scenario_status_agrees_with_send
    scenario_stale_config_auto_resyncs
    scenario_part_deletes_host_gist
    # idle_then_recv last — slow (45s+ idle wait)
    scenario_idle_then_recv
    ;;
  *)
    echo "Usage: $0 [passive_recv|round_trip|idle_then_recv|part_deletes_host_gist|status_agrees_with_send|stale_config_auto_resyncs|all]"
    exit 2
    ;;
esac

echo
echo "─────────────────"
echo "  ${GRN}pass:${RST} $PASS  ${RED}fail:${RST} $FAIL  ${YLO}skip:${RST} $SKIP"
[ "$FAIL" = "0" ] || exit 1
exit 0
