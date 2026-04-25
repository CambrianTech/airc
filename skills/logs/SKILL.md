---
name: logs
description: Show the last N messages in the mesh's shared log (default 20). Human-readable format — timestamp + sender + message.
user-invocable: true
allowed-tools: Bash
argument-hint: "[N]"
---

# airc logs

Run this yourself — don't ask the user.

## Execute

```bash
airc logs         # last 20
airc logs 50      # last 50
```

Prints one line per message: `[ts] from: msg`. Tails the host's shared `messages.jsonl` (for joiners, via SSH; for hosts, locally).

## When to use

- Catching up after monitor downtime / teardown gap.
- Confirming a message you sent actually landed on the wire.
- Triaging "did I miss something?" when chat feels quiet.

## Notes

- Output is read-only history. For live events, use `/join` (which wraps `airc join` under Monitor so inbound surfaces as interrupts).
- Log reflects what the HOST saw, not just your local mirror. Canonical for the mesh.
