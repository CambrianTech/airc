#!/bin/bash
# AIRC integration test — two scenarios, minimal deps.
#
#   tabs   two airc processes on one machine (different ports + isolated homes)
#   scope  per-project $PWD/.airc tier precedence + home tier fallthrough
#
# Uses the `airc` binary from this repo (../airc, relative to the test dir).
# Idempotent: cleans up before and after.
#
# Usage:
#   ./test/integration.sh            # run everything
#   ./test/integration.sh tabs       # only tabs scenario
#   ./test/integration.sh scope      # only scope scenario

set -u

# ── Harness ─────────────────────────────────────────────────────────────

AIRC="${AIRC:-$(cd "$(dirname "$0")/.." && pwd)/airc}"
[ -x "$AIRC" ] || { echo "FATAL: $AIRC not executable"; exit 2; }

RED=$'\033[0;31m'; GRN=$'\033[0;32m'; YLO=$'\033[0;33m'; RST=$'\033[0m'
PASS=0; FAIL=0; TRACE=()

pass() { echo "  ${GRN}✓${RST} $1"; PASS=$((PASS+1)); }
fail() { echo "  ${RED}✗${RST} $1"; FAIL=$((FAIL+1)); TRACE+=("$1"); }
section() { echo; echo "${YLO}── $1 ──${RST}"; }

cleanup_procs() {
  # Kill everything associated with prior test scenarios by walking airc.pid
  # files under /tmp/airc-it-*. These files are the canonical scope markers
  # written by `airc connect`. Each file contains PID(s) of its test host.
  local pidfile
  for pidfile in /tmp/airc-it-*/state/airc.pid; do
    [ -f "$pidfile" ] || continue
    local pids; pids=$(cat "$pidfile" 2>/dev/null)
    if [ -n "$pids" ]; then
      # Include direct children (python listeners)
      local all="$pids"
      for p in $pids; do
        all="$all $(pgrep -P "$p" 2>/dev/null | tr '\n' ' ')"
      done
      kill -9 $all 2>/dev/null || true
    fi
    rm -f "$pidfile"
  done
  # Safety net: reap any python listener still on port 7548 — if it's ours it
  # had a chance to exit with its parent; if it's not, someone's squatting.
  local lpids
  lpids=$(lsof -tiTCP:7548 -sTCP:LISTEN 2>/dev/null || true)
  for lpid in $lpids; do
    if ps -p "$lpid" -o command= 2>/dev/null | grep -q "socket.SOCK_STREAM"; then
      kill -9 "$lpid" 2>/dev/null || true
    fi
  done
  sleep 1
}

cleanup_dirs() {
  # Use find not glob: zsh with nomatch errors when no match exists, and we
  # still want deterministic cleanup between runs. Find exits 0 on no match.
  find /tmp -maxdepth 1 -name 'airc-it-*' -exec rm -rf {} + 2>/dev/null || true
}

cleanup_all() { cleanup_procs; cleanup_dirs; }

# Boot a host. Args: home, name, port
spawn_host() {
  local home="$1" name="$2" port="$3"
  mkdir -p "$home"
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME="$name" AIRC_PORT="$port" \
      "$AIRC" connect > "$home/out.log" 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -q 'Hosting as' "$home/out.log" 2>/dev/null && return 0
  done
  return 1
}

# Join a host. Args: home, name, join-string
spawn_joiner() {
  local home="$1" name="$2" join="$3"
  mkdir -p "$home"
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME="$name" \
      "$AIRC" connect "$join" > "$home/out.log" 2>&1 & )
  local i
  for i in 1 2 3 4 5 6; do
    sleep 1
    grep -q 'Connected to' "$home/out.log" 2>/dev/null && return 0
  done
  return 1
}

# Extract the join string from a host's log.
read_join_string() {
  grep -oE '[a-z0-9-]+@[a-z]+@[^:]+(:[0-9]+)?#[A-Za-z0-9+/=]+' "$1/out.log" | head -1
}

# airc send from a given home.
as_home() {
  local home="$1"; shift
  AIRC_HOME="$home/state" "$AIRC" "$@"
}

# ── Scenario: tabs ──────────────────────────────────────────────────────

