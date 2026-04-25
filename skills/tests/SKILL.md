---
name: airc:tests
description: Run the airc integration test suite (alias for airc doctor). Validates pairing, send, rename, room substrate, scope isolation, queue resilience, and more. Use after install or upgrade, or when something feels off.
user-invocable: true
allowed-tools: Bash
argument-hint: "[scenario|all]"
---

# airc tests

Run this yourself — don't ask the user.

## Execute

```bash
airc doctor $ARGUMENTS
```

Empty `$ARGUMENTS` (or `all`) runs every scenario sequentially. A specific scenario name runs just that one.

## What's in the suite

11 scenarios at last count, all rooted in `test/integration.sh`:

| Scenario | What it proves |
|---|---|
| `tabs` | Two airc processes on one machine pair via inline invite, send bidirectionally, audit-trail to local log, send-file works, rename propagates |
| `scope` | Per-project `$PWD/.airc/` shadows home tier; scoped config doesn't leak |
| `reminder` | `AIRC_REMINDER` interval persists, heartbeat fires after silence, `airc reminder off/<n>` controls |
| `teardown` | Host killed, port freed, state preserved without `--flush`; scope isolation (one teardown doesn't kill another tab's host) |
| `resilience` | Wire failures don't drop messages — local mirror + `[QUEUED]` marker + `pending.jsonl` for retry |
| `reconnect` | Stale pidfile recovered, host re-spawn against same scope works |
| `queue` | `pending.jsonl` drains automatically when host returns |
| `status` | `airc status` reports liveness correctly, `--probe` runs SSH check |
| `auth_failure` | Bad key gives clear "re-pair required" error, NOT silent queue forever |
| `resume_stale_auth` | Resume detects stale SSH key at probe time, dies loud with reconstructed invite string |
| `room` | #39 IRC substrate — `--room <name>` + cmd_part host/joiner detection + `room_name` state file |

## How to read output

Final line: `N passed, M failed`. `0 failed` means green. Failures print by name above the summary; report them verbatim to the user.

## When to run

- Right after install — `airc tests` to confirm the binary works on this machine before pairing for real.
- After `airc update` — confirm the new binary didn't regress.
- When something feels wrong — rule out a binary-level bug before blaming network / gh / SSH.
- After local edits to `airc` script — fast feedback loop instead of pairing manually.

## Common failure → diagnosis

Most failures fall into a small set:

- **`alpha host failed to start`** — `airc` not on PATH, or port 7549 (test-reserved) is taken. Check `lsof -iTCP:7549`. Killing whatever holds it usually fixes.
- **`beta join failed`** — SSH Remote Login isn't enabled on this machine, OR a firewall is blocking TCP to 7549. macOS: System Settings → General → Sharing → Remote Login.
- **`scope: local tier shadows home` / `home tier inheritance`** — the two-tier resolver in airc itself is regressed. Usually a recent edit broke `ensure_init` / `get_config_val`. Bisect against last known green sha.
- **`teardown in different scope killed foreign host`** — scope isolation broke. Critical (one tab nuking another's session). File issue immediately.
- **`alpha cmd_part DID NOT identify as host`** — cmd_part's host-vs-joiner detection regressed. The signal is `config.json::host_target` empty = host. Don't fall back to gist_id presence — that was the original bug (#39 PR2).

## Notes

- Suite uses port 7549 (test-reserved) and `AIRC_HOME=/tmp/airc-it-*`. Won't touch live airc on default 7547 or alt 7548.
- All scenarios are `gh`-free — they use the inline invite handshake, not the gist transport. (This is by design; tests must run in CI without GH credentials.)
- The room scenario uses `--no-gist --room <unique-name>` to exercise IRC-substrate flag plumbing without polluting the user's gh gist namespace.
- Cleans up via pidfiles after itself — no broad `pkill` hammers. If something hangs, `airc teardown --all` is the manual recovery.
