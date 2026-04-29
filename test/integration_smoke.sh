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
  for i in $(seq 1 30); do
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
scenario_clean_install_smoke() {
  # Joel 2026-04-29: 'I have a fresh macbook ... we can make sure
  # macbook e2e from nothing is covered.' This scenario runs install.sh
  # in a sandbox (BIN_DIR + SKILLS_TARGET overrides + AIRC_DIR isolated)
  # so it doesn't clobber the real install, then verifies the resulting
  # airc binary is callable + recognizes the canonical commands.
  #
  # NOT a full e2e (we don't run airc join — that would need a fresh
  # gh account). Catches: install.sh doesn't crash, the binary lands
  # on PATH, skills land in SKILLS_TARGET, venv has cryptography.
  section "clean_install_smoke: install.sh sandbox install lands a working airc"
  command -v gh >/dev/null 2>&1 || { skip "gh not installed (install.sh would auto-install via brew, but harness can't sudo)"; return; }
  command -v python3 >/dev/null 2>&1 || { skip "python3 not installed"; return; }
  if ! command -v brew >/dev/null 2>&1; then
    skip "brew not installed — install.sh would prompt for it"
    return
  fi

  local SANDBOX
  SANDBOX=$(mktemp -d -t airc-clean-install.XXXXXX)
  trap "rm -rf '$SANDBOX'" RETURN

  # Run install.sh into sandbox via the env-var overrides.
  local install_log="$SANDBOX/install.log"
  if ! AIRC_DIR="$SANDBOX/airc-src" \
       BIN_DIR="$SANDBOX/bin" \
       SKILLS_TARGET="$SANDBOX/skills" \
       bash "$REPO_ROOT/install.sh" > "$install_log" 2>&1; then
    fail "install.sh exited non-zero — see $install_log"
    tail -10 "$install_log" | sed 's/^/    /'
    return
  fi
  pass "install.sh completed cleanly"

  # Binary on PATH within sandbox?
  if [ -x "$SANDBOX/bin/airc" ] || [ -L "$SANDBOX/bin/airc" ]; then
    pass "airc binary placed at \$BIN_DIR/airc"
  else
    fail "airc binary missing from \$BIN_DIR after install"
    ls -la "$SANDBOX/bin/" 2>&1 | sed 's/^/    /'
    return
  fi

  # Binary runs?
  local version_out
  version_out=$("$SANDBOX/bin/airc" version 2>&1 || true)
  if printf '%s' "$version_out" | grep -qE 'airc [a-f0-9]{7}'; then
    pass "airc version returns a sha"
  else
    fail "airc version output unexpected: $version_out"
  fi

  # help works (smoke for argument parsing)?
  if "$SANDBOX/bin/airc" --help >/dev/null 2>&1 \
       || "$SANDBOX/bin/airc" connect --help >/dev/null 2>&1; then
    pass "airc help paths work"
  else
    fail "airc help paths broken"
  fi

  # Skills landed?
  if [ -d "$SANDBOX/skills/join" ]; then
    pass "skills wired to \$SKILLS_TARGET (join skill present)"
  else
    fail "skills not wired — \$SKILLS_TARGET/join missing"
  fi

  # Venv has cryptography (needed for envelope encryption)?
  local venv_python="$SANDBOX/airc-src/.venv/bin/python3"
  if [ -x "$venv_python" ]; then
    if "$venv_python" -c "import cryptography" 2>/dev/null; then
      pass "venv has cryptography (envelope encryption available)"
    else
      fail "venv exists but cryptography not importable"
    fi
  else
    skip "venv not at expected location ($venv_python) — install.sh path may differ"
  fi
}

