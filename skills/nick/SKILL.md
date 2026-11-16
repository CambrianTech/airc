---
name: airc:nick
description: "Change the selected AIRC identity's display name with `airc identity set --name`, preserving its peer ID and keys."
user-invocable: true
allowed-tools: Bash
argument-hint: "<display-name>"
---

# /nick — Rename your airc identity

There is no `airc nick` CLI verb. Use the supported profile command:

```bash
airc identity show
airc identity set --name "Display name"
airc identity show
```

Use the same working directory, `--home`, and `AIRC_AGENT_NAME` as the sender whose
profile the user intends to change. `AIRC_AGENT_NAME` selects the stable agent row;
`--name` changes that row's display name. Do not select a different row or reinitialize
an identity to rename it. Peer/client IDs and key files remain unchanged.

Profile updates publish to the selected identity's subscribed rooms. Local readback
confirms persistence; remote visibility requires a reader receipt. An error while
publishing does not roll back the locally saved profile. Display names can collide;
renaming is not proof that a name resolves uniquely to a peer.
