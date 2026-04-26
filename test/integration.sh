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
  #
  # Resolve /tmp before walking — on macOS /tmp is a symlink to /private/tmp
  # and `find /tmp -maxdepth 1` does NOT traverse it without `-L` or
  # the canonical path. Without this the cleanup silently no-ops between
  # runs and stale identity / config / pidfiles leak forward, causing
  # spurious test failures (saw scenario_identity see "pronouns: they"
  # left over from a prior invocation, 2026-04-25).
  local tmpdir
  tmpdir=$(cd /tmp && pwd -P)   # /private/tmp on macOS, /tmp on Linux
  find "$tmpdir" -maxdepth 1 -name 'airc-it-*' -exec rm -rf {} + 2>/dev/null || true
}

cleanup_known_hosts() {
  # Test alpha/beta hosts run on the user's real SSH target, so their
  # pair handshake writes ephemeral test host keys into
  # ~/.ssh/known_hosts. Left behind, those stale keys collide with the
  # user's real airc host running on the same IP — SSH to the real host
  # fails with REMOTE HOST IDENTIFICATION HAS CHANGED. Clear any entries
  # for THIS machine's address between runs.
  #
  # We only clean addresses we discover dynamically:
  #   - hostname -I (Linux/WSL) primary local IP
  #   - ipconfig getifaddr en0 (macOS) primary interface
  #   - tailscale ip -4 (cross-platform) the tailnet address airc most
  #     commonly pairs over
  # No hardcoded IPs — the prior version pinned 100.91.51.87 (the airc
  # author's tailnet IP), which was a dead branch for any other user
  # AND a low-grade PII leak in the repo.
  local addr; addr=$(hostname -I 2>/dev/null | awk '{print $1}')
  [ -z "$addr" ] && addr=$(ipconfig getifaddr en0 2>/dev/null)
  if [ -n "$addr" ]; then
    ssh-keygen -R "$addr" -f "$HOME/.ssh/known_hosts" >/dev/null 2>&1 || true
  fi
  # Tailscale address (if up) — same machine, different routable IP.
  if command -v tailscale >/dev/null 2>&1; then
    local ts_ip; ts_ip=$(tailscale ip -4 2>/dev/null | head -1)
    if [ -n "$ts_ip" ]; then
      ssh-keygen -R "$ts_ip" -f "$HOME/.ssh/known_hosts" >/dev/null 2>&1 || true
    fi
  fi
}

cleanup_all() { cleanup_procs; cleanup_dirs; cleanup_known_hosts; }

# Boot a host. Args: home, name, port
#
# Defaults to --no-general --no-gist for two reasons:
# (1) These existing scenarios test the LOWER-layer single-pair invite
#     behavior, not the IRC substrate. With #39's defaults, bare
#     `airc connect` would create a real `airc room: general` gist on
#     the user's gh account and pollute the test environment for every
#     subsequent scenario that bare-connects.
# (2) Tests must run gh-free in CI; --no-gist is the explicit opt-out.
# Scenarios that DO want substrate behavior (scenario_room) call airc
# directly with their own flags rather than going through spawn_host.
spawn_host() {
  local home="$1" name="$2" port="$3"
  mkdir -p "$home"
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME="$name" AIRC_PORT="$port" \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-general --no-gist > "$home/out.log" 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -q 'Hosting as' "$home/out.log" 2>/dev/null && return 0
  done
  return 1
}

# Join a host. Args: home, name, join-string
#
# AIRC_NO_DISCOVERY=1 also for tests — the joiner's target is always an
# inline invite string in the existing scenarios; we don't want it
# probing gh for a #general gist that may have been created out-of-band.
spawn_joiner() {
  local home="$1" name="$2" join="$3"
  mkdir -p "$home"
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME="$name" \
      AIRC_NO_DISCOVERY=1 \
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
  # Joel 2026-04-24: rename print format changed from 'Peer renamed: <old> -> <new>'
  # to 'nick: <old> → <new>' (IRC-canonical). Match the new format; old-format
  # backward-compat is intentionally NOT kept since the wire protocol [rename]
  # marker is what peers actually exchange — only the human-visible print changed.
  grep -qE 'nick.*alpha.*gamma|Peer renamed' /tmp/airc-it-j/out.log && pass "beta saw [rename] marker" \
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
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-general --no-gist > "$home/out.log" 2>&1 & )
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
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-general --no-gist > "$sp_home/out.log" 2>&1 & )
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
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-general --no-gist >> /tmp/airc-it-rec-h/out.log 2>&1 & )
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

scenario_status() {
  section "status: liveness view reflects identity, monitor, queue, last-activity"
  cleanup_all

  spawn_host /tmp/airc-it-s-h shost 7549 || { fail "shost failed to start"; return; }
  local join; join=$(read_join_string /tmp/airc-it-s-h)
  spawn_joiner /tmp/airc-it-s-j sjoiner "$join" || { fail "sjoiner join failed"; return; }
  sleep 2

  # Host status: should show "hosting on port 7549" + monitor running
  local h_out
  h_out=$(AIRC_HOME=/tmp/airc-it-s-h/state "$AIRC" status 2>&1)
  echo "$h_out" | grep -q 'hosting on port 7549' && pass "host status: identity line reads 'hosting on port 7549'" \
                                                 || fail "host status missing port (got: $h_out)"
  echo "$h_out" | grep -Eq 'monitor:\s+running' && pass "host status: monitor shown running" \
                                                || fail "host status: monitor not shown running"
  echo "$h_out" | grep -q 'queue:.*empty' && pass "host status: queue empty (no pending)" \
                                          || fail "host status: queue line wrong"

  # Joiner status: should show "joiner of shost"
  local j_out
  j_out=$(AIRC_HOME=/tmp/airc-it-s-j/state "$AIRC" status 2>&1)
  echo "$j_out" | grep -q 'joiner of' && pass "joiner status: identity line shows joiner role" \
                                      || fail "joiner status missing joiner-of line (got: $j_out)"
  echo "$j_out" | grep -q ':7549' && pass "joiner status: host port visible" \
                                  || fail "joiner status missing host port"

  # Send a message then assert status reflects activity
  as_home /tmp/airc-it-s-j send @shost "status-probe" >/dev/null 2>&1
  sleep 1
  local j_out2; j_out2=$(AIRC_HOME=/tmp/airc-it-s-j/state "$AIRC" status 2>&1)
  echo "$j_out2" | grep -Eq 'last send:\s+[0-9]+s ago' && pass "joiner status: last-send shows elapsed seconds" \
                                                       || fail "joiner status: last send not updated (got: $(echo "$j_out2" | grep 'last send'))"

  # Pending queue: simulate an outage by flipping host_target and sending, then assert queue size.
  # Reuse the same fake-target pattern as scenario_queue.
  python3 -c "
import json
p = '/tmp/airc-it-s-j/state/config.json'
c = json.load(open(p))
c['_real_host_target'] = c['host_target']
c['host_target'] = 'nobody@127.0.0.99'
json.dump(c, open(p, 'w'))
"
  echo '{"name":"shost","host":"nobody@127.0.0.99","airc_home":"/tmp/nowhere"}' > /tmp/airc-it-s-j/state/peers/shost.json
  AIRC_HOME=/tmp/airc-it-s-j/state "$AIRC" send @shost "status-queue-probe" >/dev/null 2>&1 || true
  local j_out3; j_out3=$(AIRC_HOME=/tmp/airc-it-s-j/state "$AIRC" status 2>&1)
  echo "$j_out3" | grep -Eq 'queue:\s+[1-9][0-9]* pending' \
    && pass "joiner status: queue shows 1+ pending after outage send" \
    || fail "joiner status: queue line didn't reflect pending (got: $(echo "$j_out3" | grep 'queue'))"

  cleanup_all
}

scenario_auth_failure() {
  section "auth_failure: fresh-install joiner with stale authorized_keys must fail LOUDLY"
  cleanup_all

  # This scenario mimics the exact situation memento hit today: a joiner
  # reinstalls airc (regenerating identity keys), then runs `airc connect`
  # with no args (resume from saved pairing). The host still has the OLD
  # authorized_keys, so SSH auth fails. Pre-this-fix, cmd_send silently
  # queued with a misleading "Host unreachable" message and exit 0 — the
  # user thought their send succeeded when nothing reached the host.
  #
  # Correct behavior: auth failure is fundamentally different from a
  # transient network error. Retry won't help — every attempt auths with
  # the same (wrong) key. Must die() with clear stderr + repair instructions.

  spawn_host /tmp/airc-it-af-h afhost 7549 || { fail "afhost failed to start"; return; }
  local join; join=$(read_join_string /tmp/airc-it-af-h)
  spawn_joiner /tmp/airc-it-af-j afjoiner "$join" || { fail "afjoiner join failed"; return; }
  sleep 3

  # Baseline: normal send works (pair-handshake added joiner's key).
  as_home /tmp/airc-it-af-j send @afhost "pre-reinstall" >/dev/null 2>&1 \
    && pass "baseline: send to host works after fresh pair" \
    || { fail "baseline send broken — can't set up auth-fail test"; return; }

  # ── Simulate joiner reinstall: regenerate identity keys in-place,
  # keeping config.json (host_target etc) intact so `airc connect` with no
  # args resumes with the stale host pairing. Host's authorized_keys still
  # has the ORIGINAL joiner key, not the new one.
  rm -f /tmp/airc-it-af-j/state/identity/ssh_key \
        /tmp/airc-it-af-j/state/identity/ssh_key.pub
  ssh-keygen -t ed25519 -f /tmp/airc-it-af-j/state/identity/ssh_key \
             -N '' -q -C 'airc-fresh-reinstall' 2>/dev/null

  # ── The test: joiner tries `airc send`. Expected: die loudly with
  # auth stderr + repair instructions. NOT silent queue.
  local err_file; err_file=$(mktemp -t airc-af-err.XXXXXX)
  AIRC_HOME=/tmp/airc-it-af-j/state "$AIRC" send @afhost "post-reinstall" >/dev/null 2>"$err_file"
  local af_exit=$?

  [ $af_exit -ne 0 ] && pass "auth failure: cmd_send exits non-zero (was $af_exit)" \
                     || fail "auth failure: cmd_send exited 0 — silent regression"

  grep -qiE 'auth|permission|publickey' "$err_file" \
    && pass "auth failure: stderr surfaces the actual SSH error" \
    || fail "auth failure: stderr doesn't mention auth (got: $(cat "$err_file"))"

  grep -qE 'teardown --flush|re-pair|invite' "$err_file" \
    && pass "auth failure: stderr tells user HOW to fix (re-pair command)" \
    || fail "auth failure: no repair guidance in stderr (got: $(cat "$err_file"))"

  # Critically: message must NOT have been queued. Every retry would fail
  # the same way, so queuing creates user confusion + log spam.
  if [ -f /tmp/airc-it-af-j/state/pending.jsonl ]; then
    grep -q 'post-reinstall' /tmp/airc-it-af-j/state/pending.jsonl \
      && fail "auth failure: message WAS queued — will retry-fail forever" \
      || pass "auth failure: message not queued (correct — retry wouldn't help)"
  else
    pass "auth failure: no pending.jsonl created (correct — retry wouldn't help)"
  fi

  # And the host's messages.jsonl must NOT contain the post-reinstall message.
  grep -q 'post-reinstall' /tmp/airc-it-af-h/state/messages.jsonl 2>/dev/null \
    && fail "auth failure: message somehow reached host (how? auth was broken)" \
    || pass "auth failure: host correctly never received the message"

  rm -f "$err_file"
  cleanup_all
}

