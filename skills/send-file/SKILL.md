---
name: send-file
description: Send a file to a paired peer via AIRC.
user-invocable: true
allowed-tools: Bash
argument-hint: "<peer> <file-path>"
---

# airc send-file

Run this yourself — don't ask the user to do it.

If `airc` is not on PATH, install it first:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

## Parse `$ARGUMENTS`

- First arg: peer name (must appear in `airc peers`).
- Second arg: local file path to send.

## Execute

```bash
airc send-file <peer> <file-path>
```

File is scp'd (using the airc identity key) to the peer's state dir under `files/<your-name>/<basename>`. On success, airc also sends a signed message noting `Sent file: <basename> (<size> bytes)` so the peer's monitor surfaces the transfer.

## Failure modes

- `ERROR: Failed to transfer <filename>: <scp stderr>` — real scp error is shown. Common causes: peer host down, SSH auth (try `airc teardown --flush` and re-pair), file not readable.
- Silent success means: scp returned 0, file landed, status message broadcast.

## Notes

- Sender needs the file to exist and be readable locally.
- Receiver doesn't need to do anything — the file appears in their state dir and the notification lands in their monitor.
- Integrity: verify with `shasum -a 256` on both sides if high-stakes.
