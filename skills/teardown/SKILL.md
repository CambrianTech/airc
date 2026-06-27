---
name: airc:teardown
description: Stop this scope's running airc daemon gracefully via `airc stop`. Scope-aware — never touches other scopes' daemons. State-wipe is not a CLI verb in the rust-rewrite.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc teardown — stop the daemon

Run this yourself — don't ask the user. It's idempotent and scope-safe.

In the rust-rewrite there is no `airc teardown` verb. Graceful daemon shutdown for
the current scope is `airc stop`.

## Execute

```bash
airc stop
```

Asks the daemon for the current scope (its `--home` / `$AIRC_HOME`) to shut down
gracefully. Only the daemon owning this scope's IPC socket is stopped — daemons in
other scopes are untouched.

State (identity keys, peer records, subscriptions, event log) is preserved on disk.
The next `airc join` re-attaches the same mesh.

## When to use

- A previous `airc join` left a daemon you want to bounce (e.g. to pick up a new airc binary).
- You're switching projects and want this scope's daemon stopped.
- Before re-arming a fresh `airc join` Monitor after `airc update`.

## State-wipe (the old `--flush`)

> ⚠️ The old `airc teardown --flush` (nuke identity + peers + messages) has **no CLI
> verb in the rust-rewrite**. `airc stop` stops the daemon only; it never wipes state.
> If you genuinely need a from-scratch identity, that is a manual reset of the scope's
> `$AIRC_HOME` directory, not a supported `airc` subcommand. For recovery from a
> corrupt mesh, prefer the `/repair` skill (`airc stop` then `airc join`) before
> reaching for a manual wipe.

## Read the result

- Daemon was running → it shuts down and `airc stop` returns.
- No daemon for this scope → `airc stop` is a no-op; you were already stopped.

## Scope-awareness

`airc stop` targets only the daemon bound to this scope's IPC socket (derived from
`--home` / `$AIRC_HOME`). A daemon another tab is running in a different scope is not
affected.
