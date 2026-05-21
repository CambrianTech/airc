#!/usr/bin/env bash
set -euo pipefail

# Public installed-command proof.
#
# Verifies the contract a stranger landing at the repo + running
# `curl install.sh | bash` would experience: the Rust binary is at
# `~/.local/bin/airc`, the substrate primitives behind `airc join`
# work, and same-account project scopes converge on a shared
# `#general` wire.
#
# Post-demolition (PR D / #864): the bash wrapper is gone. This
# proof tests the Rust binary directly. The earlier wrapper-shape
# `--no-gist` / `--channel` / `--attach` / env-var flags don't
# apply; those were wrapper concerns.
#
# Coverage that defers to later architecture PRs:
#   - msg/attach/monitor live narration → PR 1 (daemon auto-start)
#                                       + PR 2 (`airc attach` subcommand)
#   - Codex hook injection              → PR 3 (hook reads daemon socket)
#
# Until those land, this proof verifies what the substrate CAN
# verify today through the public command: subscription set,
# RoomId derivation, machine-account wire promotion.

fail() {
  echo "public installed runtime proof failed: $*" >&2
  exit 1
}

log() {
  printf 'proof: %s\n' "$*"
}

AIRC_BIN="${AIRC_BIN:-$(command -v airc 2>/dev/null || true)}"
[ -n "$AIRC_BIN" ] || fail "airc is not on PATH"
[ -x "$AIRC_BIN" ] || fail "airc is not executable: $AIRC_BIN"

expected_bin="${AIRC_EXPECT_BIN:-$HOME/.local/bin/airc}"
if [ -n "${AIRC_EXPECT_BIN:-}" ] || [ -e "$expected_bin" ]; then
  [ "$AIRC_BIN" = "$expected_bin" ] || fail "airc resolved to $AIRC_BIN, expected $expected_bin"
fi

# Clap exposes the version via `--version`, not a `version` subcommand.
"$AIRC_BIN" --version >/dev/null || fail "airc --version failed"

# Demolition contract: no stale `airc-core` on PATH next to the
# real binary. Symlink to source-tree target/release/airc IS the
# expected shape (legacy "must be a shim file" check was wrapper-era).
case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *)
    [ ! -e "$(dirname "$AIRC_BIN")/airc-core" ] \
      || fail "POSIX install must not expose stale airc-core on PATH"
    ;;
esac

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/airc-public-proof.XXXXXX")"
cleanup() {
  rm -rf "$ROOT"
}
trap cleanup EXIT INT TERM

MACHINE_HOME="$ROOT/home"
CONTINUUM_REPO="$MACHINE_HOME/continuum"
OPENCLAW_REPO="$MACHINE_HOME/openclaw"
CONTINUUM_SCOPE="$CONTINUUM_REPO/.airc"
OPENCLAW_SCOPE="$OPENCLAW_REPO/.airc"

mkdir -p "$CONTINUUM_REPO/.git" "$OPENCLAW_REPO/.git" "$CONTINUUM_SCOPE" "$OPENCLAW_SCOPE"
MACHINE_HOME_REAL="$(cd "$MACHINE_HOME" && pwd -P)"

cat > "$CONTINUUM_REPO/.git/config" <<'EOF'
[remote "origin"]
    url = https://github.com/CambrianTech/continuum.git
EOF

cat > "$OPENCLAW_REPO/.git/config" <<'EOF'
[remote "origin"]
    url = https://github.com/OpenClaw/openclaw.git
EOF

# Pin mesh identity so the RoomId derivation is deterministic in CI
# (no `gh api user` shell-out). Operator source is documented as
# trusted as-is.
write_operator_identity() {
  local scope="$1"
  cat > "$scope/mesh_identity.json" <<'EOF'
{
  "version": 1,
  "identity": "joelteply",
  "source": "operator",
  "resolved_at_ms": 1,
  "ttl_ms": 86400000
}
EOF
}

write_operator_identity "$CONTINUUM_SCOPE"
write_operator_identity "$OPENCLAW_SCOPE"

