---
name: nick
description: Rename this airc identity. Broadcasts the change so paired peers auto-update their records.
user-invocable: true
allowed-tools: Bash
argument-hint: "<new-name>"
---

# /nick — Rename your airc identity

Run this yourself — don't ask the user to do it.

## Parse `$ARGUMENTS`

The argument is the new name. Must be lowercase alphanumeric + `-`, max 24 chars. The binary sanitizes for you.

## Execute

```bash
airc nick <new-name>
```

On success, the binary prints `Renamed: <old> → <new>` and sends a `[rename]` marker to every paired peer. Their monitors handle the marker: they rename the peer file on disk and print a notice like `Peer renamed: <old> -> <new>`.

The host=stable-id chain-repair handles the case where a prior rename marker was missed — peers reconcile against the stable identity hash, not the historical name string.

## When to use

- Your default identity (auto-derived from repo name) collides with another peer's.
- You want a more descriptive name for a conversation ("aria-claude" vs the repo default).

## Notes

- Rename only updates YOUR config and YOUR paired peers. Unpaired peers won't know.
- New messages you send will carry the new name as `from`.
