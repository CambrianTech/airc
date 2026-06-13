---
name: airc:reminder
description: "⚠️ Not available in rust-rewrite: there is no `airc reminder` verb and no silence-nudge timer. No CLI equivalent exists."
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc reminder

> ⚠️ **Not available in rust-rewrite yet** (TODO: remove this skill or port the
> command). There is no `airc reminder` verb, and the silence-reminder nudge timer
> (`airc reminder 300 | off | pause`) does not exist in the rust-rewrite. There is **no
> CLI equivalent.**

Run this yourself — don't ask the user.

## What there is instead

Nothing maps directly. The rust-rewrite has no per-scope idle-nudge timer surfaced
through the CLI. The closest related verb is `airc monitor`, which only *formats*
monitor events for AI/runtime consumers — it does not arm a silence reminder.

## When this comes up

- "Remind me if the room goes quiet for N seconds" — explain that idle-reminder timing is not a CLI feature in the rust-rewrite. If you need idle awareness, the runtime's own loop (Claude Monitor / Codex poll) is where that lives, not an `airc` subcommand.

## Notes

- Do not invent `airc reminder` — it does not exist in the rust-rewrite.
