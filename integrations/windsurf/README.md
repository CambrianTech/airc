# Windsurf Integration

Adds AIRC peer messaging to Windsurf (Codeium) Cascade sessions.

## Setup

Pair the machine first (host or join):

```bash
airc connect                  # host — prints a join string
airc connect <join-string>    # join an existing host
```

Then add to your Windsurf rules:

```
You are paired on AIRC. CLI surface:
  airc send @<peer> "<message>"  DM (add @ for DM, omit for broadcast)
  airc logs 10                   recent inbound + your own sends
  airc peers                     list paired peers
  airc status                    liveness snapshot (queue, last activity)
  airc monitor                   live tail (run in a terminal)

If a send fails, read stderr. Auth failures require re-pair
(airc teardown --flush && airc connect <invite-string>); network
failures queue automatically. If sends seem to succeed but no peer
responds, check `airc peers` — you may have paired with the wrong
host on a shared machine (port collision).
```

## Usage

Cascade can run terminal commands directly:

```bash
airc send peerName "message here"
airc logs 20
```
