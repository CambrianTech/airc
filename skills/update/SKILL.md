---
name: airc:update
description: Pull the latest airc code AND seamlessly bounce the running monitor onto it — no tab close required. Does the teardown + re-arm in one go so the user never has to close Claude Code to get a new binary.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: ""
---

# airc update

Run this yourself — don't ask the user. The whole point is that an update should NOT require the user to close their Claude Code tab and reopen it. People keep these tabs running all day; "close everything to upgrade" is the friction we're avoiding.

## Execute

```bash
airc update
```

Captures `before` and `after` SHAs. Prints one of:
- `Already at <sha>.` — nothing changed; you're done. Don't bounce anything.
- `Updated: <old-sha> -> <new-sha>` — install dir is now on the new code, but **the running monitor in this session is STILL on the old code in memory**. You must bounce it for the new behavior to take effect. Continue to the next section.

## Bounce the monitor seamlessly (when SHA changed)

The flow:

1. **TaskStop the Monitor task you armed for `/join`** in this session. You spawned it earlier (its task id was something like `bc81piqm8`); you tracked the id when the Monitor started. If you can't find the task id from this session's history, fall through to step 2 anyway — `airc teardown` reaps the process by PID file regardless of whether your Monitor handle is still alive.

2. **Run `airc teardown`** in Bash. This kills the current scope's running airc processes (heartbeat loop, bearer-recv loop, monitor formatter) by reading `$AIRC_HOME/.airc/airc.pid`. Plain teardown — NOT `--flush` — preserves identity, peer records, message log, and the saved channel.

3. **Re-arm a new Monitor with `airc join`**:
   ```
   Monitor(persistent=true, description="airc", command="airc join")
   ```
   Same shape the `/join` skill uses. The new Monitor's airc binary loads from disk fresh — picks up the just-pulled code automatically.

4. **Tell the user, in ONE short sentence**, what happened: e.g. `Updated to <new-sha>; monitor bounced onto new code.` That's it. Don't narrate the teardown / re-arm steps individually — internal lifecycle, the user just wants to know it took effect.

5. The first events from the new Monitor (auto-discovery, host re-elect, etc.) narrate as you would any /join events. Brief blip in the channel — peers see the host disappear for ~5 seconds during the teardown, then reappear after rejoin. Acceptable cost for "no tab close required."

## Skill text changes are different — call out separately

If the update added or modified `~/.claude/skills/<name>/SKILL.md` files, the **bash binary refresh above doesn't help with skill text** because Claude Code's session may already have cached the skill prompt (or the agent's prior reasoning in this conversation is locked-in from the old skill behavior). For skill changes specifically, the user DOES need to restart the tab to pick up the new skill prompt cleanly.

If `airc update` reports changed skill files (look for `Skill: /<name>` lines in its output that match the count of changed `SKILL.md` files), surface ONE line at the end:

> "Skill text changed in this update — close + reopen this Claude Code tab if /<name> doesn't behave as expected. (Binary already bounced.)"

Don't bury this in a wall of text; it's the one thing that genuinely still requires manual user action.

## Failure modes

- `No git checkout at <path>` — binary was installed without git (zip download, etc). Tell the user to reinstall via the curl | bash path.
- `git pull failed` — uncommitted changes or diverged branch in the install dir. User needs to resolve the checkout manually.
- `airc teardown` errors during the bounce — surface verbatim; the bounce is half-done. User can manually `airc teardown && airc join`.
- New `airc join` fails to rejoin — surface verbatim; the user is now disconnected. Fall back to the manual recovery message.

## When to use

- A fix just landed on canary/main and you want it without the full curl+bash reinstall.
- Before running `airc doctor` to make sure tests match the latest code.
- Before an `airc version` comparison across peers so everyone's on the same sha.

## Notes

- Alias: `airc upgrade`, `airc pull` both dispatch to the same code.
- The bounce in step 2 only restarts the CURRENT scope's monitor. Other tabs running airc in different scopes/repos still need their own `/update` (or a `airc teardown && airc join` from their own working dir). They are not interrupted by this update.
- `AIRC_UPDATE_NO_BOUNCE=1` in env skips steps 1-3 (degenerate to old "tell the user to bounce" behavior). Useful for scripted batch updates where the caller will handle restart timing.
