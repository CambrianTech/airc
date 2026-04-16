---
name: airc:teardown
description: Kill airc processes belonging to THIS scope (this AIRC_HOME), free its port. Scope-aware — never touches other tabs' sessions. Add --flush to also wipe state.
user-invocable: true
allowed-tools: Bash
argument-hint: "[--flush]"
---

# airc teardown

Run this yourself — don't ask the user. It's idempotent and scope-safe.

## When to use

- A previous `airc connect` left a zombie holding this scope's port
- You're switching projects and want a clean slate
- "Pair refused" / "Failed to deliver" errors that don't make sense — nuke and re-pair
- Before `airc connect` with a new identity, to avoid pairing with your own stale listener

## `--flush` vs plain teardown — when to use which

**Plain `airc teardown`**: kills processes, keeps state. Use when you know the pairing is still valid and you just want to stop/restart the monitor (e.g. to pick up a new airc binary).

**`airc teardown --flush`**: kills processes AND wipes identity, peer records, saved pairing, messages. Use in ANY of these cases:
- `airc connect` (resume) died with "Resume aborted — re-pair required" (stale SSH key)
- `airc send` died with auth error pointing at "re-pair required"
- You just reinstalled airc and your identity keys may no longer be authorized on the host
- You're not sure what's broken but you definitely can't reach your peers anymore
- You're changing which host you pair with

**Rule of thumb**: if anything about your pairing feels uncertain, use `--flush`. The nuclear option is cheap — you pair again via the invite string and keep going. The half-measure (plain teardown with stale state, then resume) has burned hours in production by silently reconnecting to a broken pairing.

## What it does

```bash
airc teardown
```

**Scope-aware.** Reads `$AIRC_WRITE_DIR/airc.pid` (written by `airc connect` at startup), kills ONLY those PIDs plus their direct descendants (python listeners). Then checks the scope's port and reaps any now-orphaned listener parented to init. Will NOT touch other tabs' sessions running under different `AIRC_HOME` values.

State is preserved: identity keys, peer records, message log all stay on disk. Next `airc connect` resumes.

```bash
airc teardown --flush
```

Additionally wipes `$AIRC_WRITE_DIR` (identity, peers, messages, config — everything). Nuclear option. Next `airc connect` generates a fresh identity and pairs from scratch.

## Read the result

- `No airc processes running.` — nothing to do, you were already clean.
- `killing scope <dir>: <pids>` then `Teardown complete.` — killed your scope's processes and any orphaned listener on your port.

## Scope-awareness guarantee

If another Claude tab is running `airc connect` in a different `AIRC_HOME` (even on a different port), this command will NOT touch it. The guarantee is tested by `airc doctor` — the `teardown` scenario spawns two hosts in different scopes and asserts a teardown from scope A doesn't kill scope B.

## Aliases

`airc stop` and `airc flush` dispatch to the same command.
