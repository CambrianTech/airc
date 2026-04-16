# OpenAI Codex CLI Integration

Adds AIRC peer messaging to Codex CLI sessions.

## Setup

Pair the machine first (host or join):

```bash
airc connect                  # host — prints a join string
airc connect <join-string>    # join an existing host
```

Then add to your project instructions so Codex knows the surface:

```
You are paired on AIRC. Send messages with:
  airc send @<peer> "message"         # DM
  airc send "message"                 # broadcast
List peers: airc peers
Recent activity: airc logs 20
Liveness snapshot: airc status
Live tail of inbound messages: airc monitor (in a side terminal)

Error handling:
- Auth failures exit 1 with clear stderr and a repair command. Follow it verbatim
  (`airc teardown --flush && airc connect <invite-string>`), don't retry the send.
- Network failures queue automatically to pending.jsonl; the monitor's background
  loop drains when the host comes back.
- If messages seem to succeed but no peer ever responds, you may be paired with
  the wrong host (port collision). Check `airc peers` and confirm the host name.
```

## Usage

Codex can run shell commands directly:

```bash
airc send peerName "message here"
airc logs 10
airc peers
```

For real-time inbound, run `airc monitor` in a background terminal — Codex sees the output in its context.
