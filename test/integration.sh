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
  # Kill ONLY processes belonging to this test run. Walk airc.pid files under
  # /tmp/airc-it-*/ (canonical scope markers written by `airc connect`).
  # NEVER fall back to broad "kill anything on port X" — that's what took out
  # a live demo host on 7549 earlier. If a test leaves something running,
  # that's a test bug to fix via pidfile, not a bigger pkill hammer.
  local pidfile
  for pidfile in /tmp/airc-it-*/state/airc.pid; do
    [ -f "$pidfile" ] || continue
    local pids; pids=$(cat "$pidfile" 2>/dev/null)
    if [ -n "$pids" ]; then
      local all="$pids"
      for p in $pids; do
        all="$all $(pgrep -P "$p" 2>/dev/null | tr '\n' ' ')"
      done
      kill -9 $all 2>/dev/null || true
    fi
    rm -f "$pidfile"
  done
  sleep 1
}

cleanup_dirs() {
  # Use find not glob: zsh with nomatch errors when no match exists, and we
  # still want deterministic cleanup between runs. Find exits 0 on no match.
  find /tmp -maxdepth 1 -name 'airc-it-*' -exec rm -rf {} + 2>/dev/null || true
}

cleanup_known_hosts() {
  # Test alpha/beta hosts run on the user's real SSH target
  # (joelteply@100.91.51.87 or similar), so their pair handshake writes
  # ephemeral test host keys into ~/.ssh/known_hosts. Left behind, those
  # stale keys collide with the user's real airc host running on the
  # same IP — SSH to the real host fails with REMOTE HOST IDENTIFICATION
  # HAS CHANGED. Clear any entries for this machine's address between runs.
  local addr; addr=$(hostname -I 2>/dev/null | awk '{print $1}')
  [ -z "$addr" ] && addr=$(ipconfig getifaddr en0 2>/dev/null)
  if [ -n "$addr" ]; then
    ssh-keygen -R "$addr" -f "$HOME/.ssh/known_hosts" >/dev/null 2>&1 || true
  fi
  # Also the tailscale IP family airc tests commonly use
  ssh-keygen -R 100.91.51.87 -f "$HOME/.ssh/known_hosts" >/dev/null 2>&1 || true
}

