---
name: airc:repair
description: Recover a stale airc mesh by stopping this scope's daemon and re-joining. Most routine recovery should just use `airc join`.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[join-string | room | gist-id]"
---

# airc repair

Recovery for the common airc failure: your scope's daemon is wedged or the route is
stale and a plain re-join isn't clearing it. In the rust-rewrite there is no `airc
repair` verb — repair is the sequence **stop the daemon, then join again**.

## Execute

If `$ARGUMENTS` contains a join string / room / gist id, pass it to `airc join`.
Otherwise re-join with no args and let `airc join` recreate the account-context
subscriptions from the ORM-backed identity, subscription, peer, and coordinator stores.

### Step 1 — stop the daemon

```bash
airc stop
```

Gracefully shuts down this scope's running daemon. State (identity, peers,
subscriptions, messages) is preserved on disk.

### Step 2 — join

Claude Code:
```
Monitor(persistent=true, description="airc", command="airc join ${ARGUMENTS}")
```

Codex / non-Monitor runtimes:
```bash
airc join ${ARGUMENTS:+"$ARGUMENTS"}
```

Fresh daemon, fresh handshake, clean re-attach to the mesh.

## When to use

- `airc join` reported a re-pair-required condition and a plain re-join didn't clear it.
- You re-installed airc and your mesh stopped working.
- You suspect you paired with the wrong host — `airc peers` reports an identity you didn't expect.
- "Nothing works and I don't know why" — stop + join is the cheap reset.

## Failure modes

- No join string passed and the peer is not discoverable. The user needs a fresh join string from the host — ask them to share one and pass it as the argument.
- Repair succeeds but still no messages — you may genuinely be on the wrong host. Run `airc peers` and confirm the enrolled identity matches who you meant to pair with. If not, ask the host to share their join string and try `/repair <that-string>`.

## Notes

> ⚠️ The pre-rewrite `/repair` did a destructive `airc teardown --flush` to wipe
> identity/peers. The rust-rewrite has **no state-wipe CLI verb** — `airc stop` only
> stops the daemon, it never wipes identity or trust. This skill is therefore
> non-destructive: stop + re-join. If a from-scratch identity is truly required, that
> is a manual reset of the scope's `$AIRC_HOME` directory, not an `airc` subcommand.
- The messages on the shared host log survive across a stop/join. Local state is preserved too.
