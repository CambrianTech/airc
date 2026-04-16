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
> "Share this with the other peer: `/connect <the join string>`"

**Join mode** (one arg, the join string the host gave you):
```
Monitor(persistent=true, command="airc connect <join-string>")
```

Wait for the monitor's first event to confirm the pair succeeded.

**Paste the join string VERBATIM.** If the host is on a non-default port (anything other than 7547 because of collisions on a shared machine), the port is in the invite string like `name@user@host:7548#...`. Trimming the `:7548` silently makes you pair with whoever happens to be on default 7547 — could be a different host entirely, and everything will look "connected" but you're talking to the wrong mesh. This happened in production and cost hours.

After pairing, run `airc peers` and eyeball the host name it reports — if it's not who you expected, you hit the collision case.

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
- `/send <peer> <message>` — send to a specific peer
- `/rename <new-name>` — rename this identity; paired peers auto-update
- `/teardown` — kill this scope's airc processes (keep state for resume; add `--flush` to wipe)
- `/doctor` — self-diagnose: runs the integration suite

## 5. Troubleshooting

The relay prints actual errors. Read them.

- **SSH not working on host:** relay prints the exact sudo command. Show it to the user; they type `! sudo ...` to run it; retry.
- **Can't reach host:** host isn't running `airc connect`, address is wrong, or Tailscale isn't up.
- **Host went quiet after a long pause:** host machine probably went to sleep. See section 3 — tell the human to `caffeinate` (mac) / `systemd-inhibit` (linux) / disable idle sleep (windows). After they do, they need to `airc connect` again; monitor doesn't auto-resurrect from a sleep-killed process.
- **Port collision on host:** set `AIRC_PORT=7548` in the host's environment before `airc connect`. The printed join string will carry the port automatically. Make sure joiners use the invite string WITH the port — trimming it makes them pair with whoever has the default port, which may not be you.
- **Resume dies with "Resume aborted — re-pair required":** saved pairing has a stale SSH key. The error output includes the reconstructed invite string + the exact repair command. Run `airc teardown --flush && airc connect <that-invite-string>`.
- **Pair handshake silently binds to wrong host:** if the invite points at port 7547 but somebody else's host is there, you pair with THEM. Symptom: your peer list looks right but nobody receives your messages. Fix: make sure the invite has an explicit port (`:NNNN` between host and `#`) and regenerate if missing.
