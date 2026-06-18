#!/usr/bin/env bash
# Live smoke for the 5090 generate facility's GPU server (slice 1).
#
# Validates the piece unit tests + `docker compose config` can't: that llama.cpp
# actually loads the capable model on this GPU (notably Blackwell / sm_120 on the
# RTX 5090) and returns a real chat completion over `/v1/chat/completions`. The
# bridge + wire are covered by `cargo test`; THIS covers the GPU generation.
#
# Usage (from this directory, Docker engine up):
#   ./smoke.sh
# Env overrides mirror docker-compose.yml: GEN_HF_REPO, GEN_HF_FILE, etc.
# First run pulls the (large) GGUF to the model cache — allow many minutes.
#
# Exit 0 = a non-empty completion came back; the facility's GPU half is proven.
# Leaves the stack running unless KEEP_UP=0.

set -euo pipefail

cd "$(dirname "$0")"

PORT="${GEN_SMOKE_PORT:-8081}"
BASE="http://127.0.0.1:${PORT}"
HEALTH_TIMEOUT="${GEN_SMOKE_HEALTH_TIMEOUT:-900}" # large model: long first load
KEEP_UP="${KEEP_UP:-1}"

log() { printf '\n\033[1m[smoke]\033[0m %s\n' "$*"; }
fail() { printf '\n\033[31m[smoke FAIL]\033[0m %s\n' "$*" >&2; exit 1; }

command -v docker >/dev/null 2>&1 || fail "docker not on PATH"
docker info >/dev/null 2>&1 || fail "docker engine not responding (start Docker Desktop / the daemon)"

cleanup() {
  if [ "${KEEP_UP}" = "0" ]; then
    log "tearing down (KEEP_UP=0)"
    docker compose down >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

log "starting the generation server (docker compose up -d) ..."
docker compose up -d

log "waiting for /health (up to ${HEALTH_TIMEOUT}s — first run pulls a large GGUF) ..."
deadline=$(( $(date +%s) + HEALTH_TIMEOUT ))
until curl -fsS "${BASE}/health" >/dev/null 2>&1; do
  if [ "$(date +%s)" -ge "${deadline}" ]; then
    log "server did not become healthy; last 40 log lines:"
    docker compose logs --tail=40 || true
    fail "timed out waiting for ${BASE}/health (sm_120/Blackwell kernel miss? see ../embedding-facility/README fallback)"
  fi
  sleep 3
done
log "server healthy."

log "requesting a chat completion ..."
resp="$(curl -fsS "${BASE}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{"messages":[{"role":"user","content":"Reply with exactly: grid online"}],"max_tokens":16,"temperature":0}')" \
  || fail "POST /v1/chat/completions failed"

# Assert a non-empty assistant message came back, and echo it. Pure sed/grep —
# no Python runtime (Rust-cutover guard forbids a Python dependency on the
# live path). Pull the first "content":"..." string out of the JSON.
text="$(printf '%s' "$resp" | sed -n 's/.*"content"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[ -n "$text" ] \
  || fail "response had no completion content: ${resp:0:200}"

log "OK — completion returned: \"${text}\". The 5090 generate facility GPU half is PROVEN. 🚀"
log "advertise it on the grid:  cargo run -p airc-generate-bridge   (needs a running airc daemon)"
