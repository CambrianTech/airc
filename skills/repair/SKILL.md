---
name: airc:repair
description: Full re-pair of a stale airc mesh when identity/pairing state is corrupt. Most monitor recovery should use `airc join` instead.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[invite-string]"
---

# airc repair

The one-command recovery for the most common airc failure: your saved pairing is stale (SSH key rotated, host regenerated identity, reinstall broke things, you accidentally paired with the wrong host because of a port collision). Runs the full nuclear repair sequence so you don't have to remember the flag names or hunt for the invite string.

## Execute

If `$ARGUMENTS` contains an invite string, use it directly. Otherwise do a clean local reset and let `airc join` recreate the account-context subscriptions from the ORM-backed identity, subscription, peer, and coordinator stores. Do not reconstruct pairing from `config.json`; that file is not part of the redesigned runtime.

### Step 1 — teardown --flush

```bash
airc teardown --flush
```

Wipes identity, peer records, saved pairing, messages. State is gone.

### Step 2 — join

Claude Code:
```
Monitor(persistent=true, description="airc", command="airc join ${ARGUMENTS}")
```

Codex / non-Monitor runtimes:
```bash
airc join ${ARGUMENTS:+"$ARGUMENTS"}
```

Fresh handshake, fresh identity keys get pushed to the host's authorized_keys, clean pair.

## When to use

- `airc join` (resume) exited with `Resume aborted — re-pair required`.
- `airc send` exited with `Authentication failure — re-pair required`.
- You re-installed airc and your mesh stopped working.
- You suspect you paired with the wrong host because of a port collision — `airc peers` reports a host name you didn't expect.
- "Nothing works and I don't know why" — repair is the cheap nuclear option.

## Failure modes

- No invite passed and the peer is not discoverable through the account coordinator. User needs a fresh invite from the host. Ask them to get `/invite` output from the host and pass it as the argument.
- Repair succeeds but still no messages — you may genuinely be on the wrong host. Run `airc peers` and confirm the host name matches who you meant to pair with. If not, ask the host to paste their `/invite` output and try `/repair <that-invite>`.

## Notes

- This is intentionally destructive. Identity keys, peer records, message mirror — all gone. The messages on the shared host log survive; only YOUR local mirror resets.
- Safer than guessing which flag to `airc teardown` with. Pre-repair-skill, users reliably typed `airc teardown` (no flush) + `airc join` (resume) and silently stayed broken. Using this skill removes the footgun.
