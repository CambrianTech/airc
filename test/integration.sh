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
  # Dogfood the binary's own teardown — if this is broken, the test should fail
  # visibly, not silently mask it with pkill. Also nukes anything our test ports
  # were holding (teardown targets 7547/7548 by default).
  AIRC_PORT=7548 "$AIRC" teardown >/dev/null 2>&1 || true
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

  local peer_file; peer_file="/tmp/airc-it-j/state/peers/alpha.json"
  [ -f "$peer_file" ] && pass "beta's peer record of alpha written" \
                      || fail "no peer record for alpha"

  grep -q '"airc_home":' "$peer_file" && pass "peer record includes airc_home field" \
                                      || fail "peer record missing airc_home"

  # Sends must travel over SSH; wait a beat after pairing so monitor is stable.
  sleep 2
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

scenario_teardown() {
  section "teardown: airc teardown kills processes, preserves state (without --flush)"
  cleanup_all

  spawn_host /tmp/airc-it-td td-host 7548 || { fail "host failed to start for teardown test"; return; }
  pass "host running before teardown"

  # Confirm port held
  lsof -tiTCP:7548 -sTCP:LISTEN >/dev/null 2>&1 && pass "port 7548 held pre-teardown" \
                                               || fail "port 7548 not held — host not really up?"

  AIRC_PORT=7548 "$AIRC" teardown >/dev/null 2>&1
  sleep 1

  lsof -tiTCP:7548 -sTCP:LISTEN >/dev/null 2>&1 && fail "port 7548 still held after teardown" \
                                               || pass "port 7548 freed by teardown"

  pgrep -f "AIRC_NAME=td-host" >/dev/null 2>&1 && fail "host process still alive after teardown" \
                                                || pass "host process terminated by teardown"

  # State should survive a non-flush teardown
  [ -f /tmp/airc-it-td/state/config.json ] && pass "state preserved (identity kept for resume)" \
                                            || fail "state wiped by teardown (should only flush with --flush)"

  cleanup_all
}

case "$MODE" in
  tabs)      scenario_tabs  ;;
  scope)     scenario_scope ;;
  teardown)  scenario_teardown ;;
  all)       scenario_tabs; scenario_scope; scenario_teardown ;;
  *) echo "Usage: $0 [tabs|scope|teardown|all]"; exit 2 ;;
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
