---
name: airc:reminder
description: Control the silence-reminder nudge. Fires once when no send happens within N seconds; resets on next send.
user-invocable: true
allowed-tools: Bash
argument-hint: "<seconds|off|pause>"
---

# airc reminder

Run this yourself — don't ask the user.

## Execute

```bash
airc reminder 300      # nudge after 300s of silence (default)
airc reminder 0        # disable
airc reminder off      # disable
airc reminder pause    # temporarily disable without losing interval
```

A background timer inside the monitor fires exactly once per silence period. When the user next sends, the "reminded" marker clears and the timer re-arms.

## When to use

- You want less/more prodding from the system during idle windows.
- Tuning a long-running collaboration session where silence is normal.

## Notes

- Reminder text surfaces via the monitor as `[ts] airc: Reminder: ...` — same channel as all other airc events.
- Default interval is 300s (5 min). Set at host time (persisted via handshake to joiners) or locally via this command.
