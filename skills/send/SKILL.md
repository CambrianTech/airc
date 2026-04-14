---
name: relay:send
description: Send a message to a peer via Agent Relay.
user-invocable: true
allowed-tools: Bash
argument-hint: "<peer> <message>"
---

# Send a Relay Message

Run this yourself — don't ask the user to do it.

If `relay` is not on PATH, install it first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

Project-scoped state: always prefix with `AGENT_RELAY_HOME=$PWD/.agent-relay` so you target the identity and peer list established via `/relay:connect`. If `AGENT_RELAY_HOME` is already exported, respect it.

Parse the first word of `$ARGUMENTS` as the peer name, the rest as the message:

```bash
AGENT_RELAY_HOME=$PWD/.agent-relay relay send $ARGUMENTS
```

If no arguments provided, ask the user who to message and what to say.

Note: `relay connect` must be running in a Monitor (it handles the inbound stream). If not connected, tell the user to run `/relay:connect` first.
