#!/usr/bin/env bash
# airc-cargo-gate.sh — pre-push cargo fmt + clippy gate.
#
# Card cab380c0. Recurring cost: pushing airc Rust changes without running
# `cargo fmt` (or with clippy warnings) fails the `cargo fmt --check` /
# `cargo clippy -D warnings` CI gates — each burning a multi-minute CI
# round-trip plus a manual fixup commit (it bit both #1288 and #1289 in one
# session). This runs both checks LOCALLY at pre-push so the failure surfaces
# in seconds, before the push ever leaves the machine.
#
# Invoked by .git/hooks/pre-push with the phase as $1. NO-OP on any other
# phase: fmt+clippy on every commit is too slow, so push is the right gate.
#
# Optional-transport doctrine applied to tooling: if cargo is absent, the
# tree isn't the airc workspace, or CI-parity can't be established, it exits
# 0 (never block a push on a toolchain gap). fmt violations and clippy
# warnings DO block — that is the whole point.
#
# Runs in git-bash on Windows and any POSIX sh with bash. Pure bash + cargo;
# no airc binary dependency, so it works on a half-installed tree.
#
# Env knobs (all optional):
#   AIRC_HOOK_SKIP=1           disable ALL airc hooks (shared escape hatch)
#   AIRC_CARGO_GATE_SKIP=1     disable just this gate (emergency push)
#   AIRC_CARGO_GATE_CLIPPY=0   fmt-check only; skip the slower clippy pass

set -u

PHASE="${1:-}"
# Push is the gate; fmt+clippy on every commit would make commits too slow.
[ "$PHASE" = "pre-push" ] || exit 0
[ "${AIRC_HOOK_SKIP:-0}" = "1" ] && exit 0
[ "${AIRC_CARGO_GATE_SKIP:-0}" = "1" ] && exit 0

# No cargo → nothing to gate (never block on a toolchain gap).
command -v cargo >/dev/null 2>&1 || exit 0

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$REPO_ROOT" ] || exit 0
[ -f "$REPO_ROOT/Cargo.toml" ] || exit 0   # not the workspace root; nothing to gate
cd "$REPO_ROOT" || exit 0

# Share the ONE cargo cache so clippy reuses incremental artifacts (fast) and
# never balloons a per-invocation ghost target dir. Honor an existing value.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.continuum/cache/cargo-target}"

if ! cargo fmt --check; then
  echo "airc pre-push: BLOCKED — code is not cargo-fmt clean." >&2
  echo "  Fix: cargo fmt   (then re-push)" >&2
  echo "  Skip: AIRC_CARGO_GATE_SKIP=1 git push …" >&2
  exit 1
fi

if [ "${AIRC_CARGO_GATE_CLIPPY:-1}" = "1" ]; then
  if ! cargo clippy --all-targets -- -D warnings; then
    echo "airc pre-push: BLOCKED — cargo clippy found warnings/errors (deny-warnings)." >&2
    echo "  Fix the lints above, or skip clippy only: AIRC_CARGO_GATE_CLIPPY=0 git push …" >&2
    exit 1
  fi
fi

echo "airc pre-push: cargo gate clean (fmt + clippy)."
exit 0
