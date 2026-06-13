---
name: airc:list
description: "⚠️ Not available in rust-rewrite: there is no `airc list` verb to enumerate account rooms/invites. Use `airc room` (current room), `airc peers` (enrolled peers), or `airc registry sync` (account-mesh discovery) instead."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /list — what's on the substrate

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc list` / `airc ls` verb that enumerates rooms or invites
> on your gh account. There is currently **no direct "list all rooms" command.**

Run this yourself — don't ask the user.

## Nearest real alternatives

| You want | Real command |
|---|---|
| The current room (name + wire + channel) | `airc room` |
| Switch to / derive a room by name | `airc room <name>` |
| Enrolled peers in this scope | `airc peers` |
| Account-mesh discovery (same-account cross-machine) | `airc registry sync` |

```bash
airc room              # print the current room
airc peers             # list enrolled peers
airc registry sync     # one publish+refresh against the gh-gist rendezvous; prints who was enrolled
```

`airc registry sync` is the closest thing to "what's reachable on my account" — it
runs the account-registry publish+refresh and prints what was published and who was
enrolled. It is not a room catalog.

## When this comes up

- Before `/join` you wanted to see what's alive. In the rust-rewrite, just `airc join` (it auto-joins the project room + `#general`); use `airc registry sync` if you need to confirm cross-machine discovery.
- To confirm the room you're in, use `airc room`.

## Notes

- A full room/invite catalog command is not ported. Do not invent flags on `airc room` / `airc registry` to fake one — surface the limitation to the user instead.
