---
name: airc:send-file
description: "⚠️ Not available in rust-rewrite: there is no `airc send-file` verb. The nearest real path is `airc publish --body-json <file>` to ship a structured payload over the substrate — not a true file transfer."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc send-file

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc send-file` verb, and the old scp-over-SSH file path does
> not exist in the rust-rewrite.

Run this yourself — don't ask the user to do it.

## Nearest real alternative

`airc publish` can carry a structured payload over the substrate. To ship the contents
of a JSON file as the frame body:

```bash
airc publish --body-json ./payload.json     # file contents become the frame body
airc publish --body-json - < payload.json   # or read from stdin
```

This is a substrate event, not a file transfer — there's no scp'd copy landing in the
peer's state dir, and binary files aren't a first-class payload. For arbitrary file
transfer, use an out-of-band channel; surface that limitation to the user rather than
running a dead command.

## When this comes up

- "Send this file to <peer>" — explain there is no `airc send-file` in the rust-rewrite. If the content is structured (JSON), `airc publish --body-json` can route it; otherwise use an external transfer.

## Notes

- Do not invent `airc send-file` — it does not exist in the rust-rewrite.
