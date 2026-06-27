#!/usr/bin/env bash
# fetch_before_commit_hook.sh — tests for the fetch-before-commit/push
# staleness guard (card 64621946: integrations/git-hooks/airc-fetch-base.sh).
#
# Spins up throwaway repos with a fake "origin" (a local bare repo) so no
# network is touched, and asserts the binding behaviors:
#   1. origin ahead  → behind-advisory printed.
#   2. pre-push when behind → hard-gate (exit 1).
#   3. pre-commit when behind → advisory only (exit 0, never blocks).
#   4. origin unreachable → warns "fetch skipped (offline?)" AND exit 0
#      (the key one — a failed fetch must never block).
#   5. recent .git/airc-last-fetch marker → fetch is throttled (skipped),
#      behind-check still runs against the last-known ref.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKER="$ROOT/integrations/git-hooks/airc-fetch-base.sh"
[ -f "$WORKER" ] || { echo "fetch-hook test failed: worker not found: $WORKER" >&2; exit 1; }

PASS=0
fail() { echo "fetch-hook test FAILED: $*" >&2; exit 1; }
ok()   { PASS=$((PASS+1)); echo "  ok: $*"; }

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t airchook)"
cleanup() { rm -rf "$TMP" 2>/dev/null || true; }
trap cleanup EXIT

export GIT_AUTHOR_NAME="Test" GIT_AUTHOR_EMAIL="test@test" \
       GIT_COMMITTER_NAME="Test" GIT_COMMITTER_EMAIL="test@test"
# Deterministic + fast.
export AIRC_HOOK_BASE="rust-rewrite" AIRC_HOOK_FETCH_TIMEOUT="5" AIRC_HOOK_THROTTLE="180"

# ── Build: bare origin on rust-rewrite, a clone, then advance origin ────
ORIGIN="$TMP/origin.git"
WORK="$TMP/work"
git init --quiet --bare "$ORIGIN"

# Seed the bare repo via a scratch clone on rust-rewrite.
SEED="$TMP/seed"
git clone --quiet "$ORIGIN" "$SEED"
( cd "$SEED"
  git checkout --quiet -b rust-rewrite
  echo one > f.txt; git add f.txt; git commit --quiet -m "c1"
  git push --quiet -u origin rust-rewrite
)

# The work clone tracks origin/rust-rewrite at c1.
git clone --quiet "$ORIGIN" "$WORK"
( cd "$WORK" && git checkout --quiet rust-rewrite )

# Advance origin by 2 commits so the work clone is "behind by 2".
( cd "$SEED"
  echo two >> f.txt; git add f.txt; git commit --quiet -m "c2"
  echo three >> f.txt; git add f.txt; git commit --quiet -m "c3"
  git push --quiet origin rust-rewrite
)

run_hook() { # phase, cwd  → prints output, returns hook rc
  ( cd "$2" && bash "$WORKER" "$1" 2>&1 )
}

# ── 1+3. pre-commit advisory: warns "behind ... by 2", exits 0 ──────────
rm -f "$WORK/.git/airc-last-fetch"
set +e
out="$(cd "$WORK" && bash "$WORKER" pre-commit 2>&1)"; rc=$?
set -e
echo "$out" | grep -qiE "behind .*rust-rewrite.* by 2" || fail "pre-commit did not warn behind-by-2; got: $out"
[ "$rc" -eq 0 ] || fail "pre-commit must exit 0 (advisory) even when behind; got rc=$rc"
ok "pre-commit advisory: warns behind-by-2 and exits 0"

# ── 2. pre-push hard-gate when behind (exit 1) ──────────────────────────
rm -f "$WORK/.git/airc-last-fetch"
set +e
out="$(cd "$WORK" && bash "$WORKER" pre-push 2>&1)"; rc=$?
set -e
echo "$out" | grep -qiE "behind .*rust-rewrite.* by 2" || fail "pre-push did not warn behind; got: $out"
[ "$rc" -eq 1 ] || fail "pre-push must hard-gate (exit 1) when behind; got rc=$rc"
ok "pre-push hard-gates (exit 1) when behind"

# ── pre-push override AIRC_HOOK_PUSH_BLOCK=0 → advisory only ─────────────
rm -f "$WORK/.git/airc-last-fetch"
set +e
out="$(cd "$WORK" && AIRC_HOOK_PUSH_BLOCK=0 bash "$WORKER" pre-push 2>&1)"; rc=$?
set -e
[ "$rc" -eq 0 ] || fail "pre-push with PUSH_BLOCK=0 must exit 0; got rc=$rc"
ok "pre-push respects AIRC_HOOK_PUSH_BLOCK=0 (advisory)"

# ── 4. origin UNREACHABLE → warns offline AND exits 0 (never blocks) ────
# Point origin at a dead path so the fetch fails fast. Remove the local
# tracking ref so there is no behind-count to compute — proves a failed
# fetch alone never blocks, on the most stale-prone phase (pre-push).
DEAD="$TMP/work_offline"
git clone --quiet "$ORIGIN" "$DEAD"
( cd "$DEAD" && git checkout --quiet rust-rewrite )
( cd "$DEAD" && git remote set-url origin "$TMP/does-not-exist.git" )
rm -f "$DEAD/.git/airc-last-fetch"
set +e
out="$(cd "$DEAD" && bash "$WORKER" pre-push 2>&1)"; rc=$?
set -e
echo "$out" | grep -qiE "fetch skipped \(offline" || fail "offline run did not print 'fetch skipped (offline?)'; got: $out"
[ "$rc" -eq 0 ] || fail "offline fetch must NOT block (exit 0); got rc=$rc"
ok "offline (fetch fails) → warns + exits 0, never blocks (KEY behavior)"

# ── 5. recent marker → fetch throttled (skipped) ────────────────────────
# Set the marker to "now" and point origin at a dead URL. If the hook
# honored the throttle it will NOT attempt the fetch, so it must NOT print
# the offline note. Behind-check still runs against the last-known ref
# (none here for this fresh clone → no advisory, clean exit 0).
THROT="$TMP/work_throttle"
git clone --quiet "$ORIGIN" "$THROT"
( cd "$THROT" && git checkout --quiet rust-rewrite )
( cd "$THROT" && git remote set-url origin "$TMP/does-not-exist.git" )
: > "$THROT/.git/airc-last-fetch"   # fresh mtime = now
set +e
out="$(cd "$THROT" && bash "$WORKER" pre-commit 2>&1)"; rc=$?
set -e
if echo "$out" | grep -qiE "fetch skipped \(offline"; then
  fail "throttle did not skip the fetch (got offline note from a dead remote): $out"
fi
[ "$rc" -eq 0 ] || fail "throttled pre-commit must exit 0; got rc=$rc"
ok "recent marker throttles the fetch (no network attempt)"

echo "fetch-before-commit hook: all $PASS checks passed"
