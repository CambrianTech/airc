---
name: airc:canary
description: "⚠️ Not available in rust-rewrite: channel switching is not a CLI verb. There is no `airc canary` and `airc update` has no `--channel` flag. Switch the install checkout's git branch manually if you need canary."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc canary

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc canary` verb, and `airc update` has **no `--channel`
> flag** — channel switching is not a CLI operation in the rust-rewrite. `airc update`
> simply fast-forwards the installed source checkout on whatever branch it is on and
> refreshes the binary + skills.

Run this yourself — don't ask the user.

## What "canary" means

| Channel | What it is | When to use |
|---|---|---|
| `main` | Stable. Most users run this. | Default. |
| `canary` | Long-lived branch ahead of main: features land here first, then promote canary→main as a single integration commit. | Testing a not-yet-merged feature, or you want bleeding edge. |

`canary` is just a git branch in the install checkout. Because there is no
channel-switch verb, moving between branches has to be done in that checkout directly
(then re-run `airc update` to rebuild + refresh skills), and is outside the supported
`airc` command surface for now.

## Nearest real path

`airc update` (alias `airc upgrade` / `airc pull`) fast-forwards the current branch
and refreshes the binary + skills. To validate canary code you would point the install
checkout at the canary branch yourself, then:

```bash
airc update
```

Then restart this scope so the running daemon picks up the new binary:

Claude Code:
```
Monitor(persistent=true, description="airc", command="airc join")
```

Codex / non-Monitor runtimes:
```bash
airc join
```

## When this comes up

- Joel says "test the canary" / "try the new substrate work". Tell him channel switching isn't a CLI verb in the rust-rewrite; the install checkout's branch has to be moved manually, then `airc update`.
- For routine updates on the current branch → use `/airc:update`.

## Notes

- Identity, peers, and room state persist across an update (they're in `$AIRC_HOME`, not the install dir).
- Do not invent `airc update --channel ...` or `airc channel ...` — neither exists in the rust-rewrite.
