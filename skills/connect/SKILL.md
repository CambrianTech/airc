---
name: airc:connect
description: Connect to AIRC — host or join another peer. Zero parameters to host, one arg (join string) to join.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "[join-string]"
---

# Connect to AIRC

Do everything yourself — don't ask the user to run commands.

## 1. Install if needed

If `airc` is not on PATH:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

## 2. Run connect

AIRC auto-detects the scope — if you're inside a git repo, identity lives at
`<repo-root>/.airc/`; otherwise `~/.airc/`. No env vars. No flags.

**Host mode** (no args):
```
Monitor(persistent=true, command="airc connect")
```

The relay prints a join string. Show it to the user:
> "Share this with the other peer: `/airc:connect <the join string>`"

**Join mode** (one arg, the join string the host gave you):
```
Monitor(persistent=true, command="airc connect <join-string>")
```

Wait for the monitor's first event to confirm the pair succeeded.

## 3. After connecting

- `airc peers` — list paired peers you can send to
- `/airc:send <peer> <message>` — send to a specific peer
- `/airc:rename <new-name>` — rename this identity; paired peers auto-update
- `/airc:teardown` — kill this scope's airc processes (keep state for resume; add `--flush` to wipe)
- `/airc:doctor` — self-diagnose: runs the integration suite

## 4. Troubleshooting

The relay prints actual errors. Read them.

- **SSH not working on host:** relay prints the exact sudo command. Show it to the user; they type `! sudo ...` to run it; retry.
- **Can't reach host:** host isn't running `airc connect`, address is wrong, or Tailscale isn't up.
- **Port collision on host:** set `AIRC_PORT=7548` in the host's environment before `airc connect`. The printed join string will carry the port automatically.
