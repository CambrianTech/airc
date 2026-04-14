---
name: relay:connect
description: Connect to Agent Relay — host or join another machine. Per-project state in .agent-relay/ keeps peers and identity persistent.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[name@user@host#key]"
---

# Connect to Agent Relay

Do everything yourself — don't ask the user to run commands.

## 1. Install if needed

If `relay` is not on PATH:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

## 2. Project-scoped state

Each Claude instance keeps its own identity, peers, and message history in `$PWD/.agent-relay/` (alongside `.git`, `.vscode`, etc). Different projects → different state → no collisions on the same machine. Returning to the same project resumes your peer list and name automatically.

Set:
```
AGENT_RELAY_HOME=$PWD/.agent-relay
```

If `AGENT_RELAY_HOME` is already set in the environment, respect it (explicit override wins).

If `$PWD/.git` exists, append `.agent-relay/` to `$PWD/.gitignore` (create the file if missing, no duplicate lines) — the directory holds SSH keys and shouldn't be committed.

The agent-relay binary automatically uses the project basename as the peer display name unless you set `AGENT_RELAY_NAME` or rename later via `relay rename <new>`.

## 3. Connect

Run via Monitor so inbound messages stream as notifications:

**If `$ARGUMENTS` contains `@`** — joining a host:
```
Monitor(persistent=true, command="AGENT_RELAY_HOME=$PWD/.agent-relay relay connect $ARGUMENTS")
```

**If no arguments** — you are the host:
```
Monitor(persistent=true, command="AGENT_RELAY_HOME=$PWD/.agent-relay relay connect")
```

The host prints a join string. Give it to the user: "Share this with the other Claude:" followed by `/relay:connect <the join string>`.

## 4. After connecting

Always prefix relay commands with `AGENT_RELAY_HOME=$PWD/.agent-relay` (or whatever home you resolved) so you target the right identity:

- Send: `AGENT_RELAY_HOME=$PWD/.agent-relay relay send <peer> "<message>"`
- List peers: `AGENT_RELAY_HOME=$PWD/.agent-relay relay peers`
- Rename self: `AGENT_RELAY_HOME=$PWD/.agent-relay relay rename <new-name>` (notifies paired peers)

Inbound messages appear as Monitor events with shape `{"from":"<peer>","msg":"..."}`. Treat each as a turn from that peer and respond with `relay send`.

## 5. Troubleshooting

Relay prints the actual error. Read it.

- **Host mode, SSH not working:** relay prints the exact sudo command needed. Show it to the user. Retry after they run it.
- **Join mode, can't reach host:** host isn't running `relay connect`, or the address is wrong, or Tailscale isn't up on either side.
- **Messages from me echo in my own monitor:** project basename matches the peer's. Set `AGENT_RELAY_NAME=<something-unique>` explicitly, or `relay rename <new>` after connecting.
