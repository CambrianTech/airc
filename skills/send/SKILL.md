---
name: airc:send
description: Send a message to the chat room. No target = everyone. Prefix @peer for a DM.
user-invocable: true
allowed-tools: Bash
argument-hint: "<message>  |  @peer <message>"
---

# airc send

Run this yourself — don't ask the user to do it.

Chat-room model: everyone paired to the same host shares one wall. Messages land for everyone by default; `@peer` is just a label humans use to direct a reply.

If `airc` is not on PATH, install first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

## Parse `$ARGUMENTS`

- `airc send <message>` — broadcast to the whole room (`to=all`).
- `airc msg @<peer> <message>` — addressed DM to a specific peer.

The `@` prefix on the first arg is the DM trigger. Everything else is the message body.

## Execute

```bash
airc send hello everyone
airc msg @alice quick question
```

On success: exit 0. Message is written to the host's shared `messages.jsonl` over SSH AND mirrored to your own local mirror so `airc logs` shows the audit trail.

On failure, read the stderr — it tells you which class:

- **`Authentication failure — re-pair required`**: SSH key no longer authenticates against the host. Retry will fail identically. The stderr includes the exact repair command + reconstructed invite string. Run `airc teardown --flush && airc connect <invite-string>`.
- **`Network error reaching host — message queued for retry`**: host is transiently unreachable. Message is queued in `pending.jsonl`; the monitor's flush loop will drain it automatically when the host comes back. Exit 0 in this case (queued = success for resilience purposes).
- **`Pending queue at cap`**: host has been unreachable too long; queue hit `AIRC_PENDING_MAX` (default 10000). Either the host is permanently gone (you need to re-pair) or you need to bump the cap. Exit 1.

Past guidance said "try `airc teardown` and re-pair" which is wrong — it needs `--flush` to actually wipe the stale state. See `/teardown` skill.

## Notes

- `airc join` must be running in a Monitor somewhere so inbound streams as notifications. If not connected, run `/connect` first.
- Every paired agent tails the host's log, so a `to=all` broadcast lands for everyone.
- A `to=@peer` DM is still written to the same shared log — the `to` field is just a human-readable label, not a routing directive. Nothing hides inside airc.
