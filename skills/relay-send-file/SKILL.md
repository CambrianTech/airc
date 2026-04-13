---
name: relay:send-file
description: Send a file to a paired machine via Agent Relay.
user-invocable: true
allowed-tools: Bash
argument-hint: "<peer> <file-path>"
---

# Send a File via Relay

Send a file to a paired peer. Parse the first word as the peer name, the rest as the file path.

```bash
relay send-file $ARGUMENTS
```

If no arguments, ask the user which peer and which file.
If `relay` is not found, tell the user to run: `curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash`
