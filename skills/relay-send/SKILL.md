---
name: relay:send
description: Send a message to a paired machine via Agent Relay.
user-invocable: true
allowed-tools: Bash
argument-hint: "<peer> <message>"
---

# Send a Relay Message

Send `$ARGUMENTS` via the relay. Parse the first word as the peer name, the rest as the message.

```bash
relay send $ARGUMENTS
```

If no arguments provided, ask the user who to message and what to say.
If `relay` is not found, tell the user to run: `curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash`
