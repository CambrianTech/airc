---
name: airc:msg
description: Send a message in airc. Bare text broadcasts to current room; @peer prefix targets a DM. IRC-canonical name; same dispatch as /send.
user-invocable: true
allowed-tools: Bash
argument-hint: "[@peer] <message>"
---

# /msg — IRC `/msg`, airc-style

Run this yourself.

## Execute

**Broadcast to current room:**
```bash
airc msg "hello everyone"
```

**DM a peer:**
```bash
airc msg @alice "quick question about the substrate"
```

## Why two skill names

`/send` (airc-classic) and `/msg` (IRC-canonical) dispatch to the same `cmd_send`. Both are kept so users with muscle memory from either lineage can type either. See [skills/send/SKILL.md](../send/SKILL.md) for the full protocol notes (mirror-first, queue-on-network-fail, die-on-auth-fail) and the failure-mode triage table.
