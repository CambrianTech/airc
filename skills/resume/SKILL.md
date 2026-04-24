---
name: airc:resume
description: Resume a prior airc session in this scope. Alias for `airc join` with no args — picks up the saved pairing and restarts the monitor without re-pasting the join string.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: ""
---

# airc resume

Run this yourself — don't ask the user.

## Execute

```
Monitor(persistent=true, command="airc join")
```

Wrap with the Monitor tool so inbound streams as Claude Code notifications. `airc join` with no args detects the stored pairing in this scope's config.json and restarts the monitor — no fresh handshake, no join string, no env vars.

## When to use

- User re-opens a Claude Code tab in a project dir that has a prior `.airc/` scope.
- Monitor died or you need to bounce it after pulling new airc code.
- Any time the state is on disk but no airc process is running.

## Failure modes

- `Not initialized (<scope>). Run: airc join` — scope is fresh (no saved pairing). The user needs an actual join string from the host; use `/join <string>` instead.
- `Resume aborted — re-pair required` — saved SSH key no longer authenticates against the host (reinstall regenerated keys, host rotated authorized_keys, etc.). The error output prints the exact repair command + reconstructs the saved invite string so the user doesn't have to hunt for it. Follow it verbatim: `airc teardown --flush && airc join <invite-string>`.
- Silent resume (monitor running but no inbound ever arrives): used to be a silent failure mode pre-fix. Now the auth probe catches it at connect time. If you somehow still see this, the host genuinely is unreachable — check `airc status --probe` to confirm.

## Notes

- `airc join` (no args) and `airc resume` are the same command — `resume` is just a mnemonic alias.
- Skills `/join` and `/resume` both resolve to the same `airc join` invocation; which one to use is a matter of user-facing intent ("I'm starting" vs "I'm coming back").
