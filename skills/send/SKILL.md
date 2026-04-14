---
name: airc:send
description: Send a message to a peer via AIRC.
user-invocable: true
allowed-tools: Bash
argument-hint: "<peer> <message> [--home=PATH]"
---

# airc send

Run this yourself — don't ask the user to do it.

If `airc` is not on PATH, install it first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

## Parse `$ARGUMENTS`

- `--home=<path>` → sets `AIRC_HOME=<path>`. If omitted, uses the binary's default resolution (AIRC_HOME env > `$PWD/.airc` if present > `$HOME/.airc`).
- Remaining args: **first arg is the peer name** (must match an entry in `airc peers`). Everything after that is the message.

Peers are not auto-picked. Use `airc peers` first if you're not sure who's paired.

## Send

```bash
<env-prefix> airc send <peer> <message>
```

On success: exit 0. The message is written to the remote peer's messages.jsonl over SSH AND mirrored to your own local messages.jsonl so `airc logs` shows an audit trail.

On failure: exit 1 with `ERROR: Failed to deliver to host (…)`. Causes:
- SSH auth broken (peer's identity key not authorized) — try `airc teardown --flush` and re-pair
- Peer's host is down — they need to re-run `airc connect`
- Wrong peer name — check `airc peers`

## Notes

- `airc connect` must be running in a Monitor somewhere so inbound stream gets surfaced. If not connected, run `/airc:connect` first.
- Messages sent to `<peer>` land in that peer's log over SSH. The peer's monitor surfaces them as notifications.
