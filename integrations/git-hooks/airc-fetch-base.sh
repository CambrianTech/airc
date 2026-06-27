#!/usr/bin/env bash
# airc-fetch-base.sh — fetch-before-commit/push staleness guard.
#
# Card 64621946. Tonight's merge-order hazard: branches were cut off a
# rust-rewrite that was already 5 commits behind, so a slice built on a
# stale base and the integration produced E0063 between slices. This hook
# fetches the integration base (timeout-bounded, throttled, offline-safe)
# and compares local HEAD against it BEFORE a commit (advisory) or a push
# (may hard-gate) so the agent syncs first instead of building stale.
#
# Invoked by .git/hooks/pre-commit and .git/hooks/pre-push, which pass the
# phase as $1:
#   pre-commit  → advisory: warn if behind, ALWAYS exit 0 (never block a commit)
#   pre-push    → gating:   warn if behind AND exit 1 (pushing stale is the hazard)
#
# Optional-transport doctrine applied to git: a failed/slow fetch (offline,
# rate-limited, no network) NEVER blocks. It prints a brief note and the
# behind-check falls back to whatever the last successful fetch left in the
# local origin/<base> ref.
#
# Runs in git-bash on Windows (no visible console window — the calling hook
# is already a bash script invoked by git). Pure bash + git; no airc binary
# dependency, so it works even on a half-installed tree.
#
# Env knobs (all optional):
#   AIRC_HOOK_SKIP=1            disable the guard entirely (escape hatch)
#   AIRC_HOOK_FETCH_TIMEOUT=5   fetch timeout in seconds
#   AIRC_HOOK_THROTTLE=180      skip fetch if one ran < this many seconds ago
#   AIRC_HOOK_BASE=<ref>        force the integration base (else auto-detect)
#   AIRC_HOOK_PUSH_BLOCK=1      pre-push hard-gates when behind (default 1; set 0 for advisory-only push)

set -u

PHASE="${1:-pre-commit}"

# Hard escape hatch — never let the guard be the thing that wedges a commit.
[ "${AIRC_HOOK_SKIP:-0}" = "1" ] && exit 0

FETCH_TIMEOUT="${AIRC_HOOK_FETCH_TIMEOUT:-5}"
THROTTLE="${AIRC_HOOK_THROTTLE:-180}"
PUSH_BLOCK="${AIRC_HOOK_PUSH_BLOCK:-1}"

# Resolve repo root and the marker file. If we are not in a git repo, do
# nothing (defensive; git would not invoke a hook outside one, but the
# script may be sourced or run by tests).
GIT_DIR="$(git rev-parse --git-dir 2>/dev/null)" || exit 0
[ -n "$GIT_DIR" ] || exit 0
MARKER="$GIT_DIR/airc-last-fetch"

# ── Determine the integration base ──────────────────────────────────────
# Preference order:
#   1. AIRC_HOOK_BASE override.
#   2. The branch's own upstream (@{u}), stripped of remote prefix, IF that
#      upstream is a known integration line. A feature branch usually tracks
#      its base (origin/rust-rewrite) once pushed, which is exactly what we
#      want to measure against.
#   3. First of canary / main (current trunk) that exists locally-or-on-remote;
#      rust-rewrite ONLY if confirmed on the remote (its branch is deleted
#      post-#1173 and the stale local ref must not select a dead base).
_remote="origin"

_base_branch=""
if [ -n "${AIRC_HOOK_BASE:-}" ]; then
  _base_branch="$AIRC_HOOK_BASE"
