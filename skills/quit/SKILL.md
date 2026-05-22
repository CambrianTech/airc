---
name: airc:quit
description: Leave the current mesh without wiping your identity. Kills the running airc process in this scope and clears pairing state. Next `airc join` starts fresh instead of auto-resuming.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /quit — Leave the airc mesh

Run this yourself — don't ask the user.

## Execute

```bash
airc quit
```

Does two things:
1. `airc teardown` — kills the running airc process in this scope (same as normal teardown).
2. Clears pairing state from the store. Your identity name, keys, peers, and event history are preserved.

Prints: `Disconnected. Identity preserved. Next 'airc join' starts fresh (not a resume).`

## When to use

- You want to switch to a different mesh / host and don't want `airc join` (no args) to auto-resume the old pairing.
- You paired to a stale host, the join string rotated, and you want a clean slate without losing your identity.
- You want to become the host of a new mesh in the same scope.

## Differences from related commands

- `/teardown` — kills process, preserves all state including the host-pairing. Next join resumes.
- `/quit` — kills process, clears host-pairing only. Next join goes fresh.
- `/teardown --flush` — nuclear: kills process, wipes identity, peers, and event history. Fresh pair from scratch.
- `/part` — leave the current room without leaving the mesh entirely (host: deletes room gist; joiner: just teardown).
