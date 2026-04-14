---
name: airc:doctor
description: Self-diagnose AIRC. Runs the integration tests to validate pairing, send, rename, send-file, reminder heartbeat, teardown scope isolation, and the two-tier home/local resolver.
user-invocable: true
allowed-tools: Bash
argument-hint: "[tabs|scope|reminder|teardown|all]"
---

# airc doctor

Run this yourself — don't ask the user. It's fast (~45s) and self-contained.

## What it does

Invokes `airc doctor`, which runs the bundled integration suite at `$AIRC_DIR/test/integration.sh`. 31 assertions across 4 scenarios:

- **tabs** — two airc processes on one machine with isolated homes + port override. Pairing, bidirectional send, monitor correctness, rename propagation, peer-record persistence, send-file with `-i` key, local outbound mirror (audit trail).
- **scope** — per-project `$PWD/.airc/` opt-in tier. Home peers inherited when local is empty; local shadows home on name collision.
- **reminder** — `AIRC_REMINDER` env var, interval persisted, heartbeat fires after silence, `reminded` marker prevents spam, `airc reminder off/<n>` controls.
- **teardown** — host killed, port freed, state preserved without `--flush`; teardown in a different scope does NOT kill unrelated hosts (scope isolation).

The script uses **port 7549** (test-reserved) and `AIRC_HOME=/tmp/airc-it-*`. It will NOT touch any live airc session running on the default 7547 or the common alt 7548. Cleans up after itself via pidfiles — no broad process kills.

## Run

```bash
airc doctor $ARGUMENTS
```

Empty or `all` runs all 4 scenarios sequentially. `tabs`, `scope`, `reminder`, `teardown` run one.

## Read the result

Final line: `N passed, M failed`. `0 failed` means green. Otherwise the suite prints each failure by name — report them verbatim.

## When to run

- Right after install, before pairing for real
- After an upgrade, to confirm the new binary behaves
- When something feels off — rule out a binary-level regression before blaming the network

## Interpreting failures

- **alpha hosting failed** — `airc` not on PATH, or port 7549 taken (rare; port 7549 is test-reserved). Check `lsof -iTCP:7549`.
- **beta join failed** — SSH Remote Login isn't enabled on this machine, or a firewall is blocking TCP to 7549.
- **send/monitor did NOT see** — the signed-message-over-SSH path is broken. Check `~/.ssh/authorized_keys` has the test keys mid-run, or trace with `bash -x`.
- **scope: local tier shadows home** — the two-tier resolver in the binary is regressed. Upstream bug, not environment.
- **teardown in different scope killed foreign host** — scope isolation regression. Critical — would mean one Claude tab can nuke another's live session. File an issue.
