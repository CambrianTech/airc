---
name: airc:join
description: Join the airc mesh. Default = auto-#general on the user's gh account (host if nobody's there yet). Optional arg = mnemonic, gist id, room name, or inline invite.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[mnemonic | gist-id | room-name | invite-string]"
---

# /join — IRC `/join`, airc-style

Run this yourself. Same as the airc-classic `/connect` skill — the IRC verb is just the canonical name. Both work.

## Execute

**Default — auto-#general:**
```
Monitor(persistent=true, command="airc join")
```

Same gh account on multiple tabs/machines = automatic mesh, zero strings passed.

**Join a specific room:**
```
Monitor(persistent=true, command="airc join --room project-x")
```

**Join via mnemonic (cross-account, friend dictated 4-word phrase):**
```
Monitor(persistent=true, command="airc join oregon-uncle-bravo-eleven")
```

**Join via gist id (cross-account fallback when mnemonic resolution can't reach):**
```
Monitor(persistent=true, command="airc join <gist-id>")
```

## Why two skill names

`/connect` (airc-classic) and `/join` (IRC-canonical) dispatch to the same `cmd_connect` in the airc binary. Both are kept so users with muscle memory from either lineage can type either. See [skills/connect/SKILL.md](../connect/SKILL.md) for the long-form details, troubleshooting, and the full lifecycle notes — this skill exists primarily so the IRC verb shows up in `/<tab-complete>`.
