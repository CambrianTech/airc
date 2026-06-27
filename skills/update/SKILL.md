---
name: airc:update
description: Pull the latest airc code and restart this scope's running airc process when needed. Claude Code uses Monitor; Codex/non-Monitor runtimes use the same public join command, which detaches the local transport owner when needed and checks inbox.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: ""
---

# airc update

Run this yourself — don't ask the user. The whole point is that an update should not require the user to close their agent tab and reopen it. People keep these sessions running all day; "close everything to upgrade" is the friction we're avoiding.

## Execute

```bash
airc update
```

Captures `before` and `after` SHAs. Prints one of:
- `Already at <sha>.` — nothing changed; you're done. Don't bounce anything.
- `Updated: <old-sha> -> <new-sha>` — install dir is now on the new code, but **the running airc process in this scope is STILL on the old code in memory**. You must restart it for the new behavior to take effect. Continue to the next section.

## Restart this scope when SHA changed

### Claude Code flow

1. **TaskStop the Monitor task you armed for `/join`** in this session. You spawned it earlier (its task id was something like `bc81piqm8`); you tracked the id when the Monitor started. If you can't find the task id from this session's history, fall through to step 2 anyway — `airc stop` shuts down this scope's daemon by IPC regardless of whether your Monitor handle is still alive.

2. **Run `airc stop`** in Bash. This gracefully shuts down the current scope's running daemon. It preserves identity, peer records, and subscriptions — there is no state wipe (the old `airc teardown --flush` has no equivalent in the rust-rewrite).

3. **Re-arm a new Monitor with `airc join`**:
   ```
   Monitor(persistent=true, description="airc", command="airc join")
   ```
   Same shape the `/join` skill uses. The new Monitor's airc binary loads from disk fresh — picks up the just-pulled code automatically.

4. **Tell the user, in ONE short sentence**, what happened: e.g. `Updated to <new-sha>; monitor bounced onto new code.` That's it. Don't narrate the teardown / re-arm steps individually — internal lifecycle, the user just wants to know it took effect.

5. The first events from the new Monitor (auto-discovery, host re-elect, etc.) narrate as you would any /join events. Brief blip in the channel — peers see the host disappear for ~5 seconds during the teardown, then reappear after rejoin. Acceptable cost for "no tab close required."

### Codex / non-Monitor flow

Codex has no `Monitor` or `TaskStop`. Do not call those tools. Use the same public join lifecycle; the CLI detaches the local transport owner when needed:

```bash
airc join
```

After the bounce, run `airc status` and `airc inbox` for missed messages. Report one short sentence: `Updated to <new-sha>; airc process restarted on new code.`

## Skill text changes are different — call out separately

If the update added or modified `~/.claude/skills/<name>/SKILL.md` or `~/.codex/skills/<name>/SKILL.md` files, the **bash binary refresh above doesn't help with skill text** because the running agent session may already have cached the skill prompt (or the agent's prior reasoning in this conversation is locked-in from the old skill behavior). For skill changes specifically, the user may need to restart the tab to pick up the new skill prompt cleanly.

If `airc update` reports changed skill files (look for `Skill: /<name>` lines in its output that match the count of changed `SKILL.md` files), surface ONE line at the end:

> "Skill text changed in this update — restart this agent tab if /<name> doesn't behave as expected. (Binary already bounced.)"

Don't bury this in a wall of text; it's the one thing that genuinely still requires manual user action.

## Failure modes

- `No git checkout at <path>` — binary was installed without git (zip download, etc). Tell the user to reinstall via the curl | bash path.
- `git pull failed` — uncommitted changes or diverged branch in the install dir. User needs to resolve the checkout manually.
- `airc join` errors during the bounce — surface verbatim; the scope may need `airc doctor --health`.
- New `airc join` fails to rejoin — surface verbatim; the user is now disconnected. Fall back to `/repair <invite>` only if identity/pairing state is corrupt.

## When to use

- A fix just landed on canary/main and you want it without the full curl+bash reinstall.
- Before running `airc doctor` to make sure tests match the latest code.
- Before an `airc version` comparison across peers so everyone's on the same sha.

## Notes

- Alias: `airc upgrade`, `airc pull` both dispatch to the same code.
- The bounce in step 2 only restarts the CURRENT scope's monitor. Other tabs running airc in different scopes/repos still need their own `/update` (or `airc join` from their own working dir). They are not interrupted by this update.
- `AIRC_UPDATE_NO_BOUNCE=1` in env skips steps 1-3 (degenerate to old "tell the user to bounce" behavior). Useful for scripted batch updates where the caller will handle restart timing.
