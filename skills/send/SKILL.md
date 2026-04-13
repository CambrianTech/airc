---
name: relay:send
description: Send a message to a paired machine via Agent Relay.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "<peer> <message>"
---

# Send a Relay Message

Run this yourself — don't ask the user to do it.

If `relay` is not on PATH, install it first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

Parse the first word of `$ARGUMENTS` as the peer name, the rest as the message:

```bash
relay send $ARGUMENTS
```

After sending, start the monitor if it isn't already running so you can receive replies:
```
Monitor(persistent=true, command="relay monitor")
```

If no arguments provided, ask the user who to message and what to say.