cleanup_all() { cleanup_procs; cleanup_dirs; cleanup_known_hosts; }

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

  spawn_host /tmp/airc-it-h alpha 7549 || { fail "alpha host failed to start"; return; }
  pass "alpha hosting on 7549"

  local join; join=$(read_join_string /tmp/airc-it-h)
  [ -n "$join" ] && pass "join string captured: ${join:0:40}..." \
                 || { fail "no join string in alpha log"; return; }

  case "$join" in *":7549#"*) pass ":7549 in join string (port override)" ;;
                  *) fail ":port missing from join string" ;;
  esac

  spawn_joiner /tmp/airc-it-j beta "$join" || { fail "beta join failed"; return; }
  pass "beta joined alpha"

  # Let pair-handshake fs writes settle. Peer-record correctness is proven
  # transitively below: if sends, monitor reads, and rename propagation all
  # work, the peer record + airc_home field were written correctly.
  sleep 3
  local send_err
  send_err=$(as_home /tmp/airc-it-j send @alpha "m1-from-beta" 2>&1 >/dev/null)
  if [ $? -eq 0 ]; then
    pass "beta → alpha send returns OK"
  else
    fail "beta send failed: $send_err"
  fi

  # Joiner's outbound must ALSO appear in its own local messages.jsonl so
  # `airc logs` has an audit trail — not only on the remote host.
  grep -q '"m1-from-beta"' /tmp/airc-it-j/state/messages.jsonl 2>/dev/null && \
    pass "joiner outbound mirrored to local messages.jsonl (audit trail)" \
    || fail "joiner outbound NOT written locally — airc logs wouldn't show the send"

  # send-file uses scp. Broken if airc doesn't pass the isolated identity
  # key to scp (system ssh_config falls back to ~/.ssh/id_* which doesn't
  # exist in isolated homes). Surfaced by m5-test's real-world test.
  local payload=/tmp/airc-it-j/send-file-probe.txt
  printf 'airc send-file round-trip probe — %s\n' "$(date -u +%s)" > "$payload"
  as_home /tmp/airc-it-j send-file alpha "$payload" >/dev/null 2>&1 && \
    pass "send-file to alpha returns OK" || fail "send-file failed (scp auth?)"
  sleep 2
  [ -f /tmp/airc-it-h/state/files/beta/send-file-probe.txt ] && \
    pass "send-file payload landed on host at files/beta/send-file-probe.txt" \
    || fail "send-file ran but no payload on host"

  # Resilience: if the wire fails, the outbound MUST still be in local log
  # with a [QUEUED] marker AND enqueued in pending.jsonl for automatic retry.
  # (Prior to send-queue: a [SEND FAILED] marker and no retry; see scenario_queue
  # for the end-to-end drain test.) Simulate by sending with a bogus host_target.
  local fake_home=/tmp/airc-it-fail-test
  mkdir -p "$fake_home/state/peers" "$fake_home/state/identity"
  cp /tmp/airc-it-j/state/identity/* "$fake_home/state/identity/" 2>/dev/null
  cp /tmp/airc-it-j/state/config.json "$fake_home/state/config.json"
  # Point host_target at an unreachable host
  python3 -c "
import json
c = json.load(open('$fake_home/state/config.json'))
c['host_target'] = 'nobody@127.0.0.99'
c['host_airc_home'] = '/tmp/nowhere'
json.dump(c, open('$fake_home/state/config.json', 'w'))
"
  # Write a fake peer so resolution doesn't fail
  echo '{"name":"ghost","host":"nobody@127.0.0.99","airc_home":"/tmp/nowhere"}' > "$fake_home/state/peers/ghost.json"
  AIRC_HOME=$fake_home/state "$AIRC" send @ghost "this-should-fail-but-still-mirror" >/dev/null 2>&1
  # Exit should be non-zero (we die()), but local must have both the attempt AND the failure marker
  grep -q '"this-should-fail-but-still-mirror"' "$fake_home/state/messages.jsonl" 2>/dev/null && \
    pass "failed send: outbound still mirrored to local log (no silent loss)" \
    || fail "failed send: outbound NOT in local log (silent loss regression)"
  grep -q 'QUEUED' "$fake_home/state/messages.jsonl" 2>/dev/null && \
    pass "failed send: [QUEUED] marker present in local log" \
    || fail "failed send: no [QUEUED] marker — user can't tell it was queued"
  [ -s "$fake_home/state/pending.jsonl" ] && \
    grep -q 'this-should-fail-but-still-mirror' "$fake_home/state/pending.jsonl" 2>/dev/null && \
    pass "failed send: message also enqueued in pending.jsonl for retry" \
    || fail "failed send: pending.jsonl missing — message won't auto-retry"
  rm -rf "$fake_home"

  send_err=$(as_home /tmp/airc-it-h send @beta "m2-from-alpha" 2>&1 >/dev/null)
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
  section "scope: $PWD/.airc with AIRC_HOME override"
  cleanup_all

  local a="/tmp/airc-it-scope-a"
  local b="/tmp/airc-it-scope-b"
  local asub="$a/sub"
  rm -rf "$a" "$b"
  mkdir -p "$asub" "$b"

  # Resolve symlinks: detect_scope uses `pwd -P`, so compare against resolved.
  local a_real asub_real b_real
  a_real=$(cd "$a" && pwd -P)
  asub_real=$(cd "$asub" && pwd -P)
  b_real=$(cd "$b" && pwd -P)

  local scope_a scope_asub scope_b
  scope_a=$(cd "$a" && "$AIRC" debug-scope 2>&1)
  scope_asub=$(cd "$asub" && "$AIRC" debug-scope 2>&1)
  scope_b=$(cd "$b" && "$AIRC" debug-scope 2>&1)

  [ "$scope_a" = "$a_real/.airc" ] && pass "cwd=$a: scope = \$PWD/.airc" \
                                   || fail "cwd=$a: got '$scope_a' (expected $a_real/.airc)"
  [ "$scope_asub" = "$asub_real/.airc" ] && pass "subdir: scope differs from parent (per-cwd)" \
                                         || fail "subdir scope: got '$scope_asub'"
  [ "$scope_b" = "$b_real/.airc" ] && pass "different cwd: different scope" \
                                   || fail "cwd=$b: got '$scope_b'"

  # AIRC_HOME override wins.
  local scope_override
  scope_override=$(cd "$a" && AIRC_HOME=/tmp/airc-it-override "$AIRC" debug-scope 2>&1)
  [ "$scope_override" = "/tmp/airc-it-override" ] && pass "AIRC_HOME overrides cwd detection" \
                                                  || fail "override ignored: got '$scope_override'"

  # Derived name: basename + 4-char hash, same-basename-different-dirs don't collide.
  local same_base_1="/tmp/airc-it-proj-alpha/src"
  local same_base_2="/tmp/airc-it-proj-beta/src"
  mkdir -p "$same_base_1" "$same_base_2"
  local name_1 name_2
  name_1=$(cd "$same_base_1" && "$AIRC" debug-name 2>&1)
  name_2=$(cd "$same_base_2" && "$AIRC" debug-name 2>&1)
  [ -n "$name_1" ] && [ -n "$name_2" ] && [ "$name_1" != "$name_2" ] \
    && pass "same basename different dirs: unique names ('$name_1' vs '$name_2')" \
    || fail "same-basename clash: '$name_1' vs '$name_2'"

  rm -rf "$a" "$b" /tmp/airc-it-override /tmp/airc-it-proj-alpha /tmp/airc-it-proj-beta
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
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME=hb-host AIRC_PORT=7549 AIRC_REMINDER=2 \
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

  spawn_host /tmp/airc-it-td td-host 7549 || { fail "host failed to start for teardown test"; return; }
  pass "host running before teardown"

  # Confirm port held
  lsof -tiTCP:7549 -sTCP:LISTEN >/dev/null 2>&1 && pass "port 7549 held pre-teardown" \
                                               || fail "port 7549 not held — host not really up?"

  # Scope-aware teardown needs AIRC_HOME matching the host's scope, otherwise
  # it'll refuse to kill processes outside its tier (which is the whole point
  # of the scoping — different Claude tabs can't nuke each other's hosts).
  AIRC_HOME=/tmp/airc-it-td/state AIRC_PORT=7549 "$AIRC" teardown >/dev/null 2>&1
  sleep 1

  lsof -tiTCP:7549 -sTCP:LISTEN >/dev/null 2>&1 && fail "port 7549 still held after teardown" \
                                               || pass "port 7549 freed by teardown"

  pgrep -f "AIRC_NAME=td-host" >/dev/null 2>&1 && fail "host process still alive after teardown" \
                                                || pass "host process terminated by teardown"

  # State should survive a non-flush teardown
  [ -f /tmp/airc-it-td/state/config.json ] && pass "state preserved (identity kept for resume)" \
                                            || fail "state wiped by teardown (should only flush with --flush)"

  # Now test the scope-isolation guarantee: another host spawned in a different
  # AIRC_HOME should NOT be killed by a teardown running in yet-another scope.
  spawn_host /tmp/airc-it-td2 td-host-2 7549 || { fail "host failed for scope-isolation test"; return; }
  AIRC_HOME=/tmp/airc-some-unrelated-scope AIRC_PORT=7549 "$AIRC" teardown >/dev/null 2>&1
  sleep 1
  lsof -tiTCP:7549 -sTCP:LISTEN >/dev/null 2>&1 && pass "teardown in different scope did NOT kill unrelated host" \
                                               || fail "teardown crossed scope boundary and killed a foreign host"

  cleanup_all
}

scenario_resilience() {
  section "resilience: stale-pidfile recovery + malformed peer + dead-pidfile teardown"
  cleanup_all

  # ── Regression for #6 (airc-96dd PR #8) ──────────────────────────────
  # A dead PID in airc.pid used to wedge cmd_connect: `pgrep -P <dead>` exits 1,
  # pipefail promoted it, and the script aborted before the self-healing rm -f.
  # Fix guards the pipeline with `|| true`. Regression test: seed a dead PID,
  # run connect in host mode, assert it reaches "Hosting as" AND clears the pid.
  local sp_home=/tmp/airc-it-stalepid
  mkdir -p "$sp_home/state"
  # PID 1 always exists but can't be our parent, and pgrep -P 999999 always returns 1.
  echo "999999" > "$sp_home/state/airc.pid"
  ( cd "$sp_home" && AIRC_HOME="$sp_home/state" AIRC_NAME=stalepid-host AIRC_PORT=7549 \
      "$AIRC" connect > "$sp_home/out.log" 2>&1 & )
  local i
  for i in 1 2 3 4 5 6; do sleep 1; grep -q 'Hosting as' "$sp_home/out.log" 2>/dev/null && break; done
  grep -q 'Hosting as' "$sp_home/out.log" && pass "stale pidfile: cmd_connect recovers and reaches Hosting" \
                                          || fail "stale pidfile wedged cmd_connect (log: $(tail -3 "$sp_home/out.log"))"
  # After recovery, pidfile should reflect the NEW process (not the old 999999).
  local new_pid; new_pid=$(cat "$sp_home/state/airc.pid" 2>/dev/null | head -1)
  [ -n "$new_pid" ] && [ "$new_pid" != "999999" ] && \
    pass "stale pidfile: replaced by live PID on recovery ($new_pid)" \
    || fail "stale pidfile: .airc/airc.pid still '$new_pid' — self-heal didn't overwrite"

  # ── Regression for #9 teardown-with-dead-pidfile ──────────────────────
  # cmd_teardown's `cat pidfile | tr` had the same pipefail shape; if the file
  # was racily removed between -f and cat, teardown aborted. Fix adds || true.
  # Seed dead PIDs, run teardown, assert it completes cleanly.
  local td_home=/tmp/airc-it-deadtd
  mkdir -p "$td_home/state"
  echo "888888 888889" > "$td_home/state/airc.pid"
  local td_out
  td_out=$(AIRC_HOME="$td_home/state" AIRC_PORT=7550 "$AIRC" teardown 2>&1)
  # Teardown with all-dead PIDs and no live listener should not print "killing
  # scope" (kill -0 gate in cmd_connect's block, none alive). Either way, it
  # must exit 0 and clear the pidfile.
  [ -f "$td_home/state/airc.pid" ] && fail "dead-pidfile teardown: airc.pid not removed" \
                                   || pass "dead-pidfile teardown: pidfile cleared without wedging"
  echo "$td_out" | grep -q 'Teardown complete' && pass "dead-pidfile teardown: reached 'Teardown complete'" \
                                               || fail "dead-pidfile teardown: aborted before completion ($td_out)"

  # ── Regression for #9 peers-with-malformed-record ────────────────────
  # cmd_peers' `python3 -c json.load(...)[key]` exits 1 on malformed JSON,
  # which under set -e aborted the whole loop. Fix adds || true so one bad
  # record doesn't hide all the good ones.
  local pr_home=/tmp/airc-it-peersbad
  mkdir -p "$pr_home/state/peers" "$pr_home/state/identity"
  echo '{"name":"test"}' > "$pr_home/state/config.json"
  # One valid peer, one broken (missing required keys, not even valid JSON)
  echo '{"name":"good-peer","host":"user@1.2.3.4","airc_home":"/tmp/x"}' > "$pr_home/state/peers/good.json"
  echo 'not-json-at-all' > "$pr_home/state/peers/broken.json"
  local peers_out
  peers_out=$(AIRC_HOME="$pr_home/state" "$AIRC" peers 2>&1)
  echo "$peers_out" | grep -q 'good-peer' && pass "malformed peer: valid peer still enumerated" \
                                          || fail "malformed peer: good-peer missing from output ($peers_out)"

  cleanup_all
}

scenario_reconnect() {
  section "reconnect: joiner survives host down/up cycle without manual intervention"
  cleanup_all

  # ── Setup: alpha hosts on 7549, beta joins ──────────────────────────
  spawn_host /tmp/airc-it-rec-h alpha 7549 || { fail "alpha host failed to start"; return; }
  pass "alpha hosting on 7549"

  local join; join=$(read_join_string /tmp/airc-it-rec-h)
  [ -n "$join" ] || { fail "no join string"; return; }

  spawn_joiner /tmp/airc-it-rec-j beta "$join" || { fail "beta join failed"; return; }
  pass "beta joined alpha"

  sleep 3

  # Baseline: pre-outage send must reach the joiner's monitor.
  as_home /tmp/airc-it-rec-j send @alpha "pre-outage" >/dev/null 2>&1 || true
  sleep 4
  grep -q 'pre-outage' /tmp/airc-it-rec-h/out.log \
    && pass "pre-outage message delivered to host" \
    || fail "pre-outage send didn't reach host (baseline broken — skip rest)"

  # ── Outage: kill alpha's process tree, keep state on disk ────────────
  # Non-flush teardown: identity + messages.jsonl survive for restart.
  AIRC_HOME=/tmp/airc-it-rec-h/state AIRC_PORT=7549 "$AIRC" teardown >/dev/null 2>&1
  sleep 1
  lsof -tiTCP:7549 -sTCP:LISTEN >/dev/null 2>&1 \
    && fail "alpha still listening after teardown (outage simulation failed)" \
    || pass "alpha down: port 7549 freed"

  # Beta's monitor should still be running — just retrying silently.
  local beta_pid; beta_pid=$(cat /tmp/airc-it-rec-j/state/airc.pid 2>/dev/null | head -1)
  [ -n "$beta_pid" ] && kill -0 "$beta_pid" 2>/dev/null \
    && pass "beta monitor still alive during outage (PID $beta_pid)" \
    || fail "beta monitor exited when alpha went down (should have retried)"

  sleep 4

  # ── Recovery: restart alpha with same home+port+identity ────────────
  # Re-spawn using same state dir so identity/keys/peers persist.
  # (Can't use spawn_host as-is because it mkdir's and overwrites state.
  #  Instead re-invoke connect directly pointing at the same state.)
  ( cd /tmp/airc-it-rec-h && AIRC_HOME=/tmp/airc-it-rec-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      "$AIRC" connect >> /tmp/airc-it-rec-h/out.log 2>&1 & )
  local i
  for i in 1 2 3 4 5 6 7 8; do
    sleep 1
    lsof -tiTCP:7549 -sTCP:LISTEN >/dev/null 2>&1 && break
  done
  lsof -tiTCP:7549 -sTCP:LISTEN >/dev/null 2>&1 \
    && pass "alpha back up on 7549" \
    || { fail "alpha didn't restart"; return; }

  # ── Critical: post-outage send must reach joiner without manual reconnect ──
  # Give beta's monitor one reconnect cycle (sleep 3 in the retry loop).
  sleep 5
  as_home /tmp/airc-it-rec-h send @beta "post-outage" >/dev/null 2>&1 || true

  # Beta's monitor tails host's messages.jsonl over SSH with offset resume.
  # Message should appear in beta's local mirror (monitor_formatter mirrors
  # joiner-side inbound to the local log for audit).
  local saw=0
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 1
    grep -q 'post-outage' /tmp/airc-it-rec-j/state/messages.jsonl 2>/dev/null && { saw=1; break; }
  done
  [ "$saw" = "1" ] \
    && pass "post-outage: beta monitor resumed and delivered message (${i}s after send)" \
    || fail "post-outage: beta monitor did NOT pick up new message within 15s"

  cleanup_all
}

scenario_queue() {
  section "queue: SSH-unreachable sends land in pending.jsonl, drain when host returns"
  cleanup_all

  # Realistic #5 scenario isn't "airc process killed on host" (SSH still up +
  # cat >> messages.jsonl still works without airc running). It's "host MACHINE
  # unreachable" — laptop asleep, network out, SSH times out. We simulate by
  # pointing host_target at an unreachable IP, then restoring it to test drain.

  spawn_host /tmp/airc-it-q-h qhost 7549 || { fail "qhost failed to start"; return; }
  local join; join=$(read_join_string /tmp/airc-it-q-h)
  spawn_joiner /tmp/airc-it-q-j qjoiner "$join" || { fail "qjoiner join failed"; return; }
  sleep 3

  # Snapshot the real host_target, then flip to an unreachable address.
  local real_target
  real_target=$(python3 -c "import json; print(json.load(open('/tmp/airc-it-q-j/state/config.json'))['host_target'])")
  [ -n "$real_target" ] || { fail "no host_target recorded in joiner config"; return; }

  python3 -c "
import json
p = '/tmp/airc-it-q-j/state/config.json'
c = json.load(open(p))
c['host_target'] = 'nobody@127.0.0.99'
json.dump(c, open(p, 'w'))
"
  # Also fake the peer record so resolution doesn't fail on @qhost
  echo '{"name":"qhost","host":"nobody@127.0.0.99","airc_home":"/tmp/nowhere"}' \
    > /tmp/airc-it-q-j/state/peers/qhost.json
  pass "joiner: host_target flipped to unreachable (outage simulation)"

  # ── Send during outage ─────────────────────────────────────────────
  AIRC_HOME=/tmp/airc-it-q-j/state "$AIRC" send @qhost "queued-during-outage" >/dev/null 2>&1
  local send_exit=$?
  [ $send_exit -eq 0 ] && pass "send during outage: exit 0 (queued is success)" \
                       || fail "send during outage: exit $send_exit — should queue gracefully, not die"

  local pending=/tmp/airc-it-q-j/state/pending.jsonl
  [ -f "$pending" ] && grep -q 'queued-during-outage' "$pending" \
    && pass "send during outage: message landed in pending.jsonl" \
    || fail "send during outage: pending.jsonl missing or empty"

  grep -q 'QUEUED' /tmp/airc-it-q-j/state/messages.jsonl \
    && pass "send during outage: [QUEUED] marker visible in local log" \
    || fail "send during outage: no [QUEUED] marker in local messages.jsonl"

  # ── Recovery: restore real host_target, wait for flush loop ─────────
  python3 -c "
import json
p = '/tmp/airc-it-q-j/state/config.json'
c = json.load(open(p))
c['host_target'] = '$real_target'
json.dump(c, open(p, 'w'))
"
  pass "joiner: host_target restored (recovery simulation)"

  # Flush loop on joiner polls every ~5s; give up to 25s.
  local delivered=0 drained=0
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25; do
    sleep 1
    grep -q 'queued-during-outage' /tmp/airc-it-q-h/state/messages.jsonl 2>/dev/null && delivered=1
    [ ! -s "$pending" ] && drained=1
    [ "$delivered" = "1" ] && [ "$drained" = "1" ] && break
  done
  [ "$delivered" = "1" ] && pass "recovery: queued message drained to host (${i}s)" \
                         || fail "recovery: queued message NOT delivered to host within 25s"
  [ "$drained" = "1" ] && pass "recovery: pending.jsonl cleared after successful drain" \
                       || fail "recovery: pending.jsonl still has content ($(wc -l < "$pending" 2>/dev/null) lines)"

  cleanup_all
}

case "$MODE" in
  tabs)        scenario_tabs  ;;
  scope)       scenario_scope ;;
  teardown)    scenario_teardown ;;
  reminder)    scenario_reminder ;;
  resilience)  scenario_resilience ;;
  reconnect)   scenario_reconnect ;;
  queue)       scenario_queue ;;
  all)         scenario_tabs; scenario_scope; scenario_reminder; scenario_teardown; scenario_resilience; scenario_reconnect; scenario_queue ;;
  *) echo "Usage: $0 [tabs|scope|teardown|reminder|resilience|reconnect|queue|all]"; exit 2 ;;
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
