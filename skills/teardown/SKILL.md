---
name: airc:teardown
description: Kill all local airc processes and free the ports they're holding. Use --flush to also wipe state (identity, peers, messages).
user-invocable: true
allowed-tools: Bash
argument-hint: "[--flush]"
---

# airc teardown

Run this yourself — don't ask the user. It's idempotent.

## When to use

- A previous `airc connect` left a zombie holding port 7547/7548
- You're switching projects and want a clean slate
- Tests left processes around (`airc doctor` ran but didn't clean up)
- "Pair refused" / "Failed to deliver" errors that don't make sense
- Before `airc connect` with a new identity, to avoid pairing with your own stale listener

## What it does

```bash
airc teardown
```

Kills every `airc connect` / `airc monitor` process owned by this user, plus any Python listener child-processes still binding 7547 or 7548. Preserves state — identity keys, peer records, and message log stay on disk so you can `airc connect` again and resume.

```bash
airc teardown --flush
```

Also wipes the current state dir (`$AIRC_HOME` or `$PWD/.airc` or `$HOME/.airc`, depending on which tier is active). Nuclear option. Next `airc connect` will generate a fresh identity and start pairing from scratch.

## Read the result

- `No airc processes running.` — nothing to do, you were already clean
- `Teardown complete.` — killed something or freed ports; you're clean now

## Aliases

`airc stop` and `airc flush` dispatch to the same command. Use whichever feels natural.
