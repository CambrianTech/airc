---
name: airc:invite
description: Print an invite for your current mesh. Default = join string for an agent. Use --human for a self-contained paste-block a coworker can run in their terminal (includes install one-liner, works even if they don't have airc yet).
user-invocable: true
allowed-tools: Bash
argument-hint: "[--human]"
---

# airc invite

Run this yourself — don't ask the user to do it.

## Pick the right form

| Recipient | Command | Output |
|---|---|---|
| Another agent (Claude/Codex on a different machine, AI in another scope) | `airc invite` | Single join string `name@user@host[:port]#pubkey` |
| A human coworker (Slack DM, email — they may not have airc yet) | `airc invite --human` | Self-contained shell paste-block: install one-liner + connect + first-message hint |

Default to the agent form. Switch to `--human` only when the user names a specific human coworker (Toby, Ike, JJ, Brian, Todd, etc.) or says "send to a human."

## Agent form

```bash
airc invite
```

Prints a single line: the join string for your current mesh.

- **Hosting** → your own join string.
- **Joiner** → the HOST's join string, reconstructed from your saved pairing state. Any joiner can invite others; everyone converges on the same host.

Show it to the user like this:

> "Paste this to the other agent:"
> ```
> /join <the join string>
> ```

**Check the port before pasting.** Format: `name@user@host[:port]#pubkey`. If a port is present (anything other than 7547), the other agent MUST paste it with the port intact. Trimming `:7548` silently pairs them with whoever has port 7547 — possibly a different host on the same Tailscale IP. This has cost hours in production. Call out non-default ports explicitly:

> "Paste this exactly — note the `:7548` port, don't trim it."

## Human form

```bash
airc invite --human
```

(Aliases: `--share-block`, `--for-friend`.)

Prints a multi-line shell-runnable paste-block. The block includes:

1. The canonical curl|bash install one-liner — safe to re-run if they already have airc.
2. `airc join <gist-id>` using the absolute path `~/.airc/src/airc` (PATH may not include it in the same shell that just installed airc).
3. A "say hi" first-message hint that preserves literal `$(whoami)` so it expands on the receiver's shell, not the host's.
4. A clean-exit hint (`airc part`).

The block uses the **raw gist-id**, not the mnemonic — mnemonic resolution is same-gh-account-only; raw gist-id is cross-account-safe (which the human case always is).

Show it to the user like this:

> "Send this entire block to <name> via Slack/email. They paste it into their terminal and they're in:"
>
> ```
> <full paste-block from airc invite --human>
> ```
>
> "(They'll see your messages once they run it. Identity bootstrap will ask them for pronouns/role/bio on first connect.)"

## Failure modes

- `ERROR: Not initialized. Run: airc join` — you haven't paired yet, nothing to share. Run `/join` first.
- `ERROR: Host info missing from config.` (agent form) — pairing state is incomplete. Re-pair: `airc join <original join string>`.
- `ERROR: no published room gist found in this scope.` (human form) — your scope doesn't have a substrate gist. Run `airc join` first to publish one.

## When to use

- Another agent wants to join the conversation → agent form.
- A coworker without airc wants in (and may be on a different gh account, no Tailscale) → human form. This is "Toby's case": pasteable end-to-end onboarding.
- You lost the original invite and need to share it again → either form, depending on recipient.