scenario_orphan_loops_self_reap() {
  # Regression for #325. Bash subshells (reminder_timer_loop /
  # flush_pending_loop) capture $PPID at start; on parent death they
  # exit at next iteration. Pre-fix they survived parent and emitted
  # "Reminder, silent" forever, the visible 'frozen monitor' symptom.
  section "orphan_loops_self_reap: parent dies → reminder/flush exit within ~10s"
  cleanup_homes_pre

  local A_HOME
  A_HOME=$(mktemp -d -t airc-orphan-loop.XXXXXX)
  trap "cleanup_homes '$A_HOME'" RETURN
  # Inline spawn — no gh, just need a process tree alive. Don't use
  # spawn_real (which waits for channel_gists population, which never
  # happens with --no-room --no-gist).
  mkdir -p "$A_HOME/state"
  ( cd "$A_HOME" \
      && AIRC_HOME="$A_HOME/state" AIRC_NAME="orphan-A-$$" AIRC_PORT=7611 \
         AIRC_NO_DISCOVERY=1 AIRC_NO_AUTO_ROOM=1 AIRC_NO_GENERAL=1 AIRC_NO_IDENTITY_PROMPT=1 \
         "$AIRC" connect --no-room --no-gist > "$A_HOME/out.log" 2>&1 & )
  local i
  for i in $(seq 1 10); do
    sleep 1
    grep -q "Hosting as" "$A_HOME/out.log" 2>/dev/null && break
  done
  if ! grep -q "Hosting as" "$A_HOME/out.log" 2>/dev/null; then
    fail "host bash never reached 'Hosting as'"; return
  fi
  sleep 3

  # Find the parent bash via airc.pid. The pidfile has multiple
  # entries post-#328 (parent on line 1, appended subshells on
  # subsequent lines). Parent is always the FIRST field of the
  # FIRST line.
  local parent_pid loop_pids
  parent_pid=$(awk 'NR==1 {print $1; exit}' "$A_HOME/state/airc.pid" 2>/dev/null)
  [ -n "$parent_pid" ] || { fail "no airc.pid for the test scope"; return; }
  # Filter to BASH subshell children only — those are the loops with
  # the parent-liveness check (#325). Python descendants (handshake,
  # monitor_formatter, etc) get reaped by other mechanisms and aren't
  # what this test covers.
  loop_pids=$(ps -ef | awk -v p="$parent_pid" '$3 == p && $0 ~ /\/bin\/bash/ {print $2}' | tr '\n' ' ')
  pass "spawned: parent=$parent_pid loop-pids=[$loop_pids]"

  # SIGKILL the parent (no traps run; loops are now reparented to init).
  kill -9 "$parent_pid" 2>/dev/null
  # Wait long enough for the 5s loop-tick to detect parent death.
  sleep 9

  # The reminder + flush loops (which #325 fixed) should be dead. The
  # foreground monitor()'s tail/formatter pipeline children (still in
  # progress until pipe breaks) may or may not be dead at this point
  # depending on timing. Assert at least 2 of the loop_pids are dead
  # — that's the regression guard for #325. Pre-fix all of them
  # survived; post-fix at least the 2 fixed loops self-reap.
  local total=0 dead=0
  for p in $loop_pids; do
    total=$((total+1))
    kill -0 "$p" 2>/dev/null || dead=$((dead+1))
  done
  if [ "$dead" -ge 2 ]; then
    pass "$dead of $total bash subshell loops self-reaped within 9s (#325 working)"
  else
    fail "only $dead of $total subshells exited — #325 parent-liveness check broken"
    pkill -9 -f "$A_HOME/state" 2>/dev/null
  fi
}

