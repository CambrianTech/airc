#!/usr/bin/env bash
# mesh-converge.sh — the convergence smoke: N airc nodes, separate
# ~/.airc each (real isolated "machines"), one shared gh token (one
# account), on a shared bridge network. Asserts they ACTUALLY converge —
# every node sees every OTHER node's specific identity — or fails loud.
#
# The contract's "two isolated machine homes converge" acceptance test
# with REAL isolation (containers), runnable in Linux so Windows Smart App
# Control can't block the loop. Usage: docker/mesh-converge.sh [N]
#
# Honesty notes (a prior version false-positived — sentinel-caught):
#   - `airc join` self-enrols the node's own identity, so `airc peers`
#     ALWAYS lists the node itself. We must check for the OTHER nodes'
#     specific peer_ids, never a generic line count (line 1 is always self
#     and a count>=N-1 check can never fail for N=2).
#   - registry sync can be REFUSED by the gh governor (rate budget). That
#     means no publish/discover happened — it must FAIL the run, not sail
#     past to the assertion. We raise the per-node local budget for the
#     test so the governor doesn't throttle the test itself (GitHub's real
#     5000/hr limit still applies); a throttle here is then a real bug.
set -uo pipefail

N="${1:-2}"
IMAGE="airc-node:dev"
NET="airc-mesh-test"
TOKEN="$(gh auth token 2>/dev/null)"
[ -z "$TOKEN" ] && { echo "FAIL: no gh token (run: gh auth login)"; exit 1; }

cleanup() {
  for i in $(seq 1 "$N"); do docker rm -f "airc-node-$i" >/dev/null 2>&1; done
  docker network rm "$NET" >/dev/null 2>&1
}
trap cleanup EXIT
cleanup

echo "== bring up $N isolated nodes on one account =="
docker network create "$NET" >/dev/null
for i in $(seq 1 "$N"); do
  docker run -d --name "airc-node-$i" --network "$NET" \
    -e GH_TOKEN="$TOKEN" -e HOME=/node \
    -e AIRC_GH_MAX_REQUESTS_PER_MIN=500 \
    "$IMAGE" sleep infinity >/dev/null
  echo "  node-$i up"
done

run() { docker exec "airc-node-$1" sh -lc "$2" 2>&1; }

echo "== each node: airc join, capture its OWN peer_id =="
declare -a ID
for i in $(seq 1 "$N"); do
  run "$i" 'airc join >/dev/null 2>&1'
  ID[$i]="$(run "$i" 'airc status 2>/dev/null' | awk '/peer_id/{print $2}')"
  echo "  node-$i peer_id=${ID[$i]:-<none>}"
  [ -z "${ID[$i]:-}" ] && { echo "  !! node-$i has no peer_id (daemon didn't start)"; }
done

# Convergence is EVENTUALLY consistent: a single sync only sees peers that
# had already published when it ran (a node that synced first misses late
# publishers). So we re-sync in rounds until every node sees every other
# node's specific peer_id, or a bounded number of rounds elapses. This is
# what the 120s refresh loop does in production, compressed for the test.
ROUNDS="${MESH_CONVERGE_ROUNDS:-5}"
converged=0
for round in $(seq 1 "$ROUNDS"); do
  for i in $(seq 1 "$N"); do
    out="$(run "$i" 'airc registry sync 2>&1' | tail -1)"
    echo "$out" | grep -qiE 'budget exceeded|backoff active' \
      && echo "  round $round node-$i: THROTTLED ($out)"
  done
  ok=1
  for i in $(seq 1 "$N"); do
    peers="$(run "$i" 'airc peers 2>&1')"
    for j in $(seq 1 "$N"); do
      [ "$i" = "$j" ] && continue
      { [ -n "${ID[$j]:-}" ] && echo "$peers" | grep -q "${ID[$j]}"; } || ok=0
    done
  done
  echo "  round $round: all-see-all=$ok"
  [ "$ok" = 1 ] && { converged=1; break; }
done

echo "== final visibility matrix =="
for i in $(seq 1 "$N"); do
  peers="$(run "$i" 'airc peers 2>&1')"
  for j in $(seq 1 "$N"); do
    [ "$i" = "$j" ] && continue
    if [ -n "${ID[$j]:-}" ] && echo "$peers" | grep -q "${ID[$j]}"; then
      echo "  node-$i SEES node-$j (${ID[$j]:0:8})"
    else
      echo "  node-$i does NOT see node-$j (${ID[$j]:0:8})  <-- orphan"
    fi
  done
done

echo ""
if [ "$converged" = 1 ]; then
  echo "RESULT: CONVERGED — every node sees every other node's identity within $ROUNDS sync rounds."
  exit 0
else
  echo "RESULT: NOT CONVERGED after $ROUNDS rounds — a real orphan, not a passing test that lies."
  exit 1
fi
