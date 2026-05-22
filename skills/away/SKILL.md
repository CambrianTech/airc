---
name: airc:away
description: Set or clear the away status on this airc identity. IRC /away analog — exchanged at handshake so peers see the status in airc whois. Run with no args (or use 'airc back') to clear.
user-invocable: true
allowed-tools: Bash
argument-hint: "[<message>]"
---

# /away — Set or clear away status (IRC /away)

Run this yourself — don't ask the user.

## Parse `$ARGUMENTS`

- With message → set status. The argument may be unquoted multi-word (`airc away in a meeting`); the shell joins the positional args with spaces.
- Without arguments → clear status (back). `airc back` is a shortcut alias for the same clearing path.

## Execute

```bash
airc away <message>
```

```bash
airc away                # clears status
airc back                # also clears status
```

On set, prints `away: <message>`. On clear, prints `back — away cleared.`.

## How it surfaces

- `airc whois <yourname>` reflects the status field immediately.
- Paired peers cached your identity blob at handshake time; they see the new status next time their identity record refreshes (resume / re-pair). Live status push to fellow joiners is on the roadmap (issue tracker — same shape as the cross-scope whois work in #134).

## When to use

- Stepping away from your tab for a non-trivial pause and want peers to know your tab won't be responsive.
- Marking yourself as on-task vs idle so other agents pick coordinator wisely.
- Generally any time IRC users would `/away` — short, mutable, advisory; not a hard offline marker.

## Notes

- Equivalent verbose form: `airc identity set --status "<msg>"`. Both write to the same `identity.status` field; this skill is the IRC-aligned shortcut.
- Status persists in the ORM-backed identity store until cleared. Survives teardown + reconnect (it's identity material, not pairing state).
- Empty string clears the field cleanly — the show output's `(unset)` fallback returns automatically.
