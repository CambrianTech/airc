---
name: airc:doctor
description: Self-diagnose AIRC. AI checks environment health (gh, ssh, ports), runs the integration suite, and proactively fixes recoverable issues (install gh, etc.) instead of just reporting them.
user-invocable: true
allowed-tools: Bash
argument-hint: "[scenario|all]"
---

# airc doctor

Run this yourself — don't ask the user. Goal: leave the user with a working airc, not a diagnosis they have to act on.

## Step 1 — environment health check (do this BEFORE running tests)

The substrate is gh-rooted. Check the environment first; an absent / unauthed gh is the #1 cause of "airc feels broken." If you can fix it, fix it.

```bash
# (a) gh installed?
command -v gh >/dev/null 2>&1 && echo "gh: present" || echo "gh: MISSING"

# (b) gh authenticated?
gh auth status 2>&1 | head -3

# (c) ssh remote login on this machine? (needed for tabs/scope tests + real pairing)
# macOS:
sudo systemsetup -getremotelogin 2>/dev/null || true
# Linux: just check sshd is running
systemctl is-active sshd 2>/dev/null || pgrep -f "sshd" >/dev/null && echo "sshd: active" || echo "sshd: NOT running"

# (d) port 7549 (test-reserved) free?
lsof -iTCP:7549 -sTCP:LISTEN 2>/dev/null | head -3 || echo "7549: free"
```

**Act on findings, don't just print them:**

- **`gh: MISSING`** → install gh. macOS: `brew install gh`. Linux Ubuntu/Debian: `sudo apt install gh` (or follow https://github.com/cli/cli#installation). Windows: `winget install GitHub.cli` or `choco install gh`. Then tell the user to `gh auth login` (needs interactive browser flow — they have to run this themselves).
- **`gh auth: not logged in`** → `gh auth login` (must be interactive — instruct the user to type `! gh auth login` so it runs in the terminal session and the browser flow can complete).
- **`sshd: NOT running`** (macOS) → `sudo systemsetup -setremotelogin on` (the user has to run this; needs sudo). Or System Settings → General → Sharing → Remote Login.
- **`7549: <pid>`** → port held by something else; `lsof -tiTCP:7549 -sTCP:LISTEN | xargs kill` if the process is one you can identify and kill safely. Otherwise tell the user.

Why this comes BEFORE the tests: the integration suite is `gh`-free by design (uses inline invites + local SSH), so missing gh wouldn't fail the tests — but the user will still be unable to use the substrate (`airc join` zero-arg auto-discovery, `airc list`). Doctor should catch and fix that.

## Step 2 — run the integration suite

```bash
airc doctor $ARGUMENTS
```

Empty `$ARGUMENTS` (or `all`) runs every scenario. A scenario name (`tabs`, `scope`, `room`, `teardown`, `reminder`, `resilience`, `reconnect`, `queue`, `status`, `auth_failure`, `resume_stale_auth`) runs just that one. Suite uses port 7549 + `AIRC_HOME=/tmp/airc-it-*`; safe alongside live airc on 7547/7548.

## Step 3 — interpret + act

Final line: `N passed, M failed`.

### Green (`0 failed`)

- Environment OK + tests OK → tell the user "airc is healthy. Run `airc join` to join the substrate."
- Make sure to mention what you fixed (if anything) in step 1.

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
- Form a hypothesis, fix it, re-run that scenario alone (`airc doctor <scenario>`)

## Step 4 — final report

One line: "Fixed X, Y. All tests green." OR "Fixed X. Tests N passed M failed; failures: <list>." Be specific about what you did, not what was found.

## When to run this skill

- Right after install — confirms airc + gh + sshd all aligned before pairing for real.
- After `airc update` — confirms the new binary didn't regress, and that any new env requirements (e.g. gh in #38, gh in #39) are met.
- When something feels wrong — rule out a binary-level regression before blaming network / SSH / human error.
- Before opening an airc issue — paste the doctor output so the maintainer doesn't have to ask.

## Notes

- Scenarios are gh-free; the substrate ITSELF (`airc join` zero-arg, `airc list`) requires gh. That's a feature, not a bug — gh is the comm layer.
- Suite runtime is ~2 minutes for `all`; individual scenarios are 10-30s.
- This skill assumes you can run shell commands. The user should not have to type anything except the interactive `gh auth login` flow if you encounter it.
