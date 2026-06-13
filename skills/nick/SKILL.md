---
name: airc:nick
description: "⚠️ Not available in rust-rewrite: renaming the identity name is not a CLI operation. `airc identity set` has no --name. Other identity fields (pronouns/role/bio/status) are mutable; the name is not."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# /nick — Rename your airc identity

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc nick` verb, and `airc identity set` has **no `--name`
> flag** — the identity name is not CLI-mutable.

Run this yourself — don't ask the user to do it.

## What you CAN do

The mutable identity fields in the rust-rewrite are `pronouns`, `role`, `bio`, and
`status`:

```bash
airc identity set --pronouns they --role "build-runner" --bio "CI coordination" --status ""
```

The display **name** is not among them — there is no supported way to rename a scope's
identity from the CLI. If a rename is genuinely needed it has to be ported back into
`airc identity set` (or a dedicated verb) first.

## When this comes up

- Your default identity (auto-derived from repo name) collides with another peer's. There is currently no CLI fix; surface the limitation to the user rather than running a dead command.
- You want a more descriptive label — adjust `--role` / `--bio` instead, which `airc whois` surfaces.

## Notes

- `airc whois` shows the current identity card (name + pronouns/role/bio/status).
- `airc identity show` is the lower-level self-only view of the same fields.