scenario_resume_stale_auth() {
  section "resume_stale_auth: teardown + resume with stale SSH key must fail LOUDLY, not silently"
  cleanup_all

  # This is the "default broken" path joel flagged. A user runs `airc teardown`
  # (without --flush, so saved pairing stays) and then `airc connect` (no args,
  # resume path). If their SSH key has been invalidated on the host — by a
  # reinstall regenerating identity keys, by authorized_keys rotation, by ANY
  # cause — the old resume path silently started a tail loop that retried
  # forever while the user waited for a mesh that was never coming back.

  spawn_host /tmp/airc-it-rsa-h rsahost 7549 || { fail "rsahost failed to start"; return; }
  local join; join=$(read_join_string /tmp/airc-it-rsa-h)
  spawn_joiner /tmp/airc-it-rsa-j rsajoiner "$join" || { fail "rsajoiner join failed"; return; }
  sleep 3

  # Baseline: confirm fresh pair works
  as_home /tmp/airc-it-rsa-j send @rsahost "baseline" >/dev/null 2>&1 \
    && pass "baseline: fresh pair works" || { fail "baseline broken"; return; }

  # ── Simulate the stale-auth state: kill the joiner (non-flush — preserves
  # config.json + identity + peer records), then regenerate the identity key
  # BEHIND the host's back (the host's authorized_keys still has the old key).
  AIRC_HOME=/tmp/airc-it-rsa-j/state AIRC_PORT=7549 "$AIRC" teardown >/dev/null 2>&1
  sleep 1
  rm -f /tmp/airc-it-rsa-j/state/identity/ssh_key \
        /tmp/airc-it-rsa-j/state/identity/ssh_key.pub
  ssh-keygen -t ed25519 -f /tmp/airc-it-rsa-j/state/identity/ssh_key \
             -N '' -q -C 'airc-stale-post-reinstall' 2>/dev/null

  # ── Now attempt a resume. PRE-FIX: silently starts a tail loop that
  # retries forever. POST-FIX: auth probe detects the stale key and dies.
  local resume_out; resume_out=$(mktemp -t airc-rsa-out.XXXXXX)
  local resume_err; resume_err=$(mktemp -t airc-rsa-err.XXXXXX)
  # Background with a timeout so the pre-fix silent-loop doesn't hang the test.
  ( AIRC_HOME=/tmp/airc-it-rsa-j/state "$AIRC" connect >"$resume_out" 2>"$resume_err" ) &
  local resume_pid=$!
  # Give it up to 10s to either exit (post-fix) or go silent into the tail
  # retry loop (pre-fix — we'll kill it).
  local exited=0 i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    if ! kill -0 $resume_pid 2>/dev/null; then exited=1; break; fi
  done
  if [ "$exited" = "0" ]; then
    # Still running after 10s — pre-fix behavior. Kill it, record the failure.
    kill -9 $resume_pid 2>/dev/null
    fail "resume_stale_auth: connect still running after 10s — silent retry loop (pre-fix bug)"
  else
    pass "resume_stale_auth: connect exited promptly (${i}s) rather than silently looping"
  fi

  wait $resume_pid 2>/dev/null
  local resume_exit=$?

  # Post-fix expectations
  if [ "$resume_exit" -ne 0 ]; then
    pass "resume_stale_auth: connect exited non-zero ($resume_exit) on stale auth"
  else
    fail "resume_stale_auth: connect exited 0 despite broken auth"
  fi

  grep -qiE 'auth|permission|publickey' "$resume_err" \
    && pass "resume_stale_auth: stderr surfaces the SSH auth error" \
    || fail "resume_stale_auth: stderr doesn't mention auth (got: $(cat "$resume_err"))"

  grep -qE 'teardown --flush' "$resume_err" \
    && pass "resume_stale_auth: stderr tells user HOW to fix" \
    || fail "resume_stale_auth: no --flush repair command in stderr"

  grep -qE 'invite string' "$resume_err" \
    && pass "resume_stale_auth: stderr reconstructs the saved invite string for convenience" \
    || fail "resume_stale_auth: no reconstructed invite string in stderr"

  rm -f "$resume_out" "$resume_err"
  cleanup_all
}

