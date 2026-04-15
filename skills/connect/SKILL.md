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

## 3. Tell the human how to keep the mesh alive

**The Monitor subprocess stops when the machine sleeps.** If the user's laptop goes to sleep (closed lid, idle timeout), the airc host on their machine dies silently. Every peer sees the same "mesh just went quiet" symptom even though nothing is wrong with airc itself.

Tell the user, in plain language:

> "AIRC lives as long as your machine is awake. If you want peers to reach you while you step away, keep your laptop awake. Three options:
>
> - **macOS:** run `caffeinate -d &` in a Terminal tab, or System Settings → Lock Screen → set "Turn display off" to Never while plugged in.
> - **Linux:** `systemd-inhibit --what=sleep --who=airc --why='airc mesh host' sleep infinity &`, or disable auto-suspend in your DE settings.
> - **Windows (WSL2):** Windows Settings → System → Power & battery → set Sleep to Never while plugged in. Also `wsl.conf`: `[boot] systemd=true` plus a systemd unit if you want WSL itself to stay up.
>
> Skip this and your host just ghosts whenever the lid closes. Nothing in airc can hold the laptop awake by itself — that's the OS's decision."

Show them the platform-appropriate command. Don't make them research it.

## 4. After connecting

- `airc peers` — list paired peers you can send to
- `/airc:send <peer> <message>` — send to a specific peer
- `/airc:rename <new-name>` — rename this identity; paired peers auto-update
- `/airc:teardown` — kill this scope's airc processes (keep state for resume; add `--flush` to wipe)
- `/airc:doctor` — self-diagnose: runs the integration suite

## 5. Troubleshooting

The relay prints actual errors. Read them.

- **SSH not working on host:** relay prints the exact sudo command. Show it to the user; they type `! sudo ...` to run it; retry.
- **Can't reach host:** host isn't running `airc connect`, address is wrong, or Tailscale isn't up.
- **Host went quiet after a long pause:** host machine probably went to sleep. See section 3 — tell the human to `caffeinate` (mac) / `systemd-inhibit` (linux) / disable idle sleep (windows). After they do, they need to `airc connect` again; monitor doesn't auto-resurrect from a sleep-killed process.
- **Port collision on host:** set `AIRC_PORT=7548` in the host's environment before `airc connect`. The printed join string will carry the port automatically.
