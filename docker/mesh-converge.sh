#!/usr/bin/env bash
# mesh-converge.sh — the convergence smoke: N airc nodes, separate
# ~/.airc each (real isolated "machines"), one shared gh token (one
# account), on a shared bridge network. Asserts they converge — discover
# each other as peers on the same account mesh — or surfaces the orphan.
#
# This is the contract's "two isolated machine homes converge" acceptance
# test, run with REAL isolation (containers) instead of faked HOME dirs,
# and it runs in Linux containers so Windows Smart App Control can't block
# the test loop. Usage: docker/mesh-converge.sh [N]
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
    "$IMAGE" sleep infinity >/dev/null
  echo "  node-$i up"
done

run() { docker exec "airc-node-$1" sh -lc "$2" 2>&1; }

echo "== each node: airc join (spawns daemon, joins account mesh) =="
for i in $(seq 1 "$N"); do
  out="$(run "$i" 'airc join 2>&1 | head -4')"
  echo "  node-$i join: $(echo "$out" | tr '\n' ' ' | cut -c1-120)"
done

echo "== each node: registry sync (force publish + discover) =="
for i in $(seq 1 "$N"); do
  out="$(run "$i" 'airc registry sync 2>&1 | tail -1')"
  echo "  node-$i sync: $out"
done

echo "== convergence check: does each node SEE the others as peers? =="
converged=1
for i in $(seq 1 "$N"); do
  peers="$(run "$i" 'airc peers 2>&1')"
  count="$(echo "$peers" | grep -cE 'tier=|peer_id|[0-9a-f]{8}-' || true)"
  echo "  node-$i sees ~$count peer line(s)"
  # Expect to see at least N-1 OTHER nodes.
  [ "$count" -lt $((N - 1)) ] && converged=0
done

echo ""
if [ "$converged" = 1 ]; then
  echo "RESULT: CONVERGED — $N isolated nodes on one account see each other."
else
  echo "RESULT: ORPHANED — nodes did not all discover each other (the gap to fix)."
fi
