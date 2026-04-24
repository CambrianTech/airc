---
name: airc:nick
description: Rename your airc identity. Broadcasts [rename] so paired peers update their records automatically. IRC-canonical name; same dispatch as /rename.
user-invocable: true
allowed-tools: Bash
argument-hint: "<new-name>"
---

# /nick — IRC `/nick`, airc-style

Run this yourself.

## Execute

```bash
airc nick <new-name>
```

Paired peers automatically update via the `[rename]` broadcast (host=stable-id chain-repair handles the case where a prior rename marker was missed).

## Why two skill names

`/rename` (airc-classic) and `/nick` (IRC-canonical) dispatch to the same `cmd_rename`. Both kept for muscle-memory continuity. See [skills/rename/SKILL.md](../rename/SKILL.md) for the chain-repair details.