run_join_for_scope() {
  local repo="$1"
  local scope="$2"
  local out="$3"
  local err="$4"
  (
    cd "$repo" || exit 1
    HOME="$MACHINE_HOME" AIRC_HOME="$scope" \
      "$AIRC_BIN" --home "$scope" join >"$out" 2>"$err"
  ) || {
    echo "--- join stdout ---" >&2
    cat "$out" >&2 || true
    echo "--- join stderr ---" >&2
    cat "$err" >&2 || true
    fail "airc join failed for $scope"
  }
  [ -s "$scope/subscriptions.json" ] \
    || fail "airc join did not create subscriptions.json for $scope"
}

json_channel_field() {
  local file="$1"
  local channel="$2"
  local field="$3"
  awk -v channel="\"$channel\"" -v field="\"$field\"" '
    $0 ~ channel"[[:space:]]*:" { in_channel = 1; next }
    in_channel && $0 ~ /^[[:space:]]*}/ { in_channel = 0 }
    in_channel && index($0, field) {
      line = $0
      sub(/^[^:]*:[[:space:]]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$file"
}

log "joining continuum scope"
run_join_for_scope "$CONTINUUM_REPO" "$CONTINUUM_SCOPE" "$ROOT/continuum.out" "$ROOT/continuum.err"
log "joining openclaw scope"
run_join_for_scope "$OPENCLAW_REPO" "$OPENCLAW_SCOPE" "$ROOT/openclaw.out" "$ROOT/openclaw.err"

# Both scopes must have subscribed to #general (the account lobby)
# AND the inferred org/project channel (cambriantech / openclaw).
grep -q '"general"' "$CONTINUUM_SCOPE/subscriptions.json" \
  || fail "continuum scope did not subscribe #general"
grep -q '"general"' "$OPENCLAW_SCOPE/subscriptions.json" \
  || fail "openclaw scope did not subscribe #general"
grep -q '"cambriantech"' "$CONTINUUM_SCOPE/subscriptions.json" \
  || fail "continuum scope did not subscribe inferred #cambriantech"
grep -q '"openclaw"' "$OPENCLAW_SCOPE/subscriptions.json" \
  || fail "openclaw scope did not subscribe inferred #openclaw"

# The cross-scope architectural truth: same mesh identity + same
# channel name → same RoomId via #843's identity-namespaced
# derivation. Two scopes on the same gh account converge on a
# single #general room without coordination.
continuum_general_id="$(json_channel_field "$CONTINUUM_SCOPE/subscriptions.json" general room_id)"
openclaw_general_id="$(json_channel_field "$OPENCLAW_SCOPE/subscriptions.json" general room_id)"
[ -n "$continuum_general_id" ] || fail "continuum #general RoomId missing"
[ -n "$openclaw_general_id" ] || fail "openclaw #general RoomId missing"
[ "$continuum_general_id" = "$openclaw_general_id" ] \
  || fail "#general RoomId diverged between project scopes: $continuum_general_id != $openclaw_general_id"

# The machine-account wire promotion: project scopes under $HOME
# share the wire at $HOME/.airc/wires/<channel> rather than
# project-local wires. Tested via #844 + #861.
continuum_general_wire="$(json_channel_field "$CONTINUUM_SCOPE/subscriptions.json" general wire)"
openclaw_general_wire="$(json_channel_field "$OPENCLAW_SCOPE/subscriptions.json" general wire)"
[ -n "$continuum_general_wire" ] || fail "continuum #general wire missing"
[ -n "$openclaw_general_wire" ] || fail "openclaw #general wire missing"
[ "$continuum_general_wire" = "$openclaw_general_wire" ] \
  || fail "#general wire diverged between project scopes: $continuum_general_wire != $openclaw_general_wire"
[ "$continuum_general_wire" = "$MACHINE_HOME_REAL/.airc/wires/general" ] \
  || fail "#general wire must be account-home scoped, got $continuum_general_wire"

log "all checks passed"