# ── Scenario: room (#39 — IRC-style #general substrate) ────────────────
# Validates the room-mode flag plumbing, host-vs-joiner detection in
# cmd_part, and that --no-gist still records local room state. Doesn't
# touch GitHub at all (no gh dependency); all wire-level pairing reuses
# the long-invite handshake the rest of the suite already proves.
#
# What we DO test:
#   - --room flag accepted; banner reports "Hosting #<name> (gh-account substrate)"
#   - room_name file written under AIRC_HOME (even with --no-gist)
#   - joiner pairs via inline invite and bidirectional send works
#   - cmd_part on host: detects host via config.host_target absence, runs
#     teardown, removes room_name file, doesn't try to gh-delete (no
#     gist_id stored under --no-gist)
#   - cmd_part on joiner: reports joiner status, removes room_name only,
#     leaves identity intact
#
# What we explicitly DON'T test (out of scope; covered by manual e2e
# w/ real gh + the next PR's multi-room work):
#   - Discovery of an existing #general gist on the gh account
#   - Persistence of a room gist after pair (the gist itself isn't
#     created here — `--no-gist` keeps the test gh-free)
#   - Multi-joiner room (one host, N joiners) — single-joiner here
#     proves the flag path; N-joiner is a topology test, not a flag test
scenario_room() {
  section "room: #39 IRC-style substrate (--room + cmd_part, no gh)"
  cleanup_all

  local rname="test-irc-$$"

  # ── Host alpha in room mode, gist push disabled so the test runs
  #    in any environment (CI, gh-less workstations).
  mkdir -p /tmp/airc-it-h
  ( cd /tmp/airc-it-h && AIRC_HOME=/tmp/airc-it-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-gist --room "$rname" > /tmp/airc-it-h/out.log 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -q 'Hosting as' /tmp/airc-it-h/out.log 2>/dev/null && break
  done
  grep -q 'Hosting as' /tmp/airc-it-h/out.log \
    && pass "alpha hosting in room mode (--room ${rname}, --no-gist)" \
    || { fail "alpha host failed to start in room mode"; cleanup_all; return; }

  # Banner asserts substrate framing: "Hosting #<name>" must appear so
  # users (and the AI agent) can tell which channel they're on.
  grep -qE "Hosting #${rname}" /tmp/airc-it-h/out.log \
    && pass "alpha banner reports #${rname} (substrate framing)" \
    || fail "alpha banner missing 'Hosting #${rname}' line"

  # room_name file MUST be on disk even with --no-gist. cmd_part + status
  # + diagnostics rely on it.
  [ -f /tmp/airc-it-h/state/room_name ] && [ "$(cat /tmp/airc-it-h/state/room_name)" = "$rname" ] \
    && pass "alpha room_name file recorded ($(cat /tmp/airc-it-h/state/room_name))" \
    || fail "alpha room_name file missing or wrong value"

  # No gist was pushed → no room_gist_id (this is the bug we just fixed:
  # cmd_part previously used gist_id presence as the host-vs-joiner
  # signal, which would misclassify --no-gist hosts as joiners).
  [ ! -f /tmp/airc-it-h/state/room_gist_id ] \
    && pass "alpha has no room_gist_id (--no-gist as expected)" \
    || fail "alpha unexpectedly wrote room_gist_id under --no-gist"

  # ── Joiner beta pairs via inline invite (long form, gh-free).
  local join; join=$(read_join_string /tmp/airc-it-h)
  [ -n "$join" ] && pass "alpha join string captured for beta to use" \
                 || { fail "no join string in alpha log"; cleanup_all; return; }

  spawn_joiner /tmp/airc-it-j beta "$join" \
    && pass "beta joined alpha's room" \
    || { fail "beta join failed"; cleanup_all; return; }

  # Bidirectional send still works through a room (room-ness is purely
  # at the discovery + lifecycle layer; the wire is unchanged).
  sleep 3
  as_home /tmp/airc-it-j send @alpha "room-msg-from-beta" >/dev/null 2>&1 \
    && pass "beta → alpha send through room works" \
    || fail "beta → alpha send through room FAILED"
  sleep 3
  grep -q 'room-msg-from-beta' /tmp/airc-it-h/out.log \
    && pass "alpha received beta's message through room" \
    || fail "alpha did NOT receive beta's message"

  # ── cmd_part on JOINER (beta).
  # Joiner has host_target in config → cmd_part takes joiner branch:
  # removes room_name only, doesn't touch gist (we have none anyway),
  # then runs teardown.
  local part_out
  part_out=$(as_home /tmp/airc-it-j part 2>&1)
  echo "$part_out" | grep -q 'Joiner of #' \
    && pass "beta cmd_part identifies as joiner (config.host_target detection)" \
    || fail "beta cmd_part DID NOT identify as joiner: $part_out"
  echo "$part_out" | grep -qE 'gh.*delete|gist delete' \
    && fail "beta cmd_part attempted gh delete (joiner shouldn't)" \
    || pass "beta cmd_part correctly skipped gh delete (joiner)"
  [ ! -f /tmp/airc-it-j/state/room_name ] \
    && pass "beta room_name removed after part" \
    || fail "beta room_name still present after part"

  # ── cmd_part on HOST (alpha).
  # Host has no host_target → cmd_part takes host branch. With --no-gist
  # there's no gist_id, so it should report "no gist was published"
  # rather than mis-routing into joiner branch (the bug we just fixed).
  part_out=$(as_home /tmp/airc-it-h part 2>&1)
  echo "$part_out" | grep -q 'Host of #' \
    && pass "alpha cmd_part identifies as host (config no host_target)" \
    || fail "alpha cmd_part DID NOT identify as host: $part_out"
  echo "$part_out" | grep -q 'no gist was published' \
    && pass "alpha cmd_part correctly noted absent gist (--no-gist host case)" \
    || fail "alpha cmd_part didn't acknowledge --no-gist case: $part_out"
  [ ! -f /tmp/airc-it-h/state/room_name ] \
    && pass "alpha room_name removed after part" \
    || fail "alpha room_name still present after part"

  cleanup_all
}

# ── Scenario: events (Joel's monitor-preview ask) ──────────────────────
# Joel 2026-04-24: "Anvil joined" instead of generic "monitor yada yada"
# in Monitor task notifications. The preview comes from messages.jsonl
# lines with from=airc; the formatter renders them as `[#room] airc:`.
# Without lifecycle events flowing through the log, Monitor's <summary>
# falls back to whatever stale chat line was latest — telling humans
# nothing about what just happened.
#
# What we verify:
#   - After successful pair, host's messages.jsonl contains a system
#     event line with from=airc and msg matching '<peer> joined #<room>'
#   - The line lands within a few seconds of pair (not stuck behind
#     the formatter's own loop)
scenario_events() {
  section "events: pair-handshake emits 'beta joined #<room>' system event"
  cleanup_all

  local rname="test-events-$$"

  mkdir -p /tmp/airc-it-h
  ( cd /tmp/airc-it-h && AIRC_HOME=/tmp/airc-it-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-gist --room "$rname" > /tmp/airc-it-h/out.log 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -q 'Hosting as' /tmp/airc-it-h/out.log 2>/dev/null && break
  done
  grep -q 'Hosting as' /tmp/airc-it-h/out.log \
    && pass "alpha hosting (--room ${rname}, --no-gist)" \
    || { fail "alpha host failed to start"; cleanup_all; return; }

  local join; join=$(read_join_string /tmp/airc-it-h)
  [ -n "$join" ] && pass "alpha join string captured" \
                 || { fail "no join string in alpha log"; cleanup_all; return; }

  spawn_joiner /tmp/airc-it-j beta "$join" \
    && pass "beta joined alpha's room" \
    || { fail "beta join failed"; cleanup_all; return; }

  # Allow up to ~5s for the pair-accept python to finish writing the
  # event line. The handshake itself completes in <1s; the event-emit
  # is wrapped in try/except so any path that fails doesn't break the
  # pair, which means we need to check what actually landed.
  local seen=""
  for i in 1 2 3 4 5; do
    if [ -f /tmp/airc-it-h/state/messages.jsonl ] \
       && grep -q '"from": *"airc"' /tmp/airc-it-h/state/messages.jsonl \
       && grep -q "beta joined #${rname}" /tmp/airc-it-h/state/messages.jsonl; then
      seen="yes"
      break
    fi
    sleep 1
  done
  [ -n "$seen" ] \
    && pass "host messages.jsonl contains 'beta joined #${rname}' event line" \
    || fail "no 'beta joined' event line in host's messages.jsonl after 5s"

  # The event must be JSON-parseable and have the structure the formatter
  # expects (from=airc, to=all, msg + ts present). Otherwise it'll be
  # silently skipped by the monitor formatter's json.loads guard.
  if [ -n "$seen" ]; then
    python3 -c "
import json,sys
ok=False
for line in open('/tmp/airc-it-h/state/messages.jsonl'):
    try:
        m=json.loads(line)
    except Exception:
        continue
    if m.get('from')=='airc' and 'beta joined' in m.get('msg',''):
        if m.get('to')=='all' and m.get('ts'):
            ok=True
sys.exit(0 if ok else 1)
" 2>/dev/null \
      && pass "event has required fields (from=airc, to=all, ts, msg)" \
      || fail "event line malformed — formatter will skip it"
  fi

  cleanup_all
}

# ── Scenario: get_host (LAN IP fallback when Tailscale absent/disabled) ─
# Per Joel: Tailscale should be optional for same-LAN use. The new
# get_host priority is Tailscale → LAN-IP-via-UDP-trick → hostname.
# AIRC_NO_TAILSCALE=1 forces fallback for testing AND for LAN-only users.
#
# What we verify (no gh, no SSH — pure host-resolution test):
#   - Default returns SOMETHING non-empty (could be tailscale ip, lan ip,
#     or hostname depending on the machine the test runs on)
#   - AIRC_NO_TAILSCALE=1 doesn't error and returns SOMETHING non-empty
#     (LAN ip on most machines; hostname if no internet route)
#   - When forced fallback returns an IP-shaped value, it's a valid
#     RFC1918 LAN range (192.168/10/172.16-31) — i.e. routable on the
#     local network, not the loopback noise we explicitly filter for
scenario_get_host() {
  section "get_host: priority Tailscale → LAN-IP → hostname"

  local default_host
  default_host=$("$AIRC" debug-host 2>/dev/null || echo "")
  [ -n "$default_host" ] \
    && pass "default get_host returned non-empty: $default_host" \
    || fail "default get_host returned empty"

  local fallback_host
  fallback_host=$(AIRC_NO_TAILSCALE=1 "$AIRC" debug-host 2>/dev/null || echo "")
  [ -n "$fallback_host" ] \
    && pass "AIRC_NO_TAILSCALE=1 fallback returned non-empty: $fallback_host" \
    || fail "AIRC_NO_TAILSCALE=1 fallback returned empty"

  # If fallback looks like an IPv4 address, it must NOT be 127.* (we
  # explicitly filter loopback in get_host) and SHOULD be RFC1918 if
  # the test runner has typical home/office LAN routing.
  case "$fallback_host" in
    127.*)
      fail "fallback returned loopback ($fallback_host) — get_host's UDP-trick filter regressed"
      ;;
    192.168.*|10.*|172.16.*|172.17.*|172.18.*|172.19.*|172.2[0-9].*|172.3[01].*)
      pass "fallback is RFC1918 LAN address ($fallback_host) — UDP-trick worked"
      ;;
    [0-9]*.[0-9]*.[0-9]*.[0-9]*)
      pass "fallback is an IPv4 ($fallback_host) — non-RFC1918 but routable"
      ;;
    *)
      pass "fallback returned hostname-style value ($fallback_host) — UDP-trick path skipped (no internet route or no python3)"
      ;;
  esac

  # Determinism: same env, same call → same value (no flapping).
  local repeat
  repeat=$(AIRC_NO_TAILSCALE=1 "$AIRC" debug-host 2>/dev/null || echo "")
  [ "$repeat" = "$fallback_host" ] \
    && pass "fallback is stable across repeated calls" \
    || fail "fallback flapped: '$fallback_host' then '$repeat'"
}

# ── Scenario: mnemonic (humanhash → gist id resolver) ──────────────────
# Per Joel's UX target: a friend can type
#   airc connect oregon-uncle-bravo-eleven
# instead of a 32-char hex gist id. Same-account resolution = walk
# `gh gist list`, hash each id, match against the input phrase.
#
# This scenario runs as a unit-style test (no host/joiner spawning):
#   - Word-form input is detected (regex match)
#   - Hex-form input is NOT misclassified
#   - Without gh OR with no matching gist, fails LOUD with actionable error
#
# We don't depend on a real gh account here — instead we exercise the
# detection regex by capturing airc's stderr/exit-code on a known-bad
# mnemonic. The actual gh resolution path is exercised in dogfood (the
# `Peer joined` event from the live host monitor when a test process
# resolves the room mnemonic against the real gh account).
scenario_mnemonic() {
  section "mnemonic: humanhash → gist id resolver detection + error path"

  # 1. Word-form (3+ hyphens, lowercase alpha) triggers the resolver.
  #    Without gh, dies with mnemonic-needs-gh message.
  #    With gh + no match, dies with no-match message.
  # We run airc connect with the test bogus mnemonic in an isolated
  # AIRC_HOME so we don't touch the user's real state.
  local thome=/tmp/airc-it-mnem-$$
  mkdir -p "$thome"

  local out
  out=$(AIRC_HOME="$thome" AIRC_NO_DISCOVERY=1 "$AIRC" connect zzzz-yyyy-xxxx-wwww 2>&1 || true)

  # If gh is on PATH, expect the no-match error. If gh is missing,
  # expect the install-gh error. Either way: must mention 'mnemonic'
  # and exit non-zero.
  if echo "$out" | grep -qi 'mnemonic'; then
    pass "word-form input detected as mnemonic + dispatched to resolver"
  else
    fail "word-form input did NOT route through mnemonic resolver: $out"
  fi

  # 2. Hex-form input must NOT be misclassified. We don't actually pair —
  # just check that the mnemonic resolver doesn't fire (the gist resolver
  # downstream will fire instead, and either succeed or fail differently).
  out=$(AIRC_HOME="$thome" AIRC_NO_DISCOVERY=1 "$AIRC" connect 2f6a907224f4b88d236fda8ca16d37c4 2>&1 || true)
  if ! echo "$out" | grep -qi "didn't match any airc gist on this gh account"; then
    pass "hex-form input not misclassified as mnemonic"
  else
    fail "hex-form input incorrectly routed through mnemonic resolver: $out"
  fi

  # 3. A 1-hyphen string (looks like a CLI flag value, not a mnemonic)
  # should NOT trigger the resolver. The regex requires 2+ hyphens.
  out=$(AIRC_HOME="$thome" AIRC_NO_DISCOVERY=1 "$AIRC" connect foo-bar 2>&1 || true)
  if ! echo "$out" | grep -qi "didn't match any airc gist"; then
    pass "1-hyphen string ('foo-bar') not misclassified as mnemonic"
  else
    fail "1-hyphen string incorrectly routed through mnemonic resolver: $out"
  fi

  rm -rf "$thome"
}

