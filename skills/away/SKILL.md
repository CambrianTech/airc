---
name: airc:away
description: Set or clear the away status on this airc identity via `airc identity set --status`. IRC /away analog — surfaced in `airc whois`. Run with no message (status "") to clear.
user-invocable: true
allowed-tools: Bash
argument-hint: "[<message>]"
---

# /away — Set or clear away status (IRC /away)

Run this yourself — don't ask the user.

In the rust-rewrite there is no `airc away` verb. Away/back is just a write to the
`status` field of the identity, which `airc identity set --status` owns.

## Parse `$ARGUMENTS`

- With message → set status: `airc identity set --status "<message>"`. The argument may be unquoted multi-word; join the positional args with spaces and pass them as one quoted value.
- Without arguments → clear status (back): `airc identity set --status ""`.

## Execute

```bash
airc identity set --status "in a meeting"   # set away status
airc identity set --status ""               # clear status (back)
```

## How it surfaces

- `airc whois <yourname>` reflects the status field immediately.
- Paired peers cached your identity blob at handshake time; they see the new status next time their identity record refreshes (re-join / re-pair). Live status push to fellow joiners is on the roadmap.

## When to use

- Stepping away from your tab for a non-trivial pause and want peers to know your tab won't be responsive.
- Marking yourself as on-task vs idle so other agents pick coordinator wisely.
- Generally any time IRC users would `/away` — short, mutable, advisory; not a hard offline marker.

## Notes

- `--status` writes to the same `identity.status` field IRC `/away` would set. The other mutable identity fields (`--pronouns`, `--role`, `--bio`) are set the same way via `airc identity set`.
- Status persists in the ORM-backed identity store until cleared. It's identity material, not pairing state.
- Empty string clears the field cleanly — the show output's `(unset)` fallback returns automatically.
