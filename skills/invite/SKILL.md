---
name: airc:invite
description: "⚠️ Not available in rust-rewrite: there is no `airc invite` verb. The nearest real action is `airc init`, which prints this peer's spec for out-of-band sharing."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc invite

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc invite` verb (and no `--human` paste-block form) in the
> rust-rewrite.

Run this yourself — don't ask the user to do it.

## Nearest real alternative

`airc init` creates or loads the persisted identity and **prints this peer's spec for
out-of-band sharing** — the closest thing to "give another peer what they need to find
me". It is idempotent: repeat runs return the same peer id.

```bash
airc init          # print this peer's spec (idempotent)
airc whois         # this scope's identity card
```

For account-mesh discovery on the same gh account, `airc registry sync` publishes +
refreshes against the gh-gist rendezvous. There is no single command that emits a
ready-to-paste cross-account join string or a human onboarding paste-block.

## When this comes up

- Another agent wants to join → share the peer spec from `airc init`. There is no one-line `airc invite` join string in the rust-rewrite.
- A human coworker without airc wants in → the self-contained `--human` paste-block is not ported; you'd have to assemble install + join steps by hand. Surface this limitation rather than running a dead command.

## Notes

- Do not invent `airc invite` / `airc invite --human` — neither exists in the rust-rewrite.
