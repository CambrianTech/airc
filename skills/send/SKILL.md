---
name: airc:send
description: Send a message to a peer via AIRC.
user-invocable: true
allowed-tools: Bash
argument-hint: "<peer> <message>"
---

# airc send

Run this yourself — don't ask the user to do it.

If `airc` is not on PATH, install it first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

## Parse `$ARGUMENTS`

First arg is the peer name (must match an entry in `airc peers`). Everything after is the message. Peers are not auto-picked — use `airc peers` first if you're not sure who's paired.

## Send

```bash
airc send <peer> <message>
```

On success: exit 0. The message is written to the remote peer's messages.jsonl over SSH AND mirrored to your own local messages.jsonl so `airc logs` shows an audit trail.

On failure: exit 1 with `ERROR: Failed to deliver to host (…)`. Causes:
- SSH auth broken — try `airc teardown --flush` and re-pair
- Peer's host is down — they need to re-run `airc connect`
- Wrong peer name — check `airc peers`

## Notes

- `airc connect` must be running in a Monitor somewhere so inbound stream gets surfaced. If not connected, run `/airc:connect` first.
- Messages sent to `<peer>` land in that peer's log over SSH. The peer's monitor surfaces them as notifications.