# ── Scenario: identity (issue #34, v1) ─────────────────────────────────
# Identity layer = pronouns/role/bio/status/integrations stored on top of
# the bootstrap name from derive_name. v1 surface: airc identity
# show/set/link locally; airc whois on self prints the same. Cross-peer
# WHOIS over SSH is the v2 cut.
scenario_identity() {
  section "identity: airc identity show/set/link + airc whois self"
  cleanup_all
  local home=/tmp/airc-it-id
  local name=alpha-id
  local port=7549
  mkdir -p "$home"

  # Spin up a host so config.json gets written (identity helpers
  # require ensure_init).
  ( cd "$home" && AIRC_HOME="$home/state" AIRC_NAME="$name" AIRC_PORT="$port" \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-general --no-gist > "$home/out.log" 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -q 'Hosting as' "$home/out.log" 2>/dev/null && break
  done
  grep -q 'Hosting as' "$home/out.log" 2>/dev/null \
    && pass "host spawned for identity scenario" \
    || { fail "host did not start; aborting identity scenario"; cleanup_all; return; }

  # Small settle pause: the "Hosting as" banner can fire fractionally
  # before the python config-merge subprocess flushes config.json under
  # heavy concurrent test load. Without this, identity show occasionally
  # reads a half-written config and misses the (unset) defaults.
  sleep 1

  # ── show on empty identity ──
  local out
  out=$(AIRC_HOME="$home/state" "$AIRC" identity show 2>&1)
  echo "$out" | grep -q "name: *$name" \
    && pass "identity show prints the derived name" \
    || fail "identity show missing name (got: $out)"
  echo "$out" | grep -q "pronouns: *(unset)" \
    && pass "pronouns default to (unset) on fresh init" \
    || fail "pronouns field missing or wrong default (got: $out)"
  echo "$out" | grep -q "integrations: *(none)" \
    && pass "integrations default to (none)" \
    || fail "integrations field missing or wrong default (got: $out)"

  # ── set ──
  AIRC_HOME="$home/state" "$AIRC" identity set \
    --pronouns they --role test-role --bio "test bio line" --status "running scenario_identity" >/dev/null 2>&1 \
    && pass "identity set returned ok" \
    || fail "identity set returned nonzero"

  # ── show round-trip ──
  out=$(AIRC_HOME="$home/state" "$AIRC" identity show 2>&1)
  echo "$out" | grep -q "pronouns: *they" && pass "pronouns=they round-trips" || fail "pronouns missing post-set"
  echo "$out" | grep -q "role: *test-role" && pass "role=test-role round-trips" || fail "role missing post-set"
  echo "$out" | grep -q "bio: *test bio line" && pass "bio round-trips" || fail "bio missing post-set"
  echo "$out" | grep -q "status: *running scenario_identity" && pass "status round-trips" || fail "status missing post-set"

  # ── partial set (only --status) ──
  AIRC_HOME="$home/state" "$AIRC" identity set --status "second status" >/dev/null 2>&1
  out=$(AIRC_HOME="$home/state" "$AIRC" identity show 2>&1)
  echo "$out" | grep -q "status: *second status" && pass "partial set updates only --status" || fail "partial set didn't update status"
  echo "$out" | grep -q "pronouns: *they" && pass "partial set preserves untouched fields" || fail "partial set wiped other fields"

  # ── link / unlink ──
  AIRC_HOME="$home/state" "$AIRC" identity link continuum Earl >/dev/null 2>&1
  AIRC_HOME="$home/state" "$AIRC" identity link slack U07ABC123 >/dev/null 2>&1
  out=$(AIRC_HOME="$home/state" "$AIRC" identity show 2>&1)
  echo "$out" | grep -q "continuum: *Earl" && pass "link continuum=Earl recorded" || fail "continuum link missing"
  echo "$out" | grep -q "slack: *U07ABC123" && pass "link slack recorded" || fail "slack link missing"

  AIRC_HOME="$home/state" "$AIRC" identity link continuum >/dev/null 2>&1   # empty handle = unlink
  out=$(AIRC_HOME="$home/state" "$AIRC" identity show 2>&1)
  echo "$out" | grep -q "continuum:" \
    && fail "empty-handle link should unlink continuum" \
    || pass "empty-handle link unlinks continuum"
  echo "$out" | grep -q "slack: *U07ABC123" && pass "unlinking continuum preserves slack link" || fail "unlinking continuum nuked slack too"

  # ── whois self ──
  out=$(AIRC_HOME="$home/state" "$AIRC" whois "$name" 2>&1)
  echo "$out" | grep -q "pronouns: *they" \
    && pass "whois <self> prints identity blob" \
    || fail "whois <self> missing identity"

  # ── whois unknown peer ──
  out=$(AIRC_HOME="$home/state" "$AIRC" whois ghost-zzzz 2>&1 || true)
  echo "$out" | grep -q "no record for 'ghost-zzzz'" \
    && pass "whois on unknown peer prints helpful error" \
    || fail "whois on unknown peer didn't print expected error (got: $out)"

  # ── persistence across teardown (no flush) + reread ──
  AIRC_HOME="$home/state" "$AIRC" teardown >/dev/null 2>&1 || true
  out=$(AIRC_HOME="$home/state" "$AIRC" identity show 2>&1)
  echo "$out" | grep -q "pronouns: *they" \
    && pass "identity survives airc teardown (no flush)" \
    || fail "identity wiped after teardown — should only happen on --flush"

  # ── airc nick post-sanitization can't produce a leading dash ──
  # Input like ".foo" used to slip past the leading-dash check (the
  # case check fires BEFORE sanitization, then `.` → `-` produces
  # "-foo" which made the resulting nick unreachable to airc whois /
  # airc kick). Now stripped post-sanitization. Verify by setting a
  # nick that would have triggered the bug and asserting the stored
  # name has no leading dash.
  AIRC_HOME="$home/state" "$AIRC" nick ".dottyname" >/dev/null 2>&1 || true
  local renamed; renamed=$(python3 -c "import json; print(json.load(open('$home/state/config.json')).get('name',''))" 2>/dev/null)
  case "$renamed" in
    -*) fail "airc nick produced leading-dash name '$renamed' — sanitization regression" ;;
    "") fail "airc nick wrote empty name — sanitization regression" ;;
    *)  pass "airc nick strips leading dash post-sanitization (got '$renamed')" ;;
  esac

  cleanup_all
}

# ── Scenario: whois (issue #34, v2) ────────────────────────────────────
# Identity gets exchanged at pair-handshake time. Verify:
#   - Joiner's identity lands in host's peer file
#   - Host's identity lands in joiner's config under host_identity
#   - airc whois <joiner-name> works on the host (local peer file)
#   - airc whois <host-name> works on the joiner (cached host_identity)
scenario_whois() {
  section "whois: identity exchanged at handshake (host ↔ joiner)"
  cleanup_all

  spawn_host /tmp/airc-it-w-h whost 7549 || { fail "whost failed to start"; return; }
  # Set host identity BEFORE the joiner pairs so the handshake response
  # carries it. (Re-spawn semantics: changing identity then airc connect
  # again is the natural flow; for a test we set after spawn and assume
  # the next handshake reads fresh — verified below.)
  AIRC_HOME=/tmp/airc-it-w-h/state "$AIRC" identity set \
    --pronouns they --role host-role --bio "the host bio" --status "host status" >/dev/null 2>&1

  local join; join=$(read_join_string /tmp/airc-it-w-h)
  spawn_joiner /tmp/airc-it-w-j wjoiner "$join" || { fail "wjoiner join failed"; return; }
  sleep 1

  # Joiner sets identity AFTER pairing — handshake-time identity is empty
  # in this slot (matches the realistic flow: agent gets prompted to set
  # identity post-pair). Host's stored peer.identity will be empty for
  # this joiner; that's expected. Test the host→joiner direction here;
  # full bidirectional sync at handshake-time is exercised by scenario_kick
  # which sets joiner identity before pair.
  AIRC_HOME=/tmp/airc-it-w-j/state "$AIRC" identity set \
    --pronouns she --role joiner-role --bio "the joiner bio" >/dev/null 2>&1

  # ── Joiner: airc whois <host-name> reads host_identity from config ──
  local out
  out=$(AIRC_HOME=/tmp/airc-it-w-j/state "$AIRC" whois whost 2>&1)
  echo "$out" | grep -q "pronouns: *they" && pass "joiner can whois host (pronouns)" || fail "joiner whois host missing pronouns (got: $out)"
  echo "$out" | grep -q "role: *host-role" && pass "joiner can whois host (role)" || fail "joiner whois host missing role"
  echo "$out" | grep -q "bio: *the host bio" && pass "joiner can whois host (bio)" || fail "joiner whois host missing bio"

  # ── Joiner whois on self still works (local) ──
  out=$(AIRC_HOME=/tmp/airc-it-w-j/state "$AIRC" whois wjoiner 2>&1)
  echo "$out" | grep -q "pronouns: *she" && pass "joiner whois self works post-pair" || fail "joiner whois self regressed"

  # ── Joiner whois on unknown peer still graceful ──
  out=$(AIRC_HOME=/tmp/airc-it-w-j/state "$AIRC" whois nobody 2>&1 || true)
  echo "$out" | grep -q "no record for 'nobody'" && pass "whois on unknown still graceful" || fail "whois unknown error message regressed"

  cleanup_all
}

