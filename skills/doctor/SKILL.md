---
name: airc:doctor
description: Self-diagnose AIRC. AI checks environment health (gh, ssh, ports), runs the integration suite, and proactively fixes recoverable issues (install gh, etc.) instead of just reporting them.
user-invocable: true
allowed-tools: Bash
argument-hint: "[scenario|all]"
---

# airc doctor

Run this yourself — don't ask the user. Goal: leave the user with a working airc, not a diagnosis they have to act on.

## Step 1 — environment health check

The substrate is gh-rooted. An absent / unauthed gh is the #1 cause of "airc feels broken." Run the built-in probe first:

```bash
airc doctor
```

This emits one line per prereq with `[ok]`, `[MISSING]`, or `[info]` (optional/Tailscale). For every `[MISSING]` line, the next line is `Fix: <exact command>` for the platform's package manager (brew / apt / dnf / pacman / apk; or a manual hint when no manager is detected).

**Act on findings, don't just print them:**

- For each `[MISSING]` prereq with a `Fix:` line: run the fix. Most are unattended (`brew install gh`, `sudo apt-get install -y openssh-client`, etc.).
- `gh authenticated (gist scope)` is interactive (browser flow) — instruct the user to type `! gh auth login -s gist` so it runs in their terminal session.
- `tailscale (optional)` lines never block the user (LAN-only mesh works without it). Install only if they want cross-LAN reach, then `tailscale up` is also interactive.

If `airc doctor` says **"All required prereqs present"**, environment is good — proceed to Step 2.

## Step 2 — run the integration suite

```bash
airc doctor --tests $ARGUMENTS
```

(Aliases: `airc doctor tests`, `airc tests`, `airc test`.)

Empty `$ARGUMENTS` (or `all`) runs every scenario. A scenario name (`tabs`, `scope`, `room`, `teardown`, `reminder`, `resilience`, `reconnect`, `queue`, `status`, `auth_failure`, `resume_stale_auth`) runs just that one. Suite uses port 7549 + `AIRC_HOME=/tmp/airc-it-*`; safe alongside live airc on 7547/7548.

## Step 3 — interpret + act

Final line: `N passed, M failed`.

### Green (`0 failed`)

- Environment OK + tests OK → tell the user "airc is healthy. Run `airc join` to join the substrate."
- Mention what you fixed in step 1 if anything.

### Red

For each failure name in the trace, look it up in this table and **act, don't just report**:

| Failure | Likely cause | What to do |
|---|---|---|
| `alpha host failed to start` | Port 7549 taken, OR airc not on PATH | `lsof -iTCP:7549` → kill if safe; verify `command -v airc` |
| `beta join failed` | sshd not running, OR firewall blocks loopback ssh | enable Remote Login (mac) / start sshd (linux); test `ssh localhost echo ok` |
| `scope: ...` | Two-tier resolver in airc binary regressed | rare — bisect against last green sha; this is upstream airc, file an issue |
| `teardown in different scope killed foreign host` | Scope isolation broke (critical) | file an issue immediately; this would let one Claude tab nuke another's session |
| `room: alpha unexpectedly wrote room_gist_id under --no-gist` | Use of --no-gist isn't honored on the gist-push branch | regression in cmd_connect's host-mode gist push gate |
| `room: alpha cmd_part DID NOT identify as host` | cmd_part's host-vs-joiner detection regressed | host signal is `config.json::host_target` empty; do not fall back to gist_id presence (that was the pre-PR2 bug) |
| `auth_failure: stderr did NOT mention re-pair` | cmd_send's auth-class-error detection regressed | check the regex against `permission denied|publickey|host key|...` |
| `resume_stale_auth: invite string` | Resume probe didn't reconstruct the saved invite for the user | regression in cmd_connect's resume probe failure branch |

If a failure isn't in the table:
- Read the failure verbatim
- Trace into `test/integration.sh` for that scenario name to understand what assertion fired
- Read the relevant section of the airc binary
- Form a hypothesis, fix it, re-run that scenario alone (`airc doctor --tests <scenario>`)

## Step 4 — final report

One line: "Fixed X, Y. All tests green." OR "Fixed X. Tests N passed M failed; failures: <list>." Be specific about what you did, not what was found.

## Live-bus health (post-join) — `airc doctor --health`

If the user is already joined and peers feel quiet, **don't wait** — run `airc doctor --health` first. It probes the running substrate (not the env) and pinpoints the silent-blackout failure modes:

```bash
airc doctor --health
```

Surfaces:
- **gh API rate-limit headroom** — `[WARN]` if <100 remaining (bus may stall soon), `[BLOCKED]` if API unreachable. Mitigation: bearer auto-throttles (#416); peers resume when window resets.
- **Daemon liveness** — if installed but DOWN, suggests `airc daemon restart`. If not installed, suggests it as an optional layer (survives sleep/crash).
- **Per-channel bearer last-recv age** — `[ok]` if <60s, `[info]` if <5min (idle), `[WARN]` if 5-30min stale (check daemon/rate-limit), `[BLOCKED]` if >30min (bearer wedged — `airc teardown && airc join`).

Use it BEFORE diving into logs. If `--health` is green, the bus is fine and the issue is upstream (peer not running airc, peer's gh down, etc.). If `--health` flags something, the fix is right there.

## When to run this skill

- Right after install — confirms airc + gh + sshd all aligned before pairing for real.
- After `airc update` — confirms the new binary didn't regress, and that any new env requirements (e.g. gh in #38, gh in #39) are met.
- **When something feels wrong** — `airc doctor --health` first (live bus state). If green, then full `airc doctor` to rule out env regressions. Logs are the third resort, not the first.
- Before opening an airc issue — paste the doctor output (BOTH `--health` and the full env probe) so the maintainer doesn't have to ask.

## Notes

- Scenarios are gh-free; the substrate ITSELF (`airc join` zero-arg, `airc list`) requires gh. That's a feature, not a bug — gh is the comm layer.
- Suite runtime is ~2 minutes for `all`; individual scenarios are 10-30s.
- This skill assumes you can run shell commands. The user should not have to type anything except the interactive `gh auth login -s gist` flow if you encounter it.
