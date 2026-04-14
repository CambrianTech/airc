---
name: airc:doctor
description: Self-diagnose AIRC. Runs the integration tests to validate that pairing, sending, renaming, and the two-tier scope resolver actually work on this machine.
user-invocable: true
allowed-tools: Bash
argument-hint: "[tabs|scope|all]"
---

# airc doctor

Run this yourself — don't ask the user. It's fast (~30s) and self-contained.

## What it does

Invokes `airc doctor` (a first-class command baked into the binary) which runs the bundled integration suite at `$AIRC_DIR/test/integration.sh`. Verifies:

- **tabs** — two airc processes on one machine with isolated homes + different ports. Covers port override, pairing, bidirectional send, rename propagation, peer-record persistence, and monitor correctness (does the listener actually surface inbound messages).
- **scope** — per-project `$PWD/.airc/` opt-in tier. Covers that local shadows home on name collision and home peers are inherited when local is empty.

The script is idempotent: it cleans up before and after, kills its own processes, removes temp dirs. It does NOT disturb any running airc mesh — it uses `AIRC_HOME=/tmp/airc-it-*` and a non-default port.

## Run

```bash
airc doctor $ARGUMENTS
```

If `$ARGUMENTS` is empty or `all`, both scenarios run. `tabs` or `scope` narrows.

## Read the result

Look for the final line: `N passed, M failed`. Anything other than `0 failed` is a problem. The script prints each failure by name — report them verbatim to the user.

## When to run

- Right after install, before pairing for real
- After an AIRC upgrade, to confirm the new binary behaves
- When something feels off (sends not arriving, peer list looks wrong) — rule out a binary-level regression before blaming the network

## Interpreting failures

- **alpha hosting failed** — port 7548 in use (another airc host is running there), or the `airc` binary isn't on PATH
- **beta join failed** — SSH (Remote Login) isn't enabled on this machine, or a firewall is blocking the TCP handshake
- **monitor did NOT see** — the signed-message-over-SSH path is broken. Check `~/.ssh/authorized_keys` contains the test keys during the run, or trace with `bash -x`
- **scope: local tier shadows home** — the two-tier resolver in the binary is regressed. Upstream bug, not environment
