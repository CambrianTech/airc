---
name: airc:teardown
description: Stop this scope's owning account daemon via `airc stop` and retain operator intent until explicit resume. State is preserved.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc teardown — stop the daemon

Run this yourself when asked to stop AIRC. It's idempotent and preserves state.

In the rust-rewrite there is no `airc teardown` verb. Graceful daemon shutdown for
the current scope is `airc stop`.

## Execute

```bash
airc stop
```

Stops the account daemon owning the current scope (`--home` / `$AIRC_HOME`).
Scopes sharing that account share the same daemon and stop intent. Other accounts
are untouched; a `--socket` belonging to a different owner is refused.

State (identity keys, peer records, subscriptions, event log) is preserved on disk.
The stop intent survives command exit and machine restart. Ordinary commands,
login supervisors, and `airc join --ensure` cannot clear it. An explicit
`airc join` resumes the same mesh; explicit `airc update --adopt-installed` also
resumes service. Ordinary `airc update` preserves stopped state.

## When to use

- A previous `airc join` left a daemon you want to bounce (e.g. to pick up a new airc binary).
- You want the account daemon stopped, including its subscribed project scopes.
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
- No daemon for this account → records the same durable stop intent successfully.

## Scope-awareness

`airc stop` targets this scope's canonical account endpoint, derived from
`--home` / `$AIRC_HOME`. It does not delete identities, subscriptions, or history.
