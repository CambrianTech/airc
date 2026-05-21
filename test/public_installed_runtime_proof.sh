#!/usr/bin/env bash
set -euo pipefail

# Public installed-command proof.
#
# This script is intentionally outside the cargo test harness. It proves the
# thing users and agents actually run after install:
#
#   PATH=~/.local/bin:$PATH airc ...
#
# It must not call target/debug/airc-core directly and must not depend on a
# test-only AIRC_DIR override. State isolation is handled with temporary
# HOME/AIRC_HOME values only for the two fake agent scopes.

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

[ ! -L "$AIRC_BIN" ] || fail "public airc command must be a shim file, not a symlink: $AIRC_BIN"
"$AIRC_BIN" version >/dev/null || fail "airc version failed through public command"

case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *)
    [ ! -e "$(dirname "$AIRC_BIN")/airc-core" ] \
      || fail "POSIX install must not expose stale airc-core on PATH"
    ;;
esac

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/airc-public-proof.XXXXXX")"
cleanup() {
  for scope in \
    "$ROOT/home/continuum/.airc" \
    "$ROOT/home/openclaw/.airc"; do
    if [ -d "$scope" ]; then
      AIRC_HOME="$scope" "$AIRC_BIN" teardown >/dev/null 2>&1 || true
      if [ -f "$scope/airc.pid" ]; then
        while IFS= read -r pid; do
          [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
        done < "$scope/airc.pid"
      fi
    fi
  done
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

wait_for_subscription_file() {
  local scope="$1"
  local deadline=$((SECONDS + 12))
  while [ "$SECONDS" -lt "$deadline" ]; do
    [ -s "$scope/subscriptions.json" ] && return 0
    sleep 1
  done
  return 1
}

run_join_for_scope() {
  local repo="$1"
  local scope="$2"
  local out="$3"
  local err="$4"
  (
    cd "$repo" || exit 1
    HOME="$MACHINE_HOME" \
      AIRC_HOME="$scope" \
      AIRC_NO_DISCOVERY=1 \
      AIRC_NO_GENERAL=1 \
      AIRC_BACKGROUND_OK=1 \
      AIRC_NO_ATTACH=1 \
      "$AIRC_BIN" join --no-gist >"$out" 2>"$err" &
    echo $! > "$scope/join.pid"
  )
  if ! wait_for_subscription_file "$scope"; then
    echo "--- stdout ---" >&2
    cat "$out" >&2 || true
    echo "--- stderr ---" >&2
    cat "$err" >&2 || true
    fail "airc join did not create subscriptions.json for $scope"
  fi
  if [ -f "$scope/join.pid" ]; then
    kill "$(cat "$scope/join.pid")" 2>/dev/null || true
  fi
  AIRC_HOME="$scope" "$AIRC_BIN" teardown >/dev/null 2>&1 || true
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

run_join_for_scope "$CONTINUUM_REPO" "$CONTINUUM_SCOPE" "$ROOT/continuum.out" "$ROOT/continuum.err"
run_join_for_scope "$OPENCLAW_REPO" "$OPENCLAW_SCOPE" "$ROOT/openclaw.out" "$ROOT/openclaw.err"

for channel in general cambriantech openclaw; do
  case "$channel" in
    general)
      grep -q '"general"' "$CONTINUUM_SCOPE/subscriptions.json" \
        || fail "continuum scope did not subscribe #general"
      grep -q '"general"' "$OPENCLAW_SCOPE/subscriptions.json" \
        || fail "openclaw scope did not subscribe #general"
      ;;
    cambriantech)
      grep -q '"cambriantech"' "$CONTINUUM_SCOPE/subscriptions.json" \
        || fail "continuum scope did not subscribe inferred #cambriantech"
      ;;
    openclaw)
      grep -q '"openclaw"' "$OPENCLAW_SCOPE/subscriptions.json" \
        || fail "openclaw scope did not subscribe inferred #openclaw"
      ;;
  esac
done

continuum_general_id="$(json_channel_field "$CONTINUUM_SCOPE/subscriptions.json" general room_id)"
openclaw_general_id="$(json_channel_field "$OPENCLAW_SCOPE/subscriptions.json" general room_id)"
[ -n "$continuum_general_id" ] || fail "continuum #general RoomId missing"
[ -n "$openclaw_general_id" ] || fail "openclaw #general RoomId missing"
[ "$continuum_general_id" = "$openclaw_general_id" ] \
  || fail "#general RoomId diverged between project scopes: $continuum_general_id != $openclaw_general_id"

continuum_general_wire="$(json_channel_field "$CONTINUUM_SCOPE/subscriptions.json" general wire)"
openclaw_general_wire="$(json_channel_field "$OPENCLAW_SCOPE/subscriptions.json" general wire)"
[ -n "$continuum_general_wire" ] || fail "continuum #general wire missing"
[ -n "$openclaw_general_wire" ] || fail "openclaw #general wire missing"
[ "$continuum_general_wire" = "$openclaw_general_wire" ] \
  || fail "#general wire diverged between project scopes: $continuum_general_wire != $openclaw_general_wire"

[ "$continuum_general_wire" = "$MACHINE_HOME_REAL/.airc/wires/general" ] \
  || fail "#general wire must be account-home scoped, got $continuum_general_wire"

log "public airc command, account-room derivation, and same-machine #general wire are coherent"
