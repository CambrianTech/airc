#!/usr/bin/env bash
# cargo_gate_hook.sh: tests for the pre-push cargo gate
# (integrations/git-hooks/airc-cargo-gate.sh, card 32338302).
#
# A fake `cargo` on PATH records every call and fails on request, so nothing compiles:
# these run on any machine, including one that must not build. Asserted:
#   1. default: fmt then clippy run, and the line says "fmt + clippy".
#   2. airc.cargoGate=fmt-only (durable, per machine): fmt runs, clippy does NOT, and the
#      line says clippy and the compile were left to CI and names the setting.
#   3. AIRC_CARGO_GATE_CLIPPY=0 (per push): the same, naming the env knob.
#   4. fmt not clean: blocked (exit 1), clippy never runs.
#   5. clippy failing in the default mode: blocked (exit 1).
# No outcome line may claim clippy passed when clippy did not run.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GATE="$ROOT/integrations/git-hooks/airc-cargo-gate.sh"
[ -f "$GATE" ] || { echo "cargo-gate test failed: gate not found: $GATE" >&2; exit 1; }

PASS=0
fail() { echo "cargo-gate test FAILED: $*" >&2; exit 1; }
ok()   { PASS=$((PASS+1)); echo "  ok: $*"; }

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t aircgate)"
cleanup() { rm -rf "$TMP" 2>/dev/null || true; }
trap cleanup EXIT

# A workspace the gate recognizes, and a private global git config.
WORK="$TMP/work"
mkdir -p "$WORK" "$TMP/bin"
git init --quiet "$WORK"
touch "$WORK/Cargo.toml"
export GIT_CONFIG_GLOBAL="$TMP/gitconfig"
: > "$GIT_CONFIG_GLOBAL"

# The fake cargo: log the subcommand; fail it if CARGO_FAKE_FAIL names it.
cat > "$TMP/bin/cargo" <<'FAKE'
#!/usr/bin/env bash
echo "$1" >> "$CARGO_FAKE_LOG"
[ "${CARGO_FAKE_FAIL:-}" = "$1" ] && exit 1
exit 0
FAKE
chmod +x "$TMP/bin/cargo"
export PATH="$TMP/bin:$PATH" CARGO_FAKE_LOG="$TMP/cargo.log"

run() { : > "$CARGO_FAKE_LOG"; ( cd "$WORK" && bash "$GATE" pre-push ) > "$TMP/out" 2>&1; }

# 1. default
run || fail "default mode blocked"
grep -qx fmt "$CARGO_FAKE_LOG" && grep -qx clippy "$CARGO_FAKE_LOG" || fail "default did not run fmt and clippy"
grep -q "fmt + clippy" "$TMP/out" || fail "default line: $(cat "$TMP/out")"
ok "default runs fmt and clippy and says so"

# 2. durable per-machine setting
git config --global airc.cargoGate fmt-only
run || fail "fmt-only blocked"
grep -qx fmt "$CARGO_FAKE_LOG" || fail "fmt-only did not run fmt"
! grep -qx clippy "$CARGO_FAKE_LOG" || fail "fmt-only compiled (clippy ran)"
grep -q "left to CI (airc.cargoGate=fmt-only" "$TMP/out" || fail "fmt-only line: $(cat "$TMP/out")"
! grep -q "fmt + clippy" "$TMP/out" || fail "fmt-only claimed clippy passed"
ok "airc.cargoGate=fmt-only runs no compile and says so"
git config --global --unset airc.cargoGate

# 3. per-push env knob
AIRC_CARGO_GATE_CLIPPY=0 run || fail "CLIPPY=0 blocked"
! grep -qx clippy "$CARGO_FAKE_LOG" || fail "CLIPPY=0 ran clippy"
grep -q "left to CI (AIRC_CARGO_GATE_CLIPPY=0)" "$TMP/out" || fail "CLIPPY=0 line: $(cat "$TMP/out")"
! grep -q "fmt + clippy" "$TMP/out" || fail "CLIPPY=0 claimed clippy passed"
ok "AIRC_CARGO_GATE_CLIPPY=0 says clippy was left to CI"

# 4. fmt not clean blocks, clippy never runs
if CARGO_FAKE_FAIL=fmt run; then fail "unformatted code was not blocked"; fi
! grep -qx clippy "$CARGO_FAKE_LOG" || fail "clippy ran after fmt failed"
ok "fmt failure blocks before any compile"

# 5. clippy failure blocks in the default mode
if CARGO_FAKE_FAIL=clippy run; then fail "a clippy failure was not blocked"; fi
ok "clippy failure blocks"

echo "cargo-gate hook: $PASS checks passed"
