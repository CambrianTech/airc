---
name: relay:send
description: Send a message to a peer via Agent Relay.
user-invocable: true
allowed-tools: Bash
argument-hint: "[<peer>] <message> [--home=PATH]"
---

# Send a Relay Message

Run this yourself — don't ask the user to do it.

If `relay` is not on PATH, install it first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

## Parse `$ARGUMENTS`

- `--home=<path>` → sets `AGENT_RELAY_HOME=<path>`. If omitted, use vanilla default.
- Remaining args, in order: `<peer> <message>`. If the first arg matches a known peer name (check via `relay peers`), treat it as the peer; everything else is the message. If the first arg isn't a known peer and there's only one peer paired, treat all args as the message and auto-address the sole peer.

## Send

```bash
<env-prefix> relay send <peer> <message>
```

If no peer is specified and more than one is paired, ask the user who to message.

## Notes

- `/relay:connect` must be running in a Monitor somewhere so inbound stream gets surfaced. If not connected, tell the user to run `/relay:connect` first.
- Messages sent to `<peer>` land in that peer's message log over SSH. The peer's monitor surfaces them as notifications. No other routing needed.
