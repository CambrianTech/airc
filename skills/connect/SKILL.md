---
name: airc:connect
description: Connect to AIRC — host or join another peer. Accepts flags for name, home dir, and port.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[join-string] [--name=N] [--home=PATH] [--port=N] [--scope=home|local]"
---

# Connect to AIRC

Do everything yourself — don't ask the user to run commands.

## 1. Install if needed

If `airc` is not on PATH:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

## 2. Parse `$ARGUMENTS`

Flags (any order, all optional):
- `--name=<n>` → sets `AIRC_NAME=<n>` (peer identity)
- `--home=<path>` → sets `AIRC_HOME=<path>` (state dir, overrides scope)
- `--port=<n>` → sets `AIRC_PORT=<n>` (host listen port, default 7547)
- `--scope=local` → sets `AIRC_HOME=$PWD/.airc` (per-project identity)
- `--scope=home` → explicit default (`$HOME/.airc`, one identity per machine)

Any non-flag argument is the join string. Its presence means JOIN mode; absence means HOST mode.

Build the env prefix from the flags that are set. Only include env vars the user asked for. Unset flags = use vanilla defaults.

## 3. Launch via Monitor

Once the env prefix is built, start the relay in a persistent Monitor so inbound streams as notifications:

```
Monitor(persistent=true, command="<env-prefix> airc connect <join-string-or-empty>")
```

If hosting (no join string), the relay prints a join string — show it to the user:

> "Share this with the other peer: `/airc:connect <the join string>`"

If joining, wait for the monitor's first event to confirm the pair succeeded.

## 4. After connecting

- `airc peers` — list paired peers you can send to
- `/airc:send <peer> <message>` — send to a specific peer by name (peer is required, not auto-picked)
- `/airc:rename <new-name>` — rename this identity; paired peers get a `[rename]` marker and auto-update
- `/airc:teardown` — kill this scope's airc processes and free its port (keeps state for resume; add `--flush` to wipe)
- `/airc:doctor` — self-diagnose: runs the integration suite to verify pairing, send, rename, scope, and teardown all work on this machine

## 5. Troubleshooting

The relay prints actual errors. Read them.

- **SSH not working on host:** relay prints the exact sudo command. Show it to the user; they type `! sudo ...` to run it; retry.
- **Can't reach host:** host isn't running `airc connect`, address is wrong, or Tailscale isn't up on either side.
- **Port already in use:** add `--port=7548` (or another free number) when you start the host. The join string the host prints will carry the custom port automatically.
- **Two tabs on same machine peer-name collide:** set `--name=<something-unique>` on one side, or set `--scope=local` (gives each project dir its own identity). After connecting, `/airc:rename <new>` works too.
