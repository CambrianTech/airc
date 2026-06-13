---
name: airc:kick
description: "⚠️ Not available in rust-rewrite: there is no `airc kick` verb. The nearest real action is `airc peer remove` (drop a peer from local trust), but that does not evict them from a shared host."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /kick — Remove a paired peer (host only)

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc kick` verb in the rust-rewrite.

Run this yourself — don't ask the user.

## Nearest real alternative

`airc peer remove` drops a peer from **this scope's** local trust store. It does not
manage `authorized_keys` or evict the peer from a shared host the way the old
host-only `/kick` did — it only forgets the peer locally.

```bash
airc peer remove <peer-id>     # remove from local trust store
airc peers                     # confirm who is still enrolled
```

## When this comes up

- A peer's behavior is wrong and you want them gone. In the rust-rewrite you can forget them locally with `airc peer remove`, but full host-side eviction (key revocation, ban) is not a CLI verb yet — surface that to the user.
- For leaving a room yourself, use `airc part` (the `/part` skill).

## Notes

- `airc peer add` / `airc peer remove` / `airc peer set-tier` / `airc peer list` are the real peer-trust verbs; `airc peers` is the IRC-shaped read.
- Do not invent `airc kick` — it does not exist in the rust-rewrite.