scenario_tabs() {
  section "tabs: two processes on one machine (ports + isolated homes)"
  cleanup_all

  spawn_host /tmp/airc-it-h alpha 7548 || { fail "alpha host failed to start"; return; }
  pass "alpha hosting on 7548"

  local join; join=$(read_join_string /tmp/airc-it-h)
  [ -n "$join" ] && pass "join string captured: ${join:0:40}..." \
                 || { fail "no join string in alpha log"; return; }

  case "$join" in *":7548#"*) pass ":7548 in join string (port override)" ;;
                  *) fail ":port missing from join string" ;;
  esac

  spawn_joiner /tmp/airc-it-j beta "$join" || { fail "beta join failed"; return; }
  pass "beta joined alpha"

  # Let pair-handshake fs writes settle. Peer-record correctness is proven
  # transitively below: if sends, monitor reads, and rename propagation all
  # work, the peer record + airc_home field were written correctly.
  sleep 3
  local send_err
  send_err=$(as_home /tmp/airc-it-j send alpha "m1-from-beta" 2>&1 >/dev/null)
  if [ $? -eq 0 ]; then
    pass "beta → alpha send returns OK"
  else
    fail "beta send failed: $send_err"
  fi

  send_err=$(as_home /tmp/airc-it-h send beta "m2-from-alpha" 2>&1 >/dev/null)
  if [ $? -eq 0 ]; then
    pass "alpha → beta send returns OK"
  else
    fail "alpha send failed: $send_err"
  fi

  sleep 8
  grep -q 'm1-from-beta' /tmp/airc-it-h/out.log && pass "alpha monitor saw m1" \
                                                || fail "alpha monitor did NOT see m1"
  grep -q 'm2-from-alpha' /tmp/airc-it-j/out.log && pass "beta monitor saw m2" \
                                                 || fail "beta monitor did NOT see m2"

  as_home /tmp/airc-it-h rename gamma >/dev/null 2>&1 && pass "alpha renamed to gamma" \
                                                      || fail "rename failed"

  sleep 8
  grep -q 'Peer renamed' /tmp/airc-it-j/out.log && pass "beta saw [rename] marker" \
                                                || fail "beta did NOT see rename marker"

  as_home /tmp/airc-it-j peers 2>/dev/null | grep -q gamma && pass "beta peers shows gamma" \
                                                           || fail "beta peers still shows alpha"

  # Final invariants on beta's state: peer record exists, has non-empty airc_home
  # (proves the handshake's relay_home exchange works end-to-end).
  local peer_file="/tmp/airc-it-j/state/peers/gamma.json"
  [ -f "$peer_file" ] && pass "peer record for renamed peer is on disk" \
                      || fail "no peer record for gamma (rename didn't persist)"
  local peer_home
  peer_home=$(python3 -c "import json; print(json.load(open('$peer_file')).get('airc_home',''))" 2>/dev/null || true)
  [ -n "$peer_home" ] && pass "peer record has non-empty airc_home ($peer_home)" \
                      || fail "peer record airc_home is empty — remote_home() fallback would misroute sends"

  cleanup_all
}

# ── Scenario: scope ─────────────────────────────────────────────────────

scenario_scope() {
  section "scope: per-project .airc/ precedence + home fallthrough"
  cleanup_all

  # Seed home tier with a known peer record.
  local home="/tmp/airc-it-homefake/.airc"
  mkdir -p "$home/peers" "$home/identity"
  echo '{"name":"home-peer","host":"joel@1.2.3.4"}' > "$home/peers/home-peer.json"
  echo '{"name":"home-self"}' > "$home/config.json"

  # From a dir with NO .airc/: airc should read home tier.
  local visible
  visible=$(cd /tmp/airc-it-homefake && HOME=/tmp/airc-it-homefake "$AIRC" peers 2>&1 | grep home-peer)
  [ -n "$visible" ] && pass "no local tier: airc reads home peers" \
                    || fail "no local tier: home peers not visible"

  # From a dir WITH empty .airc/: should still see home peers (union).
  local testdir="/tmp/airc-it-project"
  mkdir -p "$testdir/.airc/peers"
  cp "$home/config.json" "$testdir/.airc/config.json"
  visible=$(cd "$testdir" && HOME=/tmp/airc-it-homefake "$AIRC" peers 2>&1 | grep home-peer)
  [ -n "$visible" ] && pass "local tier present, no local peers: home peers still visible" \
                    || fail "local tier should inherit home peers"

  # Add a local peer with same name — local should shadow home.
  echo '{"name":"home-peer","host":"LOCAL-OVERRIDE"}' > "$testdir/.airc/peers/home-peer.json"
  visible=$(cd "$testdir" && HOME=/tmp/airc-it-homefake "$AIRC" peers 2>&1 | grep home-peer)
  echo "$visible" | grep -q LOCAL-OVERRIDE && pass "local tier shadows home when name collides" \
                                           || fail "local tier did NOT shadow home peer"

  rm -rf /tmp/airc-it-homefake /tmp/airc-it-project
}

# ── Entry point ─────────────────────────────────────────────────────────

MODE="${1:-all}"
trap cleanup_all EXIT INT TERM

