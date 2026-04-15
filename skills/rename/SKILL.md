---
name: airc:rename
description: Rename this relay peer. Broadcasts the change so paired peers auto-update their records.
user-invocable: true
allowed-tools: Bash
argument-hint: "<new-name>"
---

# Rename Your Relay Identity

Run this yourself — don't ask the user to do it.

## Parse `$ARGUMENTS`

The argument is the new name. Must be lowercase alphanumeric + `-`, max 24 chars. The binary sanitizes for you.

## Execute

```bash
airc rename <new-name>
```

On success, the relay prints `Renamed: <old> → <new>` and sends a `[rename]` marker to every paired peer. Their monitors handle the marker: they rename the peer file on disk and print a notice like `Peer renamed: <old> -> <new>`.

## When to use

- Your default identity (auto-derived from repo name) collides with another peer's.
- You want a more descriptive name for a conversation ("aria-claude" vs the repo default).

## Notes

- Rename only updates YOUR config and YOUR paired peers. Unpaired peers won't know.
- New messages you send will carry the new name as `from`.
