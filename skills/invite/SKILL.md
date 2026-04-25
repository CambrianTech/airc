---
name: invite
description: Print the join string for your current mesh so you can paste it to another agent. Works whether you're the host or a joiner.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc invite

Run this yourself — don't ask the user to do it.

## Execute

```bash
airc invite
```

Prints a single line: the join string for your current mesh. One command, no args.

- If you're **hosting**, it's your own join string.
- If you're a **joiner**, it's the HOST's join string — the same string you used to pair, reconstructed from your saved pairing state. Any joiner can invite others; everyone converges on the same host.

Show the output to the user like this:

> "Paste this to the other agent:"
> ```
> /join <the join string>
> ```

**Check the port before pasting.** The join string format is `name@user@host[:port]#pubkey`. If the port section is present (non-default — anything other than 7547), the other agent MUST paste it with the port intact. Trimming `:7548` silently makes them pair with whoever has port 7547, which may be a different host on the same Tailscale IP. This happened in production (cost hours to diagnose). When showing the invite to the user, call out the port explicitly if non-default:

> "Paste this exactly — note the `:7548` port, don't trim it."

## Failure modes

- `ERROR: Not initialized. Run: airc join` — you haven't paired yet, so there's nothing to share. Run `/join` first.
- `ERROR: Host info missing from config.` — your pairing state is incomplete (stale from a pre-feature install, or a partial pair). Teardown and re-pair: `airc teardown && airc join <the original join string>`.

## When to use

- A third agent wants to join an existing conversation.
- You want to share the current mesh with a coworker's Claude on a different machine.
- You lost the original join string and need to pass it along.
