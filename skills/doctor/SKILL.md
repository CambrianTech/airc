---
name: airc:doctor
description: Self-diagnose AIRC. AI checks environment health (gh, ssh, ports), runs the integration suite, and proactively fixes recoverable issues (install gh, etc.) instead of just reporting them.
user-invocable: true
allowed-tools: Bash
argument-hint: "[scenario|all]"
---

# /doctor — operational reference

Audience: Claude Code, Codex, future agent runtimes. Goal: leave the user with a working airc, not a diagnosis to act on.

## Modes

| Command | Purpose |
|---|---|
| `airc doctor` | env probe (gh, ssh, python, tailscale) — fast, local |
| `airc doctor --connect` | pre-flight before `airc connect` (also probes cached host) |
| `airc doctor --health` | LIVE bus health (rate-limit headroom, daemon, per-channel bearer last-recv) |
| `airc doctor --fix` | repair recoverable issues (currently: gh auth re-login) |
| `airc doctor --tests [scenario]` | full integration suite (~245 assertions, 32 scenarios) |

Aliases for `--tests`: `airc tests`, `airc test`.

## Decision tree

When something feels wrong, in this order:

1. **`airc doctor --health`** — live bus state. Fast. Catches silent-blackout (rate-limited, daemon crashed, bearer wedged). Green → bus is fine, issue is upstream.
2. **`airc doctor`** — env regression check. Gh missing, sshd down, python broken.
3. **`airc logs --since 5m`** — most-recent message context.
4. **`airc doctor --tests`** — only if 1-3 are green and the bug is reproducible.

## --health output classes

| Marker | Meaning | Action |
|---|---|---|
| `[ok] gh core rate-limit: <N>/5000` | Healthy headroom | None |
| `[info] gh core rate-limit: <N>/5000` (<1000) | Reduced headroom | None; bearer auto-throttles per #416 |
| `[WARN] gh core rate-limit: <N>/5000` (<100) | Bus may stall soon | Wait for window reset; peers resume automatically |
| `[BLOCKED] gh API not reachable` | Network or token | Run `airc doctor` for env probe |
| `[ok] daemon running (pid N)` | Persistence layer up | None |
| `[WARN] daemon installed but DOWN` | Stale launchd/systemd state | `airc daemon restart` |
| `[info] daemon not installed` | Optional layer | Auto-suggest if user is on a laptop |
| `[ok] #<channel> — last bearer recv <Ns>` (<60s) | Healthy | None |
| `[info] #<channel> — last bearer recv <Ns>` (<5min) | Idle | None |
| `[WARN] #<channel> — last bearer recv <Ns>` (5-30min stale) | Check daemon + rate-limit | Surface to user |
| `[BLOCKED] #<channel> — last bearer recv <Ns>` (>30min wedged) | Bearer wedged | `airc teardown && airc join` |

## env probe (`airc doctor`)

Emits one line per prereq with `[ok]`, `[MISSING]`, or `[info]` (optional). For every `[MISSING]`, the next line is `Fix: <exact command>` for the platform's package manager (brew/apt/dnf/pacman/apk).

**Act on findings:**

- `[MISSING]` with a `Fix:` line → run it. Most are unattended (`brew install gh`, `sudo apt-get install -y openssh-client`).
- `gh authenticated (gist scope)` is interactive (browser flow) → instruct user: type `! gh auth login -s gist` so it runs in their terminal.
- `tailscale (optional)` lines never block (LAN-only mesh works without it).

## Integration suite (`--tests`)

```bash
airc doctor --tests $ARGUMENTS
```

Empty `$ARGUMENTS` (or `all`) runs every scenario. Single-scenario invocation: `tabs`, `scope`, `room`, `teardown`, `reminder`, `resilience`, `reconnect`, `queue`, `status`, `auth_failure`, `resume_stale_auth`. Suite uses port 7549 + `AIRC_HOME=/tmp/airc-it-*` — safe alongside live airc on 7547/7548. Runtime: ~2min for `all`, 10-30s per scenario.

Final line: `N passed, M failed`.

## Failure → action (test scenarios)

| Failure name | Cause | Action |
|---|---|---|
| `alpha host failed to start` | Port 7549 taken OR airc not on PATH | `lsof -iTCP:7549` → kill if safe; verify `command -v airc` |
| `beta join failed` | sshd down OR firewall blocks loopback ssh | Enable Remote Login (mac) / start sshd (linux); `ssh localhost echo ok` |
| `scope: ...` | Two-tier resolver regression | Bisect against last green sha; file issue |
| `teardown in different scope killed foreign host` | Scope isolation broke (CRITICAL) | File issue immediately — would let one tab nuke another |
| `room: alpha unexpectedly wrote room_gist_id under --no-gist` | `--no-gist` not honored on push branch | Regression in `cmd_connect` host-mode gist gate |
| `room: alpha cmd_part DID NOT identify as host` | `cmd_part` host detection regressed | Host signal = `config.json::host_target` empty; do NOT fall back to gist_id presence |
| `auth_failure: stderr did NOT mention re-pair` | `cmd_send` auth-class detection regressed | Check regex against `permission denied\|publickey\|host key\|...` |
| `resume_stale_auth: invite string` | Resume probe didn't reconstruct invite | Regression in `cmd_connect` resume probe failure branch |

Failure not in table:
1. Read failure verbatim.
2. Trace into `test/integration.sh` for that scenario name to find the failing assertion.
3. Read the relevant section of the airc binary.
4. Hypothesis → fix → re-run scenario alone: `airc doctor --tests <scenario>`.

## Final report (one line)

- Green: `Fixed X, Y. All tests green.`
- Red:   `Fixed X. Tests N passed M failed; failures: <list>.`

Be specific about what you DID, not what you found.

## When to invoke

- Right after install — confirm gh + sshd aligned before pairing.
- After `airc update` — confirm new binary didn't regress.
- When something feels wrong — `--health` first, env probe second, logs third.
- Before opening an airc issue — paste BOTH `--health` and full env probe.