else
  # Upstream of the current branch, if set (e.g. "origin/rust-rewrite").
  _up="$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || true)"
  case "$_up" in
    origin/*)
      _cand="${_up#origin/}"
      case "$_cand" in
        rust-rewrite|canary|main) _base_branch="$_cand" ;;
      esac
      ;;
  esac
  if [ -z "$_base_branch" ]; then
    # canary is the current integration trunk; rust-rewrite is being
    # phased out and its remote branch is DELETED post-#1173 promotion —
    # but a stale local `origin/rust-rewrite` tracking ref persists and
    # `show-ref` happily verifies it, so a rust-rewrite-first order picks
    # a dead base. Try canary/main first; only fall back to rust-rewrite
    # if it is confirmed on the REMOTE (ls-remote), never on a stale
    # local ref alone.
    for _cand in canary main; do
      if git show-ref --verify --quiet "refs/remotes/$_remote/$_cand" \
         || git ls-remote --exit-code --heads "$_remote" "$_cand" >/dev/null 2>&1; then
        _base_branch="$_cand"
        break
      fi
    done
    if [ -z "$_base_branch" ] \
       && git ls-remote --exit-code --heads "$_remote" rust-rewrite >/dev/null 2>&1; then
      _base_branch="rust-rewrite"
    fi
  fi
fi

# No discoverable base → nothing to compare; succeed silently.
[ -n "$_base_branch" ] || exit 0

BASE_REF="$_remote/$_base_branch"

# ── Throttle: skip the network fetch if one ran recently ────────────────
# mtime-age of the marker. On any successful OR attempted fetch we touch it
# so a flapping-offline machine still backs off the network. The behind-check
# below always runs against whatever local origin/<base> we have, throttled
# or not, so rapid commits still get the cheap local advisory.
_now="$(date +%s 2>/dev/null || echo 0)"
_should_fetch=1
if [ -f "$MARKER" ]; then
  _mtime="$(date -r "$MARKER" +%s 2>/dev/null || echo 0)"
  if [ "$_mtime" -gt 0 ] && [ "$_now" -gt 0 ]; then
    _age=$(( _now - _mtime ))
    if [ "$_age" -ge 0 ] && [ "$_age" -lt "$THROTTLE" ]; then
      _should_fetch=0
    fi
  fi
fi

# ── Timeout-bounded fetch helper ────────────────────────────────────────
# Prefer coreutils `timeout` (present in git-bash on Windows). Fall back to
# a background+wait+kill if it is missing. Either way a hung/slow fetch is
# bounded and never blocks. Returns 0 on a successful fetch, non-zero on
# failure OR timeout — the caller treats both as "fetch skipped".
_bounded_fetch() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$FETCH_TIMEOUT" git fetch --quiet "$_remote" "$_base_branch" >/dev/null 2>&1
    return $?
  fi
  # Fallback: background the fetch, kill it if it overruns the budget.
  git fetch --quiet "$_remote" "$_base_branch" >/dev/null 2>&1 &
  local _pid=$!
  local _waited=0
  while kill -0 "$_pid" 2>/dev/null; do
    if [ "$_waited" -ge "$FETCH_TIMEOUT" ]; then
      kill "$_pid" 2>/dev/null
      wait "$_pid" 2>/dev/null
      return 124
    fi
    sleep 1
    _waited=$(( _waited + 1 ))
  done
  wait "$_pid"
  return $?
}

_fetch_ok=skip
if [ "$_should_fetch" = "1" ]; then
  # Touch the marker BEFORE the fetch so even a hung/killed fetch advances
  # the throttle clock (offline machines must not retry every commit).
  : > "$MARKER" 2>/dev/null || true
  if _bounded_fetch; then
    _fetch_ok=yes
    : > "$MARKER" 2>/dev/null || true
  else
    _fetch_ok=no
  fi
fi

# ── Behind-check (cheap, local) ─────────────────────────────────────────
# Always run, throttled or not. Counts commits reachable from BASE_REF but
# not from HEAD. If origin/<base> ref does not exist locally (never fetched,
# offline first run) the rev-list fails → treat as "unknown", do not block.
_behind=""
if git show-ref --verify --quiet "refs/remotes/$BASE_REF"; then
  _behind="$(git rev-list --count "HEAD..$BASE_REF" 2>/dev/null || echo "")"
fi

# Brief offline note (only when we actually tried and failed — not on throttle).
if [ "$_fetch_ok" = "no" ]; then
  printf '  \033[1;33m!\033[0m airc: fetch skipped (offline?) — staleness check used last-known %s\n' "$BASE_REF" >&2
fi

# Nothing to say if we could not compute a behind-count.
if [ -z "$_behind" ] || [ "$_behind" = "0" ]; then
  exit 0
fi

# ── Behind by N>0 → LOUD advisory ───────────────────────────────────────
RED=$'\033[1;31m'; YEL=$'\033[1;33m'; RST=$'\033[0m'
{
  # Brace EVERY color var: an unbraced `$YEL` immediately followed by a
  # multibyte box-drawing char (┌│└) is parsed as the variable name
  # `YEL┌…` and crashes under `set -u` on bash 3.2 + a multibyte locale
  # ("YEL�: unbound variable") — exactly the macOS default shell.
  printf '%s\n' "${YEL}┌──────────────────────────────────────────────────────────────${RST}"
  printf '%s\n' "${YEL}│ ⚠  BEHIND $BASE_REF BY $_behind COMMIT(S)${RST}"
  printf '%s\n' "${YEL}│    Your base is stale — building on it risks merge-order"
  printf '%s\n' "${YEL}│    breakage (E0063-between-slices class).${RST}"
  printf '%s\n' "${YEL}│    Sync first:${RST}"
  printf '%s\n' "${YEL}│      git pull --ff-only $_remote $_base_branch${RST}"
  printf '%s\n' "${YEL}│      # or: git rebase $BASE_REF${RST}"
  printf '%s\n' "${YEL}└──────────────────────────────────────────────────────────────${RST}"
} >&2

if [ "$PHASE" = "pre-push" ] && [ "$PUSH_BLOCK" = "1" ]; then
  printf '%s\n' "${RED}✗ airc: refusing to push a branch built on a base $_behind commit(s) behind $BASE_REF.$RST" >&2
  printf '%s\n' "${RED}  Sync and retry, or set AIRC_HOOK_PUSH_BLOCK=0 to override for this push.$RST" >&2
  exit 1
fi

# pre-commit (or advisory-only push) — warn but never block.
exit 0