# ── Scenario: kick (host-only peer eviction) ──────────────────────────
# Joiner sets identity FIRST, then pairs — so the host's peer file gets
# joiner.identity populated. Test:
#   - Host can `airc whois <joiner>` and see the joiner's pronouns/role/bio
#   - Host kicks the joiner
#   - Peer file is gone; joiner's pubkey removed from authorized_keys
#   - Joiner attempts kick → refuses (joiner role check)
scenario_kick() {
  section "kick: host removes paired peer + handshake identity exchange"
  cleanup_all

  # Joiner pre-sets identity in its OWN scope before pairing — but
  # spawn_joiner runs airc connect as part of pairing, which also writes
  # config.json fresh. So we initialize the joiner's identity by writing
  # config.json directly under AIRC_HOME ahead of spawn. The simpler
  # route: spawn host first, get the join string, use airc identity set
  # in the joiner's home BEFORE running airc connect, but that requires
  # ensure_init which needs an existing config. Workaround: spawn the
  # joiner, set identity, then teardown+reconnect. Cleanest for a test.
  spawn_host /tmp/airc-it-k-h khost 7549 || { fail "khost failed to start"; return; }
  local join; join=$(read_join_string /tmp/airc-it-k-h)
  spawn_joiner /tmp/airc-it-k-j kjoiner "$join" || { fail "kjoiner join failed"; return; }
  sleep 1

  # Joiner sets identity AFTER first pair — to land it in host's peer
  # file we need a re-handshake. teardown (no flush) + reconnect.
  AIRC_HOME=/tmp/airc-it-k-j/state "$AIRC" identity set \
    --pronouns he --role joined-with-id --bio "kick test joiner" >/dev/null 2>&1
  AIRC_HOME=/tmp/airc-it-k-j/state "$AIRC" teardown >/dev/null 2>&1 || true
  ( cd /tmp/airc-it-k-j && AIRC_HOME=/tmp/airc-it-k-j/state AIRC_NAME=kjoiner \
      AIRC_NO_DISCOVERY=1 "$AIRC" connect "$join" > /tmp/airc-it-k-j/out2.log 2>&1 & )
  sleep 3

  # ── Host: airc whois kjoiner pulls fields from peer file ──
  local out
  out=$(AIRC_HOME=/tmp/airc-it-k-h/state "$AIRC" whois kjoiner 2>&1)
  echo "$out" | grep -q "pronouns: *he" && pass "host can whois joiner (handshake exchange worked)" \
                                        || fail "host whois joiner missing identity (got: $out)"
  echo "$out" | grep -q "role: *joined-with-id" && pass "host whois joiner shows role" || fail "host whois joiner missing role"

  # ── Joiner attempts kick → refused ──
  out=$(AIRC_HOME=/tmp/airc-it-k-j/state "$AIRC" kick khost 2>&1 || true)
  echo "$out" | grep -qi "only the room host can kick\|joiner of" \
    && pass "joiner can't kick (rejected with helpful error)" \
    || fail "joiner kick attempt should be refused (got: $out)"

  # ── Capture joiner's SSH pubkey BEFORE kick so we can assert removal ──
  # init_identity always generates ssh_key.pub and the pair handshake always
  # appends to ~/.ssh/authorized_keys — if either is missing, that's itself
  # a regression worth failing on (the kick-revocation check below would
  # otherwise be silently skipped, defeating the whole assertion).
  local kj_ssh_pub
  kj_ssh_pub=$(cat /tmp/airc-it-k-j/state/identity/ssh_key.pub 2>/dev/null | tr -d '\n' || true)
  [ -n "$kj_ssh_pub" ] \
    && pass "joiner's ssh_key.pub generated by init_identity" \
    || { fail "joiner's ssh_key.pub missing — init_identity regression"; cleanup_all; return; }
  [ -f "$HOME/.ssh/authorized_keys" ] \
    && pass "host's authorized_keys exists post-handshake" \
    || { fail "host's authorized_keys missing post-handshake — pair regression"; cleanup_all; return; }
  grep -qF "$kj_ssh_pub" "$HOME/.ssh/authorized_keys" \
    && pass "joiner's SSH key present in authorized_keys before kick" \
    || fail "joiner's SSH key missing from authorized_keys before kick (handshake regression?)"

  # ── Host kicks joiner ──
  out=$(AIRC_HOME=/tmp/airc-it-k-h/state "$AIRC" kick kjoiner "scenario test" 2>&1)
  echo "$out" | grep -q "Kicked kjoiner" && pass "kick prints confirmation" || fail "kick missing confirmation (got: $out)"

  # ── Peer file gone ──
  [ ! -f /tmp/airc-it-k-h/state/peers/kjoiner.json ] \
    && pass "kicked peer's file removed" \
    || fail "peer file still present after kick"

  # ── SSH key actually removed from authorized_keys ──
  # Without this assertion, kick's pubkey-removal could silently regress
  # — Copilot's #73 review caught a bug where kick was reading the wrong
  # .pub file and leaving the SSH key in place.
  if [ -n "$kj_ssh_pub" ] && [ -f "$HOME/.ssh/authorized_keys" ]; then
    grep -qF "$kj_ssh_pub" "$HOME/.ssh/authorized_keys" \
      && fail "kicked peer's SSH key still in authorized_keys (kick didn't actually revoke access)" \
      || pass "kicked peer's SSH key removed from authorized_keys"
  fi

  # ── airc whois on the now-kicked peer is graceful ──
  out=$(AIRC_HOME=/tmp/airc-it-k-h/state "$AIRC" whois kjoiner 2>&1 || true)
  echo "$out" | grep -q "no record for 'kjoiner'" \
    && pass "whois post-kick prints no-record" \
    || fail "whois post-kick should report missing"

  # ── Reject path-traversal attempts in peer name ──
  out=$(AIRC_HOME=/tmp/airc-it-k-h/state "$AIRC" whois "../config" 2>&1 || true)
  echo "$out" | grep -q "invalid peer name" \
    && pass "whois rejects path-traversal in peer name" \
    || fail "whois did NOT reject '../config' as a peer name (got: $out)"
  out=$(AIRC_HOME=/tmp/airc-it-k-h/state "$AIRC" kick "../config" 2>&1 || true)
  echo "$out" | grep -q "invalid peer name" \
    && pass "kick rejects path-traversal in peer name" \
    || fail "kick did NOT reject '../config' as a peer name (got: $out)"

  cleanup_all
}

# ── Scenario: heartbeat (orphan-gist self-heal, structural fix) ───────
# When a host dies ungracefully, its room gist persists pointing at the
# corpse. With heartbeat: host updates last_heartbeat every
# AIRC_HEARTBEAT_SEC; joiners check freshness on resolve and take over
# deterministically when stale. This test:
#   1. Hosts a room (real gh, real gist)
#   2. Verifies last_heartbeat appears in the gist
#   3. Verifies last_heartbeat advances over time
#   4. kill -9's the host — heartbeat thread dies with it, gist NOT cleaned
#   5. Waits past AIRC_HEARTBEAT_STALE
#   6. Spawns a joiner with discovery enabled
#   7. Verifies joiner deleted stale gist + published fresh one
#
# Skips entirely if gh is unavailable or unauthed — this scenario can't
# run in gh-less CI. AIRC_HEARTBEAT_SEC=2 / AIRC_HEARTBEAT_STALE=5 keep
# wall-time short; cleanup deletes any gist this scenario published.
scenario_heartbeat() {
  section "heartbeat: orphan-gist self-heal via stale presence signal"

  if ! command -v gh >/dev/null 2>&1; then
    echo "  (skipped — gh CLI not installed)"
    return
  fi
  if ! gh auth status >/dev/null 2>&1; then
    echo "  (skipped — gh not authed: 'gh auth login -s gist')"
    return
  fi
  if ! command -v jq >/dev/null 2>&1; then
    echo "  (skipped — jq not installed)"
    return
  fi

  cleanup_all

  local rname="hb-test-$$"
  local hb_sec=2 hb_stale=5

  # ── Host alpha in room mode WITH gh discovery + gist push.
  mkdir -p /tmp/airc-it-h
  ( cd /tmp/airc-it-h && AIRC_HOME=/tmp/airc-it-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      AIRC_NO_DISCOVERY=1 AIRC_HEARTBEAT_SEC=$hb_sec \
      "$AIRC" connect --room "$rname" > /tmp/airc-it-h/out.log 2>&1 & )

  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    [ -f /tmp/airc-it-h/state/room_gist_id ] && break
  done

  local gist_id
  gist_id=$(cat /tmp/airc-it-h/state/room_gist_id 2>/dev/null)
  [ -n "$gist_id" ] \
    && pass "alpha published room gist ($gist_id)" \
    || { fail "alpha did not publish a room gist within 10s"; cleanup_all; return; }

  # Verify last_heartbeat field is present in the gist.
  local hb1
  hb1=$(gh api "gists/$gist_id" 2>/dev/null \
        | jq -r '.files | to_entries[0].value.content' 2>/dev/null \
        | jq -r '.last_heartbeat // empty' 2>/dev/null)
  [ -n "$hb1" ] \
    && pass "gist contains last_heartbeat field ($hb1)" \
    || { fail "gist missing last_heartbeat field"; gh gist delete "$gist_id" --yes 2>/dev/null; cleanup_all; return; }

  # Wait > 1 heartbeat interval, verify the field advanced.
  sleep $((hb_sec + 2))
  local hb2
  hb2=$(gh api "gists/$gist_id" 2>/dev/null \
        | jq -r '.files | to_entries[0].value.content' 2>/dev/null \
        | jq -r '.last_heartbeat // empty' 2>/dev/null)
  if [ -n "$hb2" ] && [ "$hb2" != "$hb1" ]; then
    pass "last_heartbeat advanced after ${hb_sec}s ($hb2)"
  else
    fail "last_heartbeat did NOT advance ($hb1 → $hb2)"
    gh gist delete "$gist_id" --yes 2>/dev/null
    cleanup_all; return
  fi

  # ── kill -9 the host. Heartbeat thread dies with it; gist persists.
  local host_pids
  host_pids=$(cat /tmp/airc-it-h/state/airc.pid 2>/dev/null)
  [ -n "$host_pids" ] || { fail "no host pid recorded"; cleanup_all; return; }
  kill -9 $host_pids 2>/dev/null || true
  sleep 1
  pass "host kill -9'd ($host_pids)"

  # Wait past the stale window. Use the earlier hb2 timestamp as our
  # "now-ish" anchor — sleep enough that whatever the next gist read
  # sees has aged past hb_stale. Buffer = 2x stale to be deterministic.
  sleep $((hb_stale + 3))

  # Verify gist still exists (host died ungracefully, so EXIT trap didn't fire).
  gh api "gists/$gist_id" >/dev/null 2>&1 \
    && pass "stale gist still present (host kill -9 = no graceful cleanup)" \
    || fail "gist already gone — kill -9 path didn't behave as expected"

  # ── Spawn joiner beta with discovery ON. Joiner should:
  #    - resolve the gist
  #    - detect last_heartbeat is stale
  #    - take over: delete stale gist, exec into host mode
  mkdir -p /tmp/airc-it-j
  ( cd /tmp/airc-it-j && AIRC_HOME=/tmp/airc-it-j/state AIRC_NAME=beta AIRC_PORT=7550 \
      AIRC_HEARTBEAT_STALE=$hb_stale AIRC_HEARTBEAT_SEC=$hb_sec \
      "$AIRC" connect --room "$rname" > /tmp/airc-it-j/out.log 2>&1 & )

  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    grep -qE 'taking over|self-healing as new host' /tmp/airc-it-j/out.log 2>/dev/null && break
  done

  grep -qE 'taking over|self-healing as new host' /tmp/airc-it-j/out.log \
    && pass "beta detected stale heartbeat + initiated takeover" \
    || { fail "beta did NOT detect stale heartbeat (log: $(tail -20 /tmp/airc-it-j/out.log))"; cleanup_all; return; }

  # Wait for beta to publish a fresh gist as new host.
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    [ -f /tmp/airc-it-j/state/room_gist_id ] && break
  done

  local new_gist_id
  new_gist_id=$(cat /tmp/airc-it-j/state/room_gist_id 2>/dev/null)
  if [ -n "$new_gist_id" ] && [ "$new_gist_id" != "$gist_id" ]; then
    pass "beta published fresh gist as new host ($new_gist_id, replaces $gist_id)"
  else
    fail "beta did not publish a fresh gist (got: '$new_gist_id', original: '$gist_id')"
  fi

  # Old gist must be gone (beta deleted it during takeover).
  if gh api "gists/$gist_id" >/dev/null 2>&1; then
    fail "stale gist $gist_id still exists after takeover"
    gh gist delete "$gist_id" --yes 2>/dev/null
  else
    pass "stale gist $gist_id removed by takeover"
  fi

  # Cleanup: delete the new gist beta published.
  if [ -n "$new_gist_id" ]; then
    gh gist delete "$new_gist_id" --yes 2>/dev/null || true
  fi
  cleanup_all
}

