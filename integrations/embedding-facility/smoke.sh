#!/usr/bin/env bash
# Live smoke for the 5090 embedding facility's GPU server (slice 1).
#
# Validates the ONE piece that unit tests + `docker compose config` cannot: that
# llama.cpp `--embedding` actually loads the canonical model on this GPU
# (notably Blackwell / sm_120 on the RTX 5090) and returns a real vector over
# the OpenAI `/v1/embeddings` endpoint. The bridge + wire contract are covered by
# `cargo test`; THIS covers the GPU round-trip.
#
# Usage (from this directory, with Docker engine up):
#   ./smoke.sh
# Env overrides mirror docker-compose.yml: EMBED_HF_REPO, EMBED_HF_FILE, etc.
# First run pulls the GGUF to the model cache — allow several minutes.
#
# Exit 0 = a non-empty embedding came back; the facility's GPU half is proven on
# this box. Non-zero (with a reason) otherwise. Leaves the stack running unless
# KEEP_UP=0, in which case it tears down on exit.

set -euo pipefail

cd "$(dirname "$0")"

PORT="${EMBED_SMOKE_PORT:-8080}"
BASE="http://127.0.0.1:${PORT}"
HEALTH_TIMEOUT="${EMBED_SMOKE_HEALTH_TIMEOUT:-600}" # seconds; first run pulls the model
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

log "starting the embedding server (docker compose up -d) ..."
docker compose up -d

log "waiting for /health (up to ${HEALTH_TIMEOUT}s — first run pulls the GGUF) ..."
deadline=$(( $(date +%s) + HEALTH_TIMEOUT ))
until curl -fsS "${BASE}/health" >/dev/null 2>&1; do
  if [ "$(date +%s)" -ge "${deadline}" ]; then
    log "server did not become healthy; last 40 log lines:"
    docker compose logs --tail=40 || true
    fail "timed out waiting for ${BASE}/health (sm_120/Blackwell kernel miss? see README fallback)"
  fi
  sleep 3
done
log "server healthy."

log "requesting an embedding for a probe input ..."
resp="$(curl -fsS "${BASE}/v1/embeddings" \
  -H 'content-type: application/json' \
  -d '{"input":"hello grid — embedding facility smoke"}')" \
  || fail "POST /v1/embeddings failed"

# Assert a non-empty numeric embedding came back, and report its dimension.
dim=""
if command -v python3 >/dev/null 2>&1; then
  dim="$(printf '%s' "$resp" | python3 -c \
    'import sys,json; d=json.load(sys.stdin)["data"][0]["embedding"]; assert d and all(isinstance(x,(int,float)) for x in d); print(len(d))' \
    2>/dev/null)" || fail "response had no valid embedding array: ${resp:0:200}"
else
  # No python3: heuristic check that an embedding array with numbers is present.
  printf '%s' "$resp" | grep -Eq '"embedding"[[:space:]]*:[[:space:]]*\[[[:space:]]*-?[0-9]' \
    || fail "response had no embedding array: ${resp:0:200}"
  dim="(install python3 for exact dim)"
fi

log "OK — embedding returned. dim=${dim}. The 5090 facility GPU half is PROVEN on this box. 🚀"
log "advertise it on the grid:  cargo run -p airc-embedding-bridge   (needs a running airc daemon)"
