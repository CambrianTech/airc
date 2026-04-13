---
name: relay:uninstall
description: Remove Agent Relay — unlinks skills, removes binary, cleans up.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# Uninstall Agent Relay

```bash
~/.agent-relay-src/uninstall.sh
```

Ask the user if they also want to remove the clone directory and relay data:
- `rm -rf ~/.agent-relay-src` — removes the source
- `rm -rf ~/.agent-relay` — removes keys, peers, and message history

Confirm before deleting data.