scenario_teardown_kills_env_tagged_orphans() {
  # Regression for #326. Even when airc.pid gets out of sync (parent
  # dies before children write their pids, or subshells reparent to
  # init), teardown must catch every process whose env has
  # AIRC_HOME=<scope> via the ps eww walk.
  section "teardown_kills_env_tagged_orphans: every AIRC_HOME-tagged proc dies"
  cleanup_homes_pre

  local A_HOME
  A_HOME=$(mktemp -d -t airc-td-orphan.XXXXXX)
  trap "cleanup_homes '$A_HOME'" RETURN
  mkdir -p "$A_HOME/state"
  ( cd "$A_HOME" \
      && AIRC_HOME="$A_HOME/state" AIRC_NAME="td-orphan-$$" AIRC_PORT=7612 \
         AIRC_NO_DISCOVERY=1 AIRC_NO_AUTO_ROOM=1 AIRC_NO_GENERAL=1 AIRC_NO_IDENTITY_PROMPT=1 \
         "$AIRC" connect --no-room --no-gist > "$A_HOME/out.log" 2>&1 & )
  local i
  for i in $(seq 1 10); do
    sleep 1
    grep -q "Hosting as" "$A_HOME/out.log" 2>/dev/null && break
  done
  grep -q "Hosting as" "$A_HOME/out.log" 2>/dev/null \
    || { fail "host bash never reached 'Hosting as'"; return; }
  sleep 3

  # Count scope-path-tagged processes pre-teardown via pgrep -f
  # (matches python children whose argv contains the scope path —
  # the same matcher cmd_teardown.sh now uses for its sweep).
  local pre_count
  pre_count=$(pgrep -f "$A_HOME/state" 2>/dev/null | wc -l | tr -d ' ')
  [ "$pre_count" -gt 0 ] || { fail "no scope-path-tagged procs found pre-teardown — broken setup"; return; }
  pass "pre-teardown: $pre_count scope-path-tagged procs alive"

  # Corrupt airc.pid to simulate the broken-tracking state — teardown
  # must rely on its sweep, not the pidfile. Background + 15s timeout
  # so a hung teardown doesn't wedge the test.
  echo "999999" > "$A_HOME/state/airc.pid"
  ( AIRC_HOME="$A_HOME/state" "$AIRC" teardown >/dev/null 2>&1 ) &
  local td_pid=$!
  local i
  for i in $(seq 1 15); do
    sleep 1
    kill -0 "$td_pid" 2>/dev/null || break
  done
  if kill -0 "$td_pid" 2>/dev/null; then
    fail "airc teardown hung beyond 15s — itself a regression"
    kill -9 "$td_pid" 2>/dev/null
    return
  fi
  sleep 1

  local post_count
  post_count=$(pgrep -f "$A_HOME/state" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$post_count" = "0" ]; then
    pass "post-teardown: zero scope-path-tagged procs (sweep worked)"
  else
    fail "post-teardown: $post_count procs still alive — #326 sweep didn't catch them"
    pgrep -f "$A_HOME/state" 2>/dev/null | xargs -I{} ps -p {} -o pid,command 2>/dev/null | head -5
  fi
}

scenario_my_scope_in_mesh() {
  # Joel 2026-04-29: 'remember you need to be part of it'. The other
  # scenarios spawn ephemeral test peers in /tmp and never include
  # the user's actual long-running airc scope. This one DOES — it
  # asserts that the live authenticator-448f scope (whatever scope
  # is running in the user's primary cwd) receives messages a fresh
  # test peer sends. If my own scope's monitor is broken, this catches
  # it where the isolated tests can't.
  section "my_scope_in_mesh: live local scope receives messages from a fresh peer"
  require_gh || return

  local MY_HOME="${HOME}/Development/ideem/authenticator/.airc"
  if [ ! -f "$MY_HOME/config.json" ]; then
    skip "primary scope ($MY_HOME) not initialized — run 'airc join' there first"
    return
  fi

  # Confirm my scope's monitor is running. If not, the test pre-condition
  # fails — the user must have a healthy scope before this test runs.
  local my_pids
  my_pids=$(ps eww -o pid,command 2>/dev/null \
            | awk -v home="AIRC_HOME=$MY_HOME" '$0 ~ home {print $1}' | head -3)
  if [ -z "$my_pids" ]; then
    skip "no running airc procs for primary scope — start it with 'airc join'"
    return
  fi
  pass "primary scope alive (pids: $(echo $my_pids | tr '\n' ' '))"

  local marker; marker="my-scope-test-$(date +%s%N)"
  local TEST_HOME
  TEST_HOME=$(mktemp -d -t airc-myscope.XXXXXX)
  trap "cleanup_homes '$TEST_HOME'" RETURN
  spawn_real "$TEST_HOME" "myscope-tester-$$" 7613 \
    || { fail "test peer failed to join"; return; }
  sleep 4

  AIRC_HOME="$TEST_HOME/state" "$AIRC" msg --room general "$marker" >/dev/null 2>&1
  pass "test peer sent marker"

  # Watch the user's primary log for ~45s (gh poll cycle + buffer).
  local i seen=0
  for i in $(seq 1 22); do
    sleep 2
    grep -qF "$marker" "$MY_HOME/messages.jsonl" 2>/dev/null && { seen=1; break; }
  done

  if [ "$seen" = "1" ]; then
    pass "primary scope's local log received the marker via gist polling"
  else
    fail "primary scope did NOT see the marker in 45s — bearer pipeline broken in user scope"
    echo "    user scope last 3 events: $(tail -3 $MY_HOME/messages.jsonl 2>/dev/null | head -c 400)"
  fi
}

# Helper: pre-test cleanup of any leftover test-scope orphans on this
# machine (conservative — only matches our test prefixes).
cleanup_homes_pre() {
  pkill -9 -f "/tmp/airc-orphan-loop\|/tmp/airc-td-orphan\|/tmp/airc-myscope" 2>/dev/null || true
  sleep 1
}

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
  # Defensive: kill orphans that survived teardown's airc.pid-driven
  # kill (background subshells re-write airc.pid after teardown, then
  # stomp guard refuses spawn2). Aggressive pkill targeting just this
  # test scope.
  pkill -9 -f "$A_HOME" 2>/dev/null || true
  sleep 2
  # Also wipe airc.pid in case a surviving subshell re-wrote it.
  rm -f "$A_HOME/state/airc.pid"

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
  clean_install_smoke)            scenario_clean_install_smoke ;;
  passive_recv)                   scenario_passive_recv ;;
  round_trip)                     scenario_round_trip ;;
  idle_then_recv)                 scenario_idle_then_recv ;;
  part_deletes_host_gist)         scenario_part_deletes_host_gist ;;
  status_agrees_with_send)        scenario_status_agrees_with_send ;;
  stale_config_auto_resyncs)      scenario_stale_config_auto_resyncs ;;
  orphan_loops_self_reap)         scenario_orphan_loops_self_reap ;;
  teardown_kills_env_tagged_orphans) scenario_teardown_kills_env_tagged_orphans ;;
  my_scope_in_mesh)               scenario_my_scope_in_mesh ;;
  all)
    scenario_clean_install_smoke
    scenario_orphan_loops_self_reap
    scenario_teardown_kills_env_tagged_orphans
    scenario_passive_recv
    scenario_round_trip
    scenario_status_agrees_with_send
    scenario_stale_config_auto_resyncs
    scenario_part_deletes_host_gist
    scenario_my_scope_in_mesh
    # idle_then_recv last — slow (45s+ idle wait)
    scenario_idle_then_recv
    ;;
  *)
    echo "Usage: $0 [passive_recv|round_trip|idle_then_recv|part_deletes_host_gist|status_agrees_with_send|stale_config_auto_resyncs|orphan_loops_self_reap|teardown_kills_env_tagged_orphans|my_scope_in_mesh|all]"
    exit 2
    ;;
esac

echo
echo "─────────────────"
echo "  ${GRN}pass:${RST} $PASS  ${RED}fail:${RST} $FAIL  ${YLO}skip:${RST} $SKIP"
[ "$FAIL" = "0" ] || exit 1
exit 0
