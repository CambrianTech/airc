---
name: relay:monitor
description: Start monitoring for incoming relay messages from paired machines.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[peer-name-filter]"
---

# Start Relay Monitor

Start a persistent background monitor for incoming relay messages.

Use the Monitor tool:

```
Monitor(persistent=true, command="relay monitor $ARGUMENTS")
```

This runs until the session ends. Each incoming message appears as an inline notification.
If `relay` is not found, tell the user to run: `curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash`
