---
name: airc:resume
description: Resume a prior airc session in this scope. Alias for `airc connect` with no args — picks up the saved pairing and restarts the monitor without re-pasting the join string.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: ""
---

# airc resume

Run this yourself — don't ask the user.

## Execute

```
Monitor(persistent=true, command="airc connect")
```

Wrap with the Monitor tool so inbound streams as Claude Code notifications. `airc connect` with no args detects the stored pairing in this scope's config.json and restarts the monitor — no fresh handshake, no join string, no env vars.

## When to use

- User re-opens a Claude Code tab in a project dir that has a prior `.airc/` scope.
- Monitor died or you need to bounce it after pulling new airc code.
- Any time the state is on disk but no airc process is running.

## Failure modes

- `Not initialized (<scope>). Run: airc connect` — scope is fresh (no saved pairing). The user needs an actual join string from the host; use `/airc:connect <join>` instead.

## Notes

- `airc connect` (no args) and `airc resume` are the same command — `resume` is just a mnemonic alias.
- Skills `/airc:connect` and `/airc:resume` both resolve to the same `airc connect` invocation; which one to use is a matter of user-facing intent ("I'm starting" vs "I'm coming back").
