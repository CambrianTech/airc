---
name: airc:canary
description: Switch this airc install to the canary channel — pre-merge features queued for the next main release. Use when Joel asks you to test something that hasn't landed on main yet, or when you want the bleeding edge.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc canary

Run this yourself — don't ask the user.

## What it does

Switches this airc install to the `canary` channel. Under the covers:
- `git fetch origin canary`
- `git checkout canary`
- `git pull --ff-only`
- Refreshes skills + binary symlinks via `install.sh`
- Persists the choice to `$AIRC_DIR/.channel` so subsequent `airc update` (no args) stays on canary

## Execute

```bash
airc canary
```

Equivalent to `airc update --channel canary`. The shortcut exists because "go canary" is the common case for pre-merge testing.

## What "canary" means

| Channel | What it is | When to use |
|---|---|---|
| `main` | Stable. Most users run this. | Default. |
| `canary` | Long-lived branch ahead of main. Features that haven't been merged to main yet land here first; we test on canary, then promote canary→main as a single integration commit. | When testing a not-yet-merged feature, OR when you want bleeding edge. |

`canary` is just a git branch. Switching back is symmetric: `airc update --channel main` (or `airc channel main && airc update`).

## Rollback

If canary breaks something:

```bash
airc update --channel main
airc teardown && airc connect
```

That's it. Branch switch + restart monitor on the new code. Identity, peers, room state all persist (they're in `$AIRC_HOME`, not `$AIRC_DIR`).

## After switching

Tell the user:

> "Switched to canary (sha `<short-sha>`). Running monitor still uses old code — `airc teardown && airc connect` to pick up the new binary."

Then if they had a paired session you should restart the monitor for them:
```
Monitor(persistent=true, command="airc connect")
```

## When to use this skill

- Joel says "test the canary" / "try the new substrate work" / similar.
- A new feature is queued in canary and bigmama / memento / anvil need to validate before promotion.
- The user mentions a recent merged-to-canary PR by number (e.g. "test PR #40").

## When NOT to use this skill

- For routine updates → use `/airc:update` (stays on whatever channel they're on; doesn't switch).
- For first-time install → use `/airc:connect` which auto-installs main.

## Notes

- This is not "experimental beta channel" — canary is "merged-but-not-yet-promoted." Code on canary has passed local tests + the contributor's review; it just hasn't earned its way to main yet via cross-machine validation.
- Channel preference lives in `$AIRC_DIR/.channel`. Inspect with `airc channel`.
- If `gh auth status` is clean, the substrate (`airc connect` zero-arg → #general) works exactly the same on canary as on main — channels affect the airc binary, not the gist namespace.
