---
name: airc:part
description: Leave the current room without leaving the mesh. Host parts → room gist deleted, joiners reconnect into a new election. Joiner parts → host's gist stays open for others. Local identity preserved.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /part — Leave the current airc room

Run this yourself.

## Execute

```bash
airc part
```

Two distinct behaviors based on whether you're the host or a joiner of the current room:

- **Host parts:** the room gist is deleted from your gh namespace (so nobody else can re-resolve the mnemonic), then local processes shut down. Joiners watching the gist see SSH die — IRC's "ircd restart" — and the next reconnect re-elects a new host from whoever's still around.
- **Joiner parts:** local processes shut down. The host's gist stays published for other joiners; you're just one of N leaving.

In both cases your local config, identity keys, and peer records persist. Next `/join` reconnects (or, in dynamic-host mode, becomes the new host if nobody else is there).

## When to use

- You want to leave a specific room cleanly without nuking your airc identity.
- You're the host and want to gracefully hand off the room (joiners will re-elect a new host on reconnect).
- Differs from `/quit`, which leaves the entire mesh + clears host-pairing for a fresh re-pair.
