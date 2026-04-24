---
name: airc:disconnect
description: Leave the current mesh without wiping your identity. Kills the running airc process in this scope and clears only the host-pairing fields from config. Next `airc join` starts fresh instead of auto-resuming.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc disconnect

Run this yourself — don't ask the user.

## Execute

```bash
airc disconnect
```

Does two things:
1. `airc teardown` — kills the running airc process in this scope (same as normal teardown).
2. Strips `host_target`, `host_name`, `host_port`, `host_ssh_pub`, `host_airc_home` from `config.json`. Your identity name, keys, peers, and message log are all preserved.

Prints: `Disconnected. Identity preserved. Next 'airc connect' starts fresh (not a resume).`

## When to use

- You want to switch to a different mesh / host and don't want `airc join` (no args) to auto-resume the old pairing.
- You paired to a stale host, the join string rotated, and you want a clean slate without losing your identity.
- You want to become the host of a new mesh in the same scope.

## Differences from related commands

- `airc teardown` — kills process, preserves all state including the host-pairing. Next connect resumes.
- `airc disconnect` — kills process, clears host-pairing only. Next connect goes fresh.
- `airc teardown --flush` — nuclear: kills process, wipes identity, peers, messages, config. Fresh pair from scratch.

## Aliases

`airc leave`, `airc unbind` both work.