# ── Scenario: bounce (teardown should not orphan the host's gist) ─────
# host A → teardown → host A again. Each cycle must leave AT MOST ONE
# gist for the room name on the gh account. Pre-fix, every bounce
# accumulated an orphan because cmd_teardown's kill -9 skipped the
# EXIT trap that would have deleted the gist (PR #110).
# Skips if gh is unavailable.
scenario_bounce() {
  section "bounce: teardown deletes hosted gist (no orphan accumulation)"

  if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
    echo "  (skipped — gh not authed)"
    return
  fi

  cleanup_all
  local rname="bounce-test-$$"
  mkdir -p /tmp/airc-it-h

  # Round 1: spawn host
  ( cd /tmp/airc-it-h && AIRC_HOME=/tmp/airc-it-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --room "$rname" > /tmp/airc-it-h/out.log 2>&1 & )
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    [ -f /tmp/airc-it-h/state/room_gist_id ] && break
  done

  local gid1; gid1=$(cat /tmp/airc-it-h/state/room_gist_id 2>/dev/null)
  [ -n "$gid1" ] && pass "round 1: alpha hosted, gist=$gid1" \
                 || { fail "round 1: no gist published"; cleanup_all; return; }

  # Teardown
  AIRC_HOME=/tmp/airc-it-h/state "$AIRC" teardown >/dev/null 2>&1
  sleep 2

  # Verify gist deleted
  if gh api "gists/$gid1" >/dev/null 2>&1; then
    fail "teardown LEFT gist $gid1 on gh account (orphan)"
    gh gist delete "$gid1" --yes 2>/dev/null  # cleanup our mess
  else
    pass "teardown deleted gist $gid1 ✓"
  fi

  # Round 2: rehost same room, verify NO orphan from round 1.
  # Teardown leaves room_gist_id behind (it only wipes airc.pid +
  # host_gist_id), so we can't `[ -f room_gist_id ]` as a "round 2
  # ready" signal — that file already exists from round 1. Wait for
  # the round-2 banner instead.
  ( cd /tmp/airc-it-h && AIRC_HOME=/tmp/airc-it-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --room "$rname" > /tmp/airc-it-h/out2.log 2>&1 & )
  for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
    sleep 1
    grep -qE "Hosting #${rname}|Waiting for peers" /tmp/airc-it-h/out2.log 2>/dev/null && break
  done
  sleep 1   # let host_gist_id finish writing

  local gid2; gid2=$(cat /tmp/airc-it-h/state/host_gist_id 2>/dev/null)
  [ -z "$gid2" ] && gid2=$(cat /tmp/airc-it-h/state/room_gist_id 2>/dev/null)
  [ -n "$gid2" ] && [ "$gid2" != "$gid1" ] \
    && pass "round 2: alpha re-hosted, fresh gist=$gid2" \
    || fail "round 2: no fresh gist or same as orphan (gid1=$gid1 gid2=$gid2)"

  local count
  count=$(gh gist list --limit 50 2>/dev/null | awk -F'\t' -v r="airc room: $rname" '$2==r' | wc -l | tr -d ' ')
  [ "$count" = "1" ] \
    && pass "exactly one #${rname} gist on account after bounce ✓" \
    || fail "expected 1 gist, found $count (orphan accumulation)"

  # Cleanup
  AIRC_HOME=/tmp/airc-it-h/state "$AIRC" teardown >/dev/null 2>&1
  [ -n "$gid2" ] && gh gist delete "$gid2" --yes 2>/dev/null
  cleanup_all
}

# ── Scenario: two-tab localhost (multi-address: same machine = 127.0.0.1) ───
# Two airc processes on the same machine, same gh account, joining the
# same room. Joiner must pick the host's localhost address via
# machine_id match, not the host's LAN/Tailscale advertised address.
# Validates host.addresses[] + host.machine_id propagation through the
# gist envelope and peer_pick_address logic.
scenario_two_tab_localhost() {
  section "two_tab_localhost: same-machine join uses 127.0.0.1 (multi-address)"

  if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
    echo "  (skipped — gh not authed)"
    return
  fi

  cleanup_all
  local rname="ttl-test-$$"
  mkdir -p /tmp/airc-it-h /tmp/airc-it-j

  # Host
  ( cd /tmp/airc-it-h && AIRC_HOME=/tmp/airc-it-h/state AIRC_NAME=alpha AIRC_PORT=7549 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --room "$rname" > /tmp/airc-it-h/out.log 2>&1 & )
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    [ -f /tmp/airc-it-h/state/room_gist_id ] && break
  done

  local gid; gid=$(cat /tmp/airc-it-h/state/room_gist_id 2>/dev/null)
  [ -n "$gid" ] && pass "alpha hosted, gist=$gid" \
                || { fail "alpha did not publish gist"; cleanup_all; return; }

  # Verify the gist envelope carries machine_id + addresses[]
  local env; env=$(gh api "gists/$gid" 2>/dev/null | jq -r '.files | to_entries[0].value.content' 2>/dev/null)
  printf '%s' "$env" | jq -e '.host.machine_id' >/dev/null 2>&1 \
    && pass "envelope has host.machine_id" \
    || fail "envelope MISSING host.machine_id"
  printf '%s' "$env" | jq -e '.host.addresses | length >= 1' >/dev/null 2>&1 \
    && pass "envelope has host.addresses[]" \
    || fail "envelope MISSING host.addresses[]"
  printf '%s' "$env" | jq -e '.host.addresses[] | select(.scope=="localhost")' >/dev/null 2>&1 \
    && pass "envelope addresses include localhost entry" \
    || fail "envelope addresses MISSING localhost"

  # Joiner via discovery (will find this gist via gh list)
  ( cd /tmp/airc-it-j && AIRC_HOME=/tmp/airc-it-j/state AIRC_NAME=beta AIRC_PORT=7550 \
      "$AIRC" connect --room "$rname" > /tmp/airc-it-j/out.log 2>&1 & )

  for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
    sleep 1
    grep -qE 'Connected to|Multi-address pick|unreachable' /tmp/airc-it-j/out.log 2>/dev/null && break
  done

  grep -qE 'Multi-address pick: 127\.0\.0\.1' /tmp/airc-it-j/out.log \
    && pass "beta picked 127.0.0.1 via machine_id match ✓" \
    || fail "beta did NOT pick localhost (log: $(grep -E 'Multi-address|Connecting' /tmp/airc-it-j/out.log | head -2 | tr '\n' '|'))"

  grep -q 'Connected to' /tmp/airc-it-j/out.log \
    && pass "beta SSH-paired with alpha over localhost" \
    || fail "beta did NOT successfully pair"

  # Cleanup
  for f in /tmp/airc-it-h/state/airc.pid /tmp/airc-it-j/state/airc.pid; do
    [ -f "$f" ] && kill -9 $(cat "$f") 2>/dev/null
  done
  sleep 1
  gh gist delete "$gid" --yes 2>/dev/null
  cleanup_all
}