scenario_reminder() {
  section "reminder: heartbeat fires after interval of silence; controls work"
  cleanup_all

  # Host with a short reminder interval (2s) so the test doesn't wait a year.
  # Monitor polls every 5s, so we need to allow one poll cycle + interval elapsed.
  local home=/tmp/airc-it-r
  mkdir -p "$home"
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME=hb-host AIRC_PORT=7548 AIRC_REMINDER=2 \
      "$AIRC" connect > "$home/out.log" 2>&1 & )
  local i
  for i in 1 2 3 4 5; do sleep 1; grep -q 'Hosting as' "$home/out.log" 2>/dev/null && break; done

  # 1) Interval was actually applied on host start
  grep -q 'reminder: 2s' "$home/out.log" && pass "AIRC_REMINDER env set interval to 2s at host start" \
                                         || fail "host didn't honor AIRC_REMINDER=2 (log says: $(grep 'Hosting as' "$home/out.log" | head -1))"

  # 2) Interval is persisted to the reminder file
  local persisted
  persisted=$(cat "$home/state/reminder" 2>/dev/null)
  [ "$persisted" = "2" ] && pass "reminder interval persisted to state/reminder ($persisted)" \
                         || fail "reminder file wrong: expected '2', got '$persisted'"

  # 3) Seed last_sent so the heartbeat guard ([ last_sent -gt 0 ]) passes.
  #    Set it 3s in the past so by the next monitor poll we'll already be silent-for-3s.
  local seeded=$(( $(date +%s) - 3 ))
  echo "$seeded" > "$home/state/last_sent"
  rm -f "$home/state/reminded"   # allow firing

  # 4) Wait up to ~12s for monitor to poll and emit the heartbeat.
  local fired=0
  for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
    sleep 1
    grep -q 'Reminder: you haven.t sent a message' "$home/out.log" 2>/dev/null && { fired=1; break; }
  done
  [ "$fired" = "1" ] && pass "heartbeat fired within 12s of silence" \
                     || fail "heartbeat did NOT fire within 12s (state/reminded=$([ -f "$home/state/reminded" ] && echo yes || echo no))"

  # 5) 'reminded' marker set so it won't re-fire
  [ -f "$home/state/reminded" ] && pass "reminded marker set after firing (won't spam)" \
                                || fail "reminded marker missing — heartbeat would re-fire every poll"

  # 6) 'airc reminder off' removes the reminder file
  AIRC_HOME="$home/state" "$AIRC" reminder off >/dev/null 2>&1
  [ ! -f "$home/state/reminder" ] && pass "'airc reminder off' removed the reminder file" \
                                  || fail "'airc reminder off' did NOT disable reminders"

  # 7) 'airc reminder <n>' re-enables with the new interval
  AIRC_HOME="$home/state" "$AIRC" reminder 42 >/dev/null 2>&1
  local updated; updated=$(cat "$home/state/reminder" 2>/dev/null)
  [ "$updated" = "42" ] && pass "'airc reminder 42' set new interval" \
                        || fail "'airc reminder 42' failed (reminder=$updated)"

  cleanup_all
}

scenario_teardown() {
  section "teardown: airc teardown kills processes, preserves state (without --flush)"
  cleanup_all

  spawn_host /tmp/airc-it-td td-host 7548 || { fail "host failed to start for teardown test"; return; }
  pass "host running before teardown"

  # Confirm port held
  lsof -tiTCP:7548 -sTCP:LISTEN >/dev/null 2>&1 && pass "port 7548 held pre-teardown" \
                                               || fail "port 7548 not held — host not really up?"

  # Scope-aware teardown needs AIRC_HOME matching the host's scope, otherwise
  # it'll refuse to kill processes outside its tier (which is the whole point
  # of the scoping — different Claude tabs can't nuke each other's hosts).
  AIRC_HOME=/tmp/airc-it-td/state AIRC_PORT=7548 "$AIRC" teardown >/dev/null 2>&1
  sleep 1

  lsof -tiTCP:7548 -sTCP:LISTEN >/dev/null 2>&1 && fail "port 7548 still held after teardown" \
                                               || pass "port 7548 freed by teardown"

  pgrep -f "AIRC_NAME=td-host" >/dev/null 2>&1 && fail "host process still alive after teardown" \
                                                || pass "host process terminated by teardown"

  # State should survive a non-flush teardown
  [ -f /tmp/airc-it-td/state/config.json ] && pass "state preserved (identity kept for resume)" \
                                            || fail "state wiped by teardown (should only flush with --flush)"

  # Now test the scope-isolation guarantee: another host spawned in a different
  # AIRC_HOME should NOT be killed by a teardown running in yet-another scope.
  spawn_host /tmp/airc-it-td2 td-host-2 7548 || { fail "host failed for scope-isolation test"; return; }
  AIRC_HOME=/tmp/airc-some-unrelated-scope AIRC_PORT=7548 "$AIRC" teardown >/dev/null 2>&1
  sleep 1
  lsof -tiTCP:7548 -sTCP:LISTEN >/dev/null 2>&1 && pass "teardown in different scope did NOT kill unrelated host" \
                                               || fail "teardown crossed scope boundary and killed a foreign host"

  cleanup_all
}

case "$MODE" in
  tabs)      scenario_tabs  ;;
  scope)     scenario_scope ;;
  teardown)  scenario_teardown ;;
  reminder)  scenario_reminder ;;
  all)       scenario_tabs; scenario_scope; scenario_reminder; scenario_teardown ;;
  *) echo "Usage: $0 [tabs|scope|teardown|reminder|all]"; exit 2 ;;
esac

echo
echo "─────────────────"
echo "$PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  echo
  echo "Failures:"
  for t in "${TRACE[@]}"; do echo "  - $t"; done
  exit 1
fi
exit 0
