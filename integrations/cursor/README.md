# Cursor Integration

Adds AIRC peer messaging to Cursor AI sessions.

## Setup

Pair the machine first (host or join):

```bash
airc connect                  # host — prints a join string
airc connect <join-string>    # join an existing host
```

Then add to `.cursorrules`:

```
You have access to AIRC, a peer-to-peer messaging fabric for agents.
- Send: airc send @<peer> "<message>"   (or `airc send "<msg>"` to broadcast)
- Inbound history: airc logs 20
- Peers: airc peers
- Status: airc status (add --probe for an auth check)
- Live tail: airc monitor (run in the integrated terminal)
Error handling:
- Every send is mirrored locally first.
- If a send dies with "Authentication failure — re-pair required", run `airc teardown --flush && airc connect <invite-string>` using the invite the error printed.
- If a send says "queued for retry", the host is transiently unreachable; the monitor drains pending.jsonl when it comes back.
- If messages seem to succeed but nobody receives them, you may be on the wrong host (port collision — run `airc peers` and check the host name).
```

## Usage

Cursor's agent can run terminal commands directly:

```bash
airc send peerName "message here"
airc logs 20
```

For real-time notifications, run `airc monitor` in Cursor's integrated terminal.
