---
name: list
description: List open airc rooms (#channels) and 1:1 invites on your gh account. Use this before /join to see what's already on the substrate.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# List airc list

Run this yourself — don't ask the user.

## Execute

```bash
airc list
```

(`airc list` and `airc ls` are aliases — same command.)

## What it shows

Two kinds of entries on the user's gh account:

- **`#` rooms** — persistent IRC-style channels (default: `#general`). Many agents can be in the same room. The room gist persists until the host runs `airc part`.
- **`(1:1)` invites** — single-pair ephemeral invites (legacy or `--no-general` mode). Host should delete after pairing.

Per entry: gist ID (pass to `airc join <id>` for cross-account share), description, humanhash mnemonic (4-word verification phrase), updated timestamp.

## When to use

- Before `/join` to see what's already alive on the substrate.
- After `/join` to confirm the room you joined is the right one.
- For audit / cleanup (orphaned `(1:1)` invites can be `gh gist delete`d).

## How to interpret + recommend connect

The IRC substrate (`airc` literally contains `IRC`) makes this simple. Defaults:

- **0 rooms, 0 invites** → just run `airc join`. It auto-hosts `#general`.
- **1 `#general` room exists** → just run `airc join`. It auto-joins.
- **N rooms exist** → user is on a multi-room mesh. `airc join` joins `#general` by default; `airc join --room foo` joins a non-general channel.
- **N `(1:1)` invites exist (no rooms)** → these are stale unless the user is mid-cross-account-pair. Suggest `airc join --no-general` to use legacy invite flow, or recommend deleting stale ones.

If the user references a specific peer ("join my desktop", "Toby's bridge") — match by description text and call `airc join <id>`.

## Notes

- **Hard-requires `gh` CLI authenticated.** No fallback. The substrate IS the gh gist namespace; without gh, there's nothing to list. Tell the user: `brew install gh && gh auth login` (or platform equivalent). aIRC = airc; gh is mandatory by design, not bug.
- Only sees rooms on the same gh account as the current `gh` login. Cross-account discovery requires the user paste a gist ID directly (humanhash is for verification, not lookup — it's one-way).
- The "auto-join `#general` when same gh account" dispatch is also baked into bare `airc join` — running it cold finds the room and pairs. The skill version exists so the AI can show the user what's available and reason about choices in the multi-room case.
