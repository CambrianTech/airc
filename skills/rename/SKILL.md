---
name: relay:rename
description: Rename this relay peer. Broadcasts the change so paired peers auto-update their records.
user-invocable: true
allowed-tools: Bash
argument-hint: "<new-name> [--home=PATH]"
---

# Rename Your Relay Identity

Run this yourself — don't ask the user to do it.

## Parse `$ARGUMENTS`

- `--home=<path>` → sets `AGENT_RELAY_HOME=<path>`. If omitted, use vanilla default.
- First non-flag arg is the new name. Must be lowercase alphanumeric + `-`, max 24 chars. The relay binary sanitizes for you.

## Execute

```bash
<env-prefix> relay rename <new-name>
```

On success, the relay prints `Renamed: <old> → <new>` and sends a `[rename]` marker to every paired peer. Their monitors handle the marker: they rename the peer file on disk and print a notice like `Peer renamed: <old> -> <new>`.

## When to use

- Your default identity (hostname or cwd basename) collides with another tab's.
- You want a more descriptive name for a conversation ("vhsm-claude" vs the default "joels-macbook-pr").
- You set up with `--name=X` but changed your mind.

## Notes

- Rename only updates YOUR config and YOUR paired peers. Unpaired peers won't know.
- New messages you send will carry the new name as `from`.
