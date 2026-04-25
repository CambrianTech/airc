---
name: peers
description: List paired peers in this scope. Add --prune to clean stale records (same-host duplicates left over from rename chain-breaks before stable-host matching landed).
user-invocable: true
allowed-tools: Bash
argument-hint: "[--prune]"
---

# airc peers

Run this yourself — don't ask the user.

## Execute

```bash
airc peers
```

Prints one line per paired peer: `<name> → <ssh-target>`.

```bash
airc peers --prune
```

Removes stale peer records whose `host` matches a newer record (cruft from rename chain-breaks before stable-host identity landed). Prints `pruned: <name> -> <host>` per removal, or `No stale records to prune.` if clean.

## When to use

- Before sending — confirm the peer name you want is actually paired.
- After a host rename burst or multi-reconnect session, to spot duplicate records (same host, different names).
- Debugging "who am I actually talking to?"

## Notes

- `--prune` is safe: it only removes records whose host matches another record's host (same machine+user, different name). Keeps the most-recently-paired one.
- Peer records are pulled fresh from disk each call; no caching.
