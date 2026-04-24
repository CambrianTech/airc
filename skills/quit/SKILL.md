---
name: airc:quit
description: Leave the mesh entirely. Kills the airc process and clears host-pairing so next /join is a fresh pair, not a resume. Identity preserved. IRC-canonical name; same dispatch as /disconnect.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /quit — IRC `/quit`, airc-style

Run this yourself.

## Execute

```bash
airc quit
```

Same as `airc disconnect`. Kills the running airc process in this scope, strips the host-pairing fields from `config.json`, but preserves your identity name, keys, peers, and message log.

Next `airc join` (or `/join`) starts fresh instead of auto-resuming the old pairing.

## Why two skill names

`/disconnect` (airc-classic) and `/quit` (IRC-canonical) dispatch to the same `cmd_disconnect`. Both kept for muscle-memory continuity. See [skills/disconnect/SKILL.md](../disconnect/SKILL.md) for the difference between `quit`, `teardown`, and `teardown --flush`.
