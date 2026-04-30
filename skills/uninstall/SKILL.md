---
name: airc:uninstall
description: Fully remove airc from this machine — stops processes, removes the daemon, deletes the clone, drops binary + skill symlinks. Confirm with the user before running; this is destructive.
user-invocable: true
allowed-tools: Bash
argument-hint: "[--yes] [--purge]"
---

# airc uninstall

**Destructive — confirm with the user before running.** This removes airc itself; per-project `.airc/` state (identity keys, peer records, chat logs) is left alone unless the user explicitly asks for `--purge`.

## When to use

- The user says "uninstall airc" / "remove airc" / "I'm done with airc."
- The user is reinstalling from scratch and wants a clean slate first.
- A botched install left stale symlinks or a clone in a weird state, and `/repair` isn't enough.

## What it does

```bash
airc uninstall
```

Walks the full removal in order:

1. `airc teardown --all` — stops every running airc process across all scopes on this machine
2. `airc daemon uninstall` — removes the launchd / systemd-user / Task Scheduler unit if present
3. Removes binary forwarders: `~/.local/bin/{airc, relay, airc.cmd, airc.ps1}`
4. Removes airc skill symlinks under `~/.claude/skills/`
5. Removes the clone dir (`~/.airc-src` or `$AIRC_DIR`), including the `.venv` inside

**Confirmation prompt:** asks the user to type `yes` to proceed. If you're invoking from an agent, pass `--yes` only after the user has explicitly confirmed.

## Flags

- `--yes` / `-y` — skip the confirmation prompt. **Only pass this after the user confirms in chat.** Required for non-interactive invocations.
- `--purge` — also print the list of per-project `.airc/` state dirs the user would need to remove manually for a fully clean machine. Does NOT auto-delete them — those hold the user's identity keys, peer records, and chat history.

## What it leaves alone

- **Per-project `.airc/` state** — your identity keys, peer records, message logs in every dir you ran `airc join` from. Use `--purge` to get a list of them.
- **`gh` auth, brew/apt-installed packages** (gh / python3 / openssl) — those aren't airc's to remove.
- **Other agents' configs** (Codex, Cursor, opencode, Windsurf) — airc only owns its own integration files.

## Read the result

- `Uninstalled.` — full removal succeeded.
- `Aborted.` — user declined the prompt; nothing changed.
- `Non-interactive run: pass --yes to confirm uninstall.` — the script needed a TTY or `--yes`; you forgot the flag.

## Reinstall

After uninstall, the standard one-liner re-installs cleanly:

```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

Per-project `.airc/` dirs (if not purged) are picked up on the next `airc join` in that scope.
