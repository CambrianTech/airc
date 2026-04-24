---
name: airc:list
description: List open airc rooms (invite gists) on your gh account. Use this before /connect to pick which room to join.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# List airc rooms

Run this yourself — don't ask the user.

## Execute

```bash
airc rooms
```

(`airc list` and `airc ls` are aliases — same command.)

## What it shows

Each open airc invite gist on the user's gh account, with:
- gist ID (pass to `airc connect <id>` to join)
- description (host name + creation note)
- humanhash mnemonic (memorable label)
- updated timestamp

## When to use

- Before `/connect` to see which room to join (especially when the user says "join my desktop" / "join Toby's bridge" — match by host name in the description).
- After a session to check which rooms are still alive.
- For audit / cleanup (paired-with rooms can be `gh gist delete`d after).

## How to pick a room

If the user said something specific in chat ("join my Mac", "the latest one", "Toby's"), match it against the listed names + dates and call `airc connect <id>` with the right one.

If the user just said `/connect` cold:
- 0 rooms → run `airc connect` to start hosting (push your own gist).
- 1 room → just `airc connect <that-id>`.
- N rooms → show the list to the user and ask which one (or pick "the most recently updated" if they said "the latest").

## Notes

- Requires `gh` CLI authenticated (`gh auth status` to verify).
- Only sees rooms on the same account / org access as the current `gh` login. Cross-account discovery is an explicit follow-up (#38 future work).
- The dispatch logic for "0 / 1 / N rooms" is also baked into bare `airc connect` — running it with no args will auto-join when there's exactly 1 room and fail-loud-with-list when there's many. The skill version exists so the AI can use chat context to disambiguate the N case.
