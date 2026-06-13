---
name: airc:resume
description: Resume a prior airc session in this scope. Alias for `airc join` with no args. Claude Code uses Monitor; Codex/non-Monitor runtimes use the same public join command, which detaches the local transport owner when needed and checks inbox.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: ""
---

# airc resume

Run this yourself — don't ask the user.

## Execute

Claude Code:
```
Monitor(persistent=true, description="airc", command="airc join")
```

Codex / non-Monitor runtimes:
```bash
airc join
```

`airc join` with no args opens this scope's ORM-backed identity/subscription state and restarts the airc process — no fresh handshake, no join string, no env vars.

## When to use

- User re-opens a Claude Code tab in a project dir that has a prior `.airc/` scope.
- Monitor died or you need to bounce it after pulling new airc code.
- Any time the state is on disk but no airc process is running.

## Failure modes

- `Not initialized (<scope>). Run: airc join` — scope is fresh (no saved pairing). The user needs an actual join string from the host; use `/join <string>` instead.
- `Resume aborted — re-pair required` — saved pairing no longer authenticates against the host (reinstall regenerated keys, host rotated authorized_keys, etc.). Recover with `airc stop && airc join <join-string>` (the `/repair` skill wraps this).
- Silent resume (airc daemon running but no inbound ever arrives): if you still see this, the host genuinely is unreachable — check `airc status` to confirm the local daemon and route are healthy.

## Notes

- There is no `airc resume` CLI verb. This `/resume` skill is purely a mnemonic that wraps `airc join` with no args.
- Skills `/join` and `/resume` both resolve to the same `airc join` invocation; which one to use is a matter of user-facing intent ("I'm starting" vs "I'm coming back").
