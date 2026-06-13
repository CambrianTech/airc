---
name: airc:doctor
description: Self-diagnose AIRC. AI checks environment health and live route/process state, then proactively fixes recoverable issues instead of just reporting them.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /doctor — operational reference

Audience: Claude Code, Codex, future agent runtimes. Goal: leave the user with a working airc, not a diagnosis to act on.

## Modes

| Command | Purpose |
|---|---|
| `airc doctor` | env probe — fast, local |
| `airc doctor --health` | live route/process health |
| `airc doctor --fix` | apply safe auto-recovery for detected issues (currently: stale daemon sockets) |

`--health` and `--fix` compose with the env probe and with each other.

## Decision tree

When something feels wrong, in this order:

1. **`airc doctor --health`** — live route/process state. Fast. Catches stopped local transport or bad route health. Green → bus is fine, issue is upstream.
2. **`airc doctor`** — env regression check.
3. **`airc inbox`** — pull buffered frames for the current room to confirm whether traffic is arriving.
4. **`airc doctor --fix`** — apply safe auto-recovery (e.g. clear a stale daemon socket) once 1-3 point at a recoverable local issue.

## --health output classes

| Marker | Meaning | Action |
|---|---|---|
| `[ok] gh core rate-limit: <N>/5000` | Healthy headroom | None |
| `[info] gh core rate-limit: <N>/5000` (<1000) | Reduced rendezvous headroom | Avoid unnecessary GitHub discovery |
| `[WARN] gh core rate-limit: <N>/5000` (<100) | Bus may stall soon | Wait for window reset; peers resume automatically |
| `[BLOCKED] gh API not reachable` | Network or token | Run `airc doctor` for env probe |
| `[ok] gh governor: no active backoff` | Cross-process gh guard is healthy | None |
| `[WARN] gh governor blocked <N>/<M>` | Local guard prevented gh spam recently | Wait; inspect top classes in output |
| `[BLOCKED] gh governor shared backoff active` | GitHub told this user/device to wait | Do not retry; wait for displayed seconds |
| `[ok] airc process running` | Daemon up | None |
| `[WARN] airc process not running` | Disconnected scope | `airc join` |
| `[ok] route health` | Healthy selected route | None |
| `[WARN] route health` | Degraded selected route | Run `airc transport health` |
| `[BLOCKED] route health` | No usable route | `airc join`, then inspect discovery/transport health |

## env probe (`airc doctor`)

Emits one line per prereq with `[ok]`, `[MISSING]`, or `[info]` (optional). For every `[MISSING]`, the next line is `Fix: <exact command>` for the platform's package manager (brew/apt/dnf/pacman/apk).

**Act on findings:**

- `[MISSING]` with a `Fix:` line → run it. Most are unattended (`brew install gh`, `sudo apt-get install -y openssh-client`).
- `gh authenticated (gist scope)` is interactive (browser flow) → instruct user: type `! gh auth login -s gist` so it runs in their terminal.
- `tailscale (optional)` lines never block (LAN-only mesh works without it).

## --fix (`airc doctor --fix`)

Applies only safe auto-recovery for detected issues — currently stale daemon
sockets. Identity partial states are reported with manual fix commands; doctor does
**not** wipe identity/trust state automatically. Without `--fix`, doctor only reports.

> ⚠️ The integration test suite (`airc doctor --tests <scenario>`, plus the `--join`
> pre-flight flag) is **not available in the rust-rewrite**. Diagnosis is the env
> probe + `--health` + `--fix`; there is no in-CLI scenario runner.

## Final report (one line)

- Green: `Healthy. Fixed X, Y.`
- Red:   `Fixed X. Still failing: <list>; next step <command>.`

Be specific about what you DID, not what you found.

## When to invoke

- Right after install — confirm gh + sshd aligned before pairing.
- After `airc update` — confirm new binary didn't regress.
- When something feels wrong — `--health` first, env probe second, logs third.
- Before opening an airc issue — paste BOTH `--health` and full env probe.
