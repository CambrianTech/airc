---
name: relay:connect
description: Connect to Agent Relay — host or join another peer. Accepts flags for name, home dir, and port.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[join-string] [--name=N] [--home=PATH] [--port=N] [--scope=home|local]"
---

# Connect to Agent Relay

Do everything yourself — don't ask the user to run commands.

## 1. Install if needed

If `relay` is not on PATH:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

## 2. Parse `$ARGUMENTS`

Flags (any order, all optional):
- `--name=<n>` → sets `AGENT_RELAY_NAME=<n>` (peer identity)
- `--home=<path>` → sets `AGENT_RELAY_HOME=<path>` (state dir, overrides scope)
- `--port=<n>` → sets `AGENT_RELAY_PORT=<n>` (host listen port, default 7547)
- `--scope=local` → sets `AGENT_RELAY_HOME=$PWD/.agent-relay` (per-project identity)
- `--scope=home` → explicit default (`$HOME/.agent-relay`, one identity per machine)

Any non-flag argument is the join string. Its presence means JOIN mode; absence means HOST mode.

Build the env prefix from the flags that are set. Only include env vars the user asked for. Unset flags = use vanilla defaults.

## 3. Launch via Monitor

Once the env prefix is built, start the relay in a persistent Monitor so inbound streams as notifications:

```
Monitor(persistent=true, command="<env-prefix> relay connect <join-string-or-empty>")
```

If hosting (no join string), the relay prints a join string — show it to the user:

> "Share this with the other peer: `/relay:connect <the join string>`"

If joining, wait for the monitor's first event to confirm the pair succeeded.

## 4. After connecting

- `/relay:send <peer> <message>` — send a message (peer optional when there's only one peer; the skill figures it out)
- `/relay:rename <new-name>` — rename this peer; paired peers get a `[rename]` marker and auto-update their records

## 5. Troubleshooting

The relay prints actual errors. Read them.

- **SSH not working on host:** relay prints the exact sudo command. Show it to the user; they type `! sudo ...` to run it; retry.
- **Can't reach host:** host isn't running `relay connect`, address is wrong, or Tailscale isn't up on either side.
- **Port already in use:** add `--port=7548` (or another free number) when you start the host. The join string the host prints will carry the custom port automatically.
- **Two tabs on same machine peer-name collide:** set `--name=<something-unique>` on one side, or set `--scope=local` (gives each project dir its own identity). After connecting, `/relay:rename <new>` works too.
