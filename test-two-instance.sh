#!/bin/bash
# Two-instance round-trip test for agent-relay patches.
#
# Prereqs: port 7547 must be FREE (stop any running relay connect).
# Run in tmux with two panes — host in one, joiner + verify in the other.
#
# Usage:
#   ./test-two-instance.sh host      # run in pane 1 (blocks, keep open)
#   ./test-two-instance.sh join      # run in pane 2 (paste join string when asked)
#   ./test-two-instance.sh verify    # run in pane 2 after join succeeds

set -e
RELAY=~/Development/cambrian/agent-relay/relay
MODE="${1:-help}"

case "$MODE" in
  host)
    rm -rf /tmp/relay-test-host
    cd /tmp && mkdir -p relay-test-host && cd relay-test-host
    export AGENT_RELAY_HOME="$PWD/.agent-relay"
    export AGENT_RELAY_NAME="host-test"
    echo "[host] starting with name=host-test, home=$AGENT_RELAY_HOME"
    exec "$RELAY" connect
    ;;

  join)
    if [ -z "$2" ]; then
      echo "Usage: $0 join '<full-join-string-from-host-pane>'"
      exit 1
    fi
    rm -rf /tmp/relay-test-join
    cd /tmp && mkdir -p relay-test-join && cd relay-test-join
    export AGENT_RELAY_HOME="$PWD/.agent-relay"
    export AGENT_RELAY_NAME="join-test"
    echo "[join] pairing as name=join-test with $2"
    # run connect in background to capture pairing, then we can send
    "$RELAY" connect "$2" &
    echo "[join] connect PID $!, waiting 3s..."
    sleep 3
    echo "[join] peer list:"
    "$RELAY" peers
    ;;

  verify)
    cd /tmp/relay-test-join
    export AGENT_RELAY_HOME="$PWD/.agent-relay"
    echo "[verify] 1. config on join side:"
    cat .agent-relay/config.json
    echo ""
    echo "[verify] 2. peer record (should contain host_relay_home):"
    ls .agent-relay/peers/ && cat .agent-relay/peers/*.json
    echo ""
    echo "[verify] 3. sending test message to host-test:"
    "$RELAY" send host-test "round-trip test from join-test" && echo "  sent"
    sleep 1
    echo ""
    echo "[verify] 4. rename test:"
    "$RELAY" rename renamed-joiner
    echo ""
    echo "[verify] 5. after rename, config:"
    cat .agent-relay/config.json
    echo ""
    echo "Check host pane: it should show (a) 'Peer joined: join-test' (b) the test message (c) a [rename] notice."
    ;;

  cleanup)
    pkill -f "relay-test-host" 2>/dev/null || true
    pkill -f "relay-test-join" 2>/dev/null || true
    rm -rf /tmp/relay-test-host /tmp/relay-test-join
    echo "cleaned up"
    ;;

  *)
    cat <<EOF
Two-instance round-trip test for agent-relay

Pane 1: $0 host
Pane 2: $0 join '<paste-the-join-string-the-host-printed>'
Pane 2: $0 verify

Pass criteria:
  - Join side's peer record contains 'relay_home': '/tmp/relay-test-host/.agent-relay'
  - Test message reaches host side (shows in its stream)
  - Rename updates config, broadcasts to host (host sees 'Peer renamed: join-test -> renamed-joiner')

After testing: $0 cleanup
EOF
    ;;
esac
