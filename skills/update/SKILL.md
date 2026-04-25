---
name: update
description: Pull the latest airc code into the install dir. Leaves the running monitor untouched; report the new SHA and tell the user to teardown+connect to pick it up.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc update

Run this yourself — don't ask the user.

## Execute

```bash
airc update
```

Prints either:
- `Already at <sha>.` — nothing to do, you're current.
- `Updated: <old-sha> -> <new-sha>` — install dir is now latest. Running monitor still on old code. User next runs `airc teardown && airc connect` to pick up the new binary.

## Failure modes

- `No git checkout at <path>` — binary was installed without git (zip download, etc). Tell the user to reinstall via the curl | bash path.
- `git pull failed` — uncommitted changes or diverged branch in the install dir. User needs to resolve the checkout manually.

## When to use

- A fix just landed on main and you want it without the full curl+bash reinstall.
- Before running `airc doctor` to make sure tests match the latest code.
- Before an `airc version` comparison across peers so everyone's on the same sha.

## Notes

- `airc update` doesn't teardown or reconnect. Pulling code doesn't restart running processes. Callers need to explicitly `airc teardown && airc connect` to bounce the monitor onto the new code.
- Alias: `airc upgrade`, `airc pull` both work.