# ── Scenario: auto_scope (default room derived from git remote org) ─────
# The /join skill contract: bare `airc join` from a useideem/* checkout
# lands in #useideem; from a cambriantech/* checkout lands in #cambriantech.
# A previous PR (#104) gated this behind AIRC_AUTO_SCOPE_ROOM=1, which
# left bare-launched agents stuck in #general regardless of cwd —
# defeating the whole point. Re-enabled as default 2026-04-26 after a
# session of dogfooding pain (two useideem tabs both hit #general
# instead of converging on #useideem).
#
# Test plan: stand up a fake git repo with origin pointing to
# `useideem/foo`, run `airc connect` in that cwd (gh-free, --no-gist),
# verify the "Auto-scoped: #useideem (from git org; ...)" banner fires
# and that room_name is "useideem". Then verify AIRC_NO_AUTO_ROOM=1
# opts out cleanly (banner absent, falls back to #general).
scenario_auto_scope() {
  section "auto_scope: bare connect derives room from git remote org"
  cleanup_all

  local repo=/tmp/airc-it-auto-repo
  rm -rf "$repo"; mkdir -p "$repo"
  ( cd "$repo" && git init -q 2>/dev/null && git remote add origin https://github.com/useideem/foo.git ) \
    || { fail "git scaffold failed"; cleanup_all; return; }

  # Default ON: bare connect should auto-scope.
  ( cd "$repo" && AIRC_HOME=/tmp/airc-it-auto-h/state AIRC_NAME=alpha AIRC_PORT=7561 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-gist > /tmp/airc-it-auto-h.log 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -qE 'Hosting as|Auto-scoped' /tmp/airc-it-auto-h.log 2>/dev/null && break
  done

  grep -qE 'Auto-scoped: #useideem \(from git org' /tmp/airc-it-auto-h.log \
    && pass "auto-scope banner: 'Auto-scoped: #useideem (from git org)'" \
    || fail "auto-scope banner MISSING (got: $(head -3 /tmp/airc-it-auto-h.log | tr '\n' '|'))"

  grep -qE 'Hosting #useideem' /tmp/airc-it-auto-h.log \
    && pass "host banner reports #useideem (auto-scoped room took effect)" \
    || fail "host banner not on #useideem (auto-scope didn't propagate to host setup)"

  # Kill that run before testing the opt-out (port + scope reuse).
  for f in /tmp/airc-it-auto-h/state/airc.pid; do
    [ -f "$f" ] && kill -9 $(cat "$f") 2>/dev/null
  done
  sleep 1
  rm -rf /tmp/airc-it-auto-h /tmp/airc-it-auto-h.log

  # Opt-out: AIRC_NO_AUTO_ROOM=1 should bypass auto-scope entirely.
  ( cd "$repo" && AIRC_HOME=/tmp/airc-it-auto-h2/state AIRC_NAME=alpha AIRC_PORT=7562 \
      AIRC_NO_DISCOVERY=1 AIRC_NO_AUTO_ROOM=1 \
      "$AIRC" connect --no-gist > /tmp/airc-it-auto-h2.log 2>&1 & )
  for i in 1 2 3 4 5; do
    sleep 1
    grep -qE 'Hosting as' /tmp/airc-it-auto-h2.log 2>/dev/null && break
  done

  ! grep -qE 'Auto-scoped' /tmp/airc-it-auto-h2.log \
    && pass "AIRC_NO_AUTO_ROOM=1 suppresses auto-scope banner" \
    || fail "AIRC_NO_AUTO_ROOM=1 still printed Auto-scoped (opt-out broken)"

  grep -qE 'Hosting #general' /tmp/airc-it-auto-h2.log \
    && pass "AIRC_NO_AUTO_ROOM=1 falls back to #general" \
    || fail "AIRC_NO_AUTO_ROOM=1 didn't land on #general (got: $(grep Hosting /tmp/airc-it-auto-h2.log | head -1))"

  for f in /tmp/airc-it-auto-h2/state/airc.pid; do
    [ -f "$f" ] && kill -9 $(cat "$f") 2>/dev/null
  done
  sleep 1
  rm -rf /tmp/airc-it-auto-h2 /tmp/airc-it-auto-h2.log "$repo"
  cleanup_all
}

# ── Scenario: room_overrides_resume (--room discards stale saved pairing) ──
# Pre-fix: `airc connect --room foo` after a prior pairing into #bar
# silently ignored the flag and resumed #bar's host, because the resume
# path didn't compare the saved room_name to the explicit --room. The
# user had to manually `airc teardown --flush` before the flag took
# effect — exactly the toil the substrate is supposed to eliminate.
#
# Post-fix: when --room is explicit AND saved room_name differs, the
# resume path discards the stale CONFIG + room_name + room_gist_id and
# falls through to discovery for the requested room. Identity persists
# (no flush needed); ssh_key + peer records survive.
scenario_room_overrides_resume() {
  section "room_overrides_resume: explicit --room discards stale saved pairing"
  cleanup_all

  # Synthesize a saved joiner state for room #old-room with a dead host.
  # We don't need a real host — the resume path checks --room/saved-room
  # mismatch BEFORE attempting any SSH probe, and bails early if they
  # differ. (The probe itself is exercised by scenario_resume_stale_auth.)
  local home=/tmp/airc-it-ror/state
  mkdir -p "$home/identity"
  ssh-keygen -t ed25519 -f "$home/identity/ssh_key" -N '' -q -C 'airc-test-ror' 2>/dev/null
  cat > "$home/config.json" <<'JSON'
{
  "name": "alpha",
  "host_target": "deadhost@127.0.0.1:9999",
  "host_name": "deadhost",
  "host_port": 9999,
  "host_ssh_pub": "ssh-ed25519 AAAAignored"
}
JSON
  echo "old-room" > "$home/room_name"

  # Run connect with --room new-room. Should discard stale pair, then
  # proceed to host #new-room (AIRC_NO_DISCOVERY=1 + --no-gist keep
  # this gh-free).
  AIRC_HOME="$home" AIRC_NAME=alpha AIRC_PORT=7563 AIRC_NO_DISCOVERY=1 \
    "$AIRC" connect --room new-room --no-gist > /tmp/airc-it-ror.log 2>&1 &
  local pid=$!
  local i
  for i in 1 2 3 4 5 6; do
    sleep 1
    grep -qE 'Hosting #new-room|discarding stale pairing' /tmp/airc-it-ror.log 2>/dev/null && break
  done

  grep -qE 'Saved pairing was for #old-room.*--room #new-room.*discarding stale pairing' /tmp/airc-it-ror.log \
    && pass "discard banner fires with old room + new room named" \
    || fail "no discard banner (got: $(head -5 /tmp/airc-it-ror.log | tr '\n' '|'))"

  grep -qE 'Hosting #new-room' /tmp/airc-it-ror.log \
    && pass "fell through to host #new-room after discarding stale pair" \
    || fail "did NOT host #new-room (got: $(grep -E 'Hosting|Resuming' /tmp/airc-it-ror.log | head -3 | tr '\n' '|'))"

  ! grep -qE 'Resuming as joiner of .deadhost' /tmp/airc-it-ror.log \
    && pass "did NOT resume the stale deadhost pairing" \
    || fail "still tried to resume deadhost despite explicit --room"

  # Identity must survive — ssh_key intact post-discard.
  [ -f "$home/identity/ssh_key" ] \
    && pass "identity (ssh_key) preserved across discard" \
    || fail "ssh_key was wiped (over-broad cleanup)"

  for f in "$home/airc.pid"; do
    [ -f "$f" ] && kill -9 $(cat "$f") 2>/dev/null
  done
  sleep 1
  rm -rf /tmp/airc-it-ror /tmp/airc-it-ror.log
  cleanup_all
}

# ── Scenario: stale_auth_room_selfheal (room-mode auto-recover) ────────
# Pre-fix companion to scenario_resume_stale_auth: when the saved
# pairing has a saved room_name (i.e. we were in a #room, not a 1:1
# invite), stale SSH auth shouldn't `die` and demand the user run
# `airc teardown --flush`. It should fall through to fresh discovery
# for that room — re-pair against whoever's now hosting, or become
# the new host. Identity persists; the user does nothing.
#
# Without this self-heal, the bare `airc join` UX hits a forced manual
# repair every time a host machine reinstalls / rotates keys / wipes
# state — exactly the cliff Joel hit twice on 2026-04-26 (vhsm-2c84
# dead host followed by the no-saved-pair-after-flush bug, which sent
# us into #general instead of #useideem).
#
# This test covers ONLY the saved-room branch. The legacy 1:1 invite
# branch (no saved room) keeps its die-loud behavior and is still
# covered by scenario_resume_stale_auth.
scenario_stale_auth_room_selfheal() {
  section "stale_auth_room_selfheal: room-mode resume self-heals on stale auth"
  cleanup_all

  local rname="sars-test-$$"
  mkdir -p /tmp/airc-it-sars-h /tmp/airc-it-sars-j

  # Host alpha in room mode (gh-free).
  ( cd /tmp/airc-it-sars-h && AIRC_HOME=/tmp/airc-it-sars-h/state AIRC_NAME=alpha AIRC_PORT=7564 \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-gist --room "$rname" > /tmp/airc-it-sars-h/out.log 2>&1 & )
  local i
  for i in 1 2 3 4 5; do
    sleep 1
    grep -q 'Hosting as' /tmp/airc-it-sars-h/out.log 2>/dev/null && break
  done
  grep -q 'Hosting as' /tmp/airc-it-sars-h/out.log \
    && pass "alpha hosting #${rname}" \
    || { fail "alpha did not start"; cleanup_all; return; }

  local join; join=$(read_join_string /tmp/airc-it-sars-h)
  [ -n "$join" ] || { fail "no join string from alpha"; cleanup_all; return; }

  # Joiner beta pairs into the room (also writes room_name on disk).
  ( cd /tmp/airc-it-sars-j && AIRC_HOME=/tmp/airc-it-sars-j/state AIRC_NAME=beta \
      AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect "$join" > /tmp/airc-it-sars-j/out.log 2>&1 & )
  for i in 1 2 3 4 5 6; do
    sleep 1
    grep -q 'Connected to' /tmp/airc-it-sars-j/out.log 2>/dev/null && break
  done
  grep -q 'Connected to' /tmp/airc-it-sars-j/out.log \
    && pass "beta paired with alpha" \
    || { fail "beta join failed"; cleanup_all; return; }

  # Beta's resume path needs a saved room_name to pick the self-heal
  # branch over the die branch. The non-discovery inline-invite join
  # path doesn't write room_name — synthesize it the way a discovery
  # join would. (Production discovery join always writes this.)
  echo "$rname" > /tmp/airc-it-sars-j/state/room_name

  # Stale-auth simulation: kill beta, regenerate beta's SSH key. Alpha's
  # authorized_keys still has the OLD key, so any resume probe will get
  # "Permission denied (publickey)" — which is the trigger for the
  # self-heal we're testing.
  AIRC_HOME=/tmp/airc-it-sars-j/state "$AIRC" teardown >/dev/null 2>&1
  sleep 1
  rm -f /tmp/airc-it-sars-j/state/identity/ssh_key \
        /tmp/airc-it-sars-j/state/identity/ssh_key.pub
  ssh-keygen -t ed25519 -f /tmp/airc-it-sars-j/state/identity/ssh_key \
             -N '' -q -C 'airc-stale-sars' 2>/dev/null

  # Resume. Pre-fix would die (exit 1). Post-fix: re-execs with
  # --room ${rname}. AIRC_NO_DISCOVERY is NOT inherited across the
  # re-exec, but with no real gh probe configured here the discovery
  # path will silently no-op and fall through to host mode — beta
  # becomes the new host of #${rname}. We just need to verify it
  # DIDN'T die and DID land in the room.
  local resume_out=/tmp/airc-it-sars-j-resume.out
  local resume_err=/tmp/airc-it-sars-j-resume.err
  ( AIRC_HOME=/tmp/airc-it-sars-j/state AIRC_PORT=7565 AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-gist >"$resume_out" 2>"$resume_err" ) &
  local resume_pid=$!
  local exited=0
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    grep -qE "Hosting #${rname}|Self-healing|Resume aborted" "$resume_out" "$resume_err" 2>/dev/null && break
    kill -0 $resume_pid 2>/dev/null || { exited=1; break; }
  done

  grep -qE 'Self-healing: discarding stale pairing' "$resume_out" \
    && pass "self-heal banner fires on stale-auth resume in room mode" \
    || fail "self-heal banner missing (got: $(head -10 "$resume_out" "$resume_err" | tr '\n' '|'))"

  ! grep -qE 'Resume aborted — re-pair required' "$resume_err" \
    && pass "did NOT die with 'Resume aborted' (room-mode self-heal took over)" \
    || fail "still died with Resume aborted despite saved room_name"

  grep -qE "Hosting #${rname}" "$resume_out" \
    && pass "beta self-healed into hosting #${rname}" \
    || fail "beta did NOT land in #${rname} after self-heal (got: $(grep -E 'Hosting|Found' "$resume_out" | head -3 | tr '\n' '|'))"

  # Identity must survive the re-exec (peer records preserved means
  # any future re-pair recognizes us as the same beta, not a stranger).
  [ -f /tmp/airc-it-sars-j/state/identity/ssh_key ] \
    && pass "identity (ssh_key) survived self-heal re-exec" \
    || fail "ssh_key wiped during self-heal"

  # Cleanup the resume process + harness.
  kill -9 $resume_pid 2>/dev/null
  for f in /tmp/airc-it-sars-h/state/airc.pid /tmp/airc-it-sars-j/state/airc.pid; do
    [ -f "$f" ] && kill -9 $(cat "$f") 2>/dev/null
  done
  sleep 1
  rm -f "$resume_out" "$resume_err"
  cleanup_all
}

# ── Scenario: send_dead_monitor_dies (no silent void-broadcasts) ─────────
# Pre-fix: `airc msg "hello"` from a host scope whose monitor is dead
# returned exit 0 with the message appended to messages.jsonl that
# nobody was tailing. The user's send "succeeded" but reached zero
# peers. This is exactly how Joel hit "i see no communication going
# on" on 2026-04-26 — shell auto-cd'd into a different scope mid-
# session, that scope was a host with a stale pidfile, every send
# returned 0 with zero delivery, and the actual paired tab waited
# forever for a reply that vanished into a void.
#
# Post-fix: cmd_send detects host-with-dead-monitor and dies with a
# clear diagnostic naming the scope, the stale pidfile path, and the
# remediation. Joiner sends are unchanged (they go via SSH; monitor
# liveness on the joiner side is irrelevant to delivery).
scenario_send_dead_monitor_dies() {
  section "send_dead_monitor_dies: host scope with dead monitor refuses to silent-succeed"
  cleanup_all

  # Synthesize a host scope (no host_target in config, identity present,
  # stale pidfile pointing at a dead PID). No actual host process —
  # we're testing cmd_send's pre-flight liveness check, not the wire.
  local home=/tmp/airc-it-sdmd/state
  mkdir -p "$home/identity" "$home/peers"
  ssh-keygen -t ed25519 -f "$home/identity/ssh_key" -N '' -q -C 'airc-test-sdmd' 2>/dev/null
  cat > "$home/config.json" <<'JSON'
{ "name": "ghost-host" }
JSON
  # Stale pidfile pointing at a definitely-dead PID. Pick 99999 — outside
  # most systems' active range, plus we kill -0 to verify before asserting.
  if kill -0 99999 2>/dev/null; then
    fail "PID 99999 unexpectedly alive on this system — pick a different stale PID"
    cleanup_all; return
  fi
  echo "99999" > "$home/airc.pid"

  local out err
  out=$(mktemp -t airc-sdmd-out.XXXXXX)
  err=$(mktemp -t airc-sdmd-err.XXXXXX)
  AIRC_HOME="$home" "$AIRC" msg "send into the void" >"$out" 2>"$err"
  local rc=$?

  [ "$rc" -ne 0 ] \
    && pass "exits non-zero ($rc) when monitor is dead" \
    || fail "exited 0 despite dead monitor (silent void-broadcast bug)"

  grep -qE 'Send NOT delivered|monitor down|broadcast into a void' "$err" \
    && pass "stderr names the failure (not silent)" \
    || fail "stderr missing the diagnostic (got: $(cat "$err"))"

  grep -qE 'pidfile.*stale|pidfile.*absent' "$err" \
    && pass "stderr identifies pidfile state (stale or absent)" \
    || fail "stderr doesn't mention pidfile state"

  grep -qE "scope:.*$home" "$err" \
    && pass "stderr names the offending scope dir" \
    || fail "stderr doesn't surface scope path (user can't tell where their cwd resolved)"

  # Also test the absent-pidfile path (monitor never started in this scope).
  rm -f "$home/airc.pid"
  AIRC_HOME="$home" "$AIRC" msg "still void" >"$out" 2>"$err"
  rc=$?
  [ "$rc" -ne 0 ] \
    && pass "exits non-zero when pidfile is absent (monitor never started)" \
    || fail "exited 0 with absent pidfile"
  grep -qE 'pidfile:.*absent' "$err" \
    && pass "stderr correctly distinguishes absent vs stale pidfile" \
    || fail "stderr doesn't say 'absent' for missing pidfile"

  # Negative control: with a live PID in the pidfile, send should NOT die
  # on this check. Use $$ — the test harness's own PID, definitely alive.
  echo $$ > "$home/airc.pid"
  AIRC_HOME="$home" "$AIRC" msg "live monitor probe ascii" >"$out" 2>"$err"
  rc=$?
  [ "$rc" = "0" ] \
    && pass "live-pid scope: send returns 0 (no false positive on liveness check)" \
    || fail "live-pid scope incorrectly rejected (rc=$rc, stderr=$(cat "$err"))"
  grep -q 'live monitor probe ascii' "$home/messages.jsonl" \
    && pass "live-pid scope: message appended to local log as expected" \
    || fail "live-pid scope: message NOT in log despite rc=0 (log=$(cat "$home/messages.jsonl" 2>/dev/null))"

  rm -f "$out" "$err"
  rm -rf /tmp/airc-it-sdmd
  cleanup_all
}

# ── Scenario: resume_404_gist_no_silent_exit (issue #118) ───────────────
# Pre-fix: when the saved room_gist_id refers to a gist that's been
# deleted (host teardown'd), the gist-probe in the resume path runs
# `gh api gists/<id>` under `set -euo pipefail` with no `|| ...`
# guard. The 404 (which is the EXPECTED signal that the gist is gone)
# trips set -e, the script exits 1 silently — BEFORE the 404
# classification + self-heal logic below it can run. Vhsm-Claude hit
# this on 2026-04-26: tab A teardown'd #useideem (deleted the gist),
# tab B's resume tried to look up the now-deleted gist and silent-
# died. The user had to `airc teardown --flush` manually, defeating
# the whole point of saved-state self-heal.
#
# Post-fix: pre-declare _gist_probe_rc=0 + use `|| _gist_probe_rc=$?`
# so set -e doesn't fire on the expected 404. The classification
# block proceeds and self-heals into fresh discovery.
#
# Test: synthesize a joiner CONFIG with a known-bogus gist_id +
# saved room_name. Run `airc connect`. Expect EITHER a self-heal
# banner OR a structured stderr — NOT silent exit 1.
scenario_resume_404_gist_no_silent_exit() {
  section "resume_404_gist_no_silent_exit: deleted-gist resume self-heals (issue #118)"

  if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
    echo "  (skipped — gh not authed; gist probe is the trigger we need)"
    return
  fi

  # Confirm gh has gist scope — the gh-health gate requires it before the
  # probe runs. Without it, the bug doesn't trigger and the test would
  # pass for the wrong reason.
  if ! gh auth status 2>&1 | grep -qiE '(scopes|token scopes):.*\bgist\b'; then
    echo "  (skipped — gh missing 'gist' scope; gh-health gate would short-circuit before the bug fires)"
    return
  fi

  cleanup_all
  local home=/tmp/airc-it-r404/state
  mkdir -p "$home/identity" "$home/peers"
  ssh-keygen -t ed25519 -f "$home/identity/ssh_key" -N '' -q -C 'airc-test-r404' 2>/dev/null

  # Synthesize a joiner with: host_target (so resume path fires),
  # saved room_name (so self-heal can re-exec --room), and a bogus
  # room_gist_id (so the 404 path is exercised). The host_target
  # points at a dead port so the SSH probe down the line fails fast
  # — but we want the BUG (silent exit before any of that runs) to
  # be the question.
  cat > "$home/config.json" <<'JSON'
{
  "name": "ghost-joiner",
  "host_target": "deadhost@127.0.0.1",
  "host_name": "deadhost",
  "host_port": 9999,
  "host_ssh_pub": "ssh-ed25519 AAAAignored"
}
JSON
  echo "useideem-test-$$" > "$home/room_name"
  # 32-char hex id that's vanishingly unlikely to exist on any gh
  # account. gh api will return 404 for this.
  echo "deadbeef00000000000000000000000d" > "$home/room_gist_id"

  local out err
  out=$(mktemp -t airc-r404-out.XXXXXX)
  err=$(mktemp -t airc-r404-err.XXXXXX)

  # Run resume with a hard timeout — pre-fix the silent-exit happens
  # immediately, post-fix the self-heal re-execs into discovery (which
  # may try to host on a port and block; that's fine, we kill below).
  ( AIRC_HOME="$home" AIRC_PORT=7567 AIRC_NO_DISCOVERY=1 \
      "$AIRC" connect --no-gist >"$out" 2>"$err" ) &
  local pid=$!
  local i exited=0
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    if grep -qE 'no longer on your gh|Re-discovering|Re-pairing|Hosting|Resume aborted|Self-healing' "$out" "$err" 2>/dev/null; then
      break
    fi
    kill -0 $pid 2>/dev/null || { exited=1; break; }
  done

  # Assertion 1: must NOT silent-exit. Either still running (self-heal
  # re-execed and is doing something) OR exited with structured stderr.
  if [ "$exited" = "1" ]; then
    # It exited. Did it leave a diagnostic?
    if [ ! -s "$err" ] && ! grep -qE '⚠|Saved room gist|Re-discovering|Re-pairing|Self-healing|Resume aborted' "$out" 2>/dev/null; then
      fail "silent exit-1 reproduced (issue #118 NOT fixed): out=$(head -3 "$out") err=$(cat "$err")"
    else
      pass "exit was NOT silent — stderr/stdout has a diagnostic"
    fi
  else
    pass "process didn't silent-exit on 404 gist (still running or self-healing)"
  fi

  # Assertion 2: the 404 self-heal banner should be visible OR another
  # honest failure (e.g. "Re-discovering" if room_name is set, or
  # "Saved room gist no longer on your gh"). Pre-fix produces neither.
  grep -qE 'no longer on your gh|Re-discovering|Re-pairing' "$out" "$err" 2>/dev/null \
    && pass "404 self-heal banner fired (gist-deleted path classified correctly)" \
    || fail "no self-heal banner — 404 classification didn't run (got out=$(head -3 "$out") err=$(head -3 "$err"))"

  # Cleanup
  kill -9 $pid 2>/dev/null
  for f in "$home/airc.pid"; do
    [ -f "$f" ] && kill -9 $(cat "$f") 2>/dev/null
  done
  sleep 1
  rm -f "$out" "$err"
  rm -rf /tmp/airc-it-r404
  cleanup_all
}

case "$MODE" in
  tabs)         scenario_tabs  ;;
  scope)        scenario_scope ;;
  teardown)     scenario_teardown ;;
  reminder)     scenario_reminder ;;
  resilience)   scenario_resilience ;;
  reconnect)    scenario_reconnect ;;
  queue)        scenario_queue ;;
  status)       scenario_status ;;
  auth_failure) scenario_auth_failure ;;
  resume_stale_auth) scenario_resume_stale_auth ;;
  room)         scenario_room ;;
  events)       scenario_events ;;
  get_host)     scenario_get_host ;;
  identity)     scenario_identity ;;
  whois)        scenario_whois ;;
  kick)         scenario_kick ;;
  heartbeat)    scenario_heartbeat ;;
  bounce)       scenario_bounce ;;
  two_tab_localhost) scenario_two_tab_localhost ;;
  auto_scope)   scenario_auto_scope ;;
  room_overrides_resume) scenario_room_overrides_resume ;;
  stale_auth_room_selfheal) scenario_stale_auth_room_selfheal ;;
  send_dead_monitor_dies) scenario_send_dead_monitor_dies ;;
  resume_404_gist_no_silent_exit) scenario_resume_404_gist_no_silent_exit ;;
  all)          scenario_tabs; scenario_scope; scenario_reminder; scenario_teardown; scenario_resilience; scenario_reconnect; scenario_queue; scenario_status; scenario_auth_failure; scenario_resume_stale_auth; scenario_room; scenario_events; scenario_get_host; scenario_identity; scenario_whois; scenario_kick; scenario_heartbeat; scenario_bounce; scenario_two_tab_localhost; scenario_auto_scope; scenario_room_overrides_resume; scenario_stale_auth_room_selfheal; scenario_send_dead_monitor_dies; scenario_resume_404_gist_no_silent_exit ;;
  *) echo "Usage: $0 [tabs|scope|teardown|reminder|resilience|reconnect|queue|status|auth_failure|resume_stale_auth|room|events|get_host|identity|whois|kick|heartbeat|bounce|two_tab_localhost|auto_scope|room_overrides_resume|stale_auth_room_selfheal|send_dead_monitor_dies|resume_404_gist_no_silent_exit|all]"; exit 2 ;;
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
