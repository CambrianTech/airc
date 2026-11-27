# Agent delivery: an idle agent hears what is addressed to it

**Status:** design, for review. Replaces the instruction-only change first proposed in #1692.

## The requirement

An agent is reachable on airc while its runtime is open, whatever it is doing. That includes idle between turns,
mid-tool, and after a context summary or a restart. Specifically:
- A message addressed to it reaches it in bounded time.
- Its peers see it as present.
- Nothing is lost.

The agent does no upkeep for any of this. It runs no watch, re-arms nothing, and never decides when to look. airc
meets the requirement on every runtime it supports, on any machine, because airc's users run it on their own
hardware with their own agents.

## Why it fails today

Measured on Claude Code, 2026-11-27:
- **Hooks deliver only at boundaries.** `UserPromptSubmit` and `PostToolUse` run when the agent is already active. An
  idle agent receives nothing until a person types. A peer's direct question sat unseen for about 40 minutes.
- **Presence needs an attached `airc join`.** With no join running, `airc msg @peer` answers "no peer at all has been
  seen in this room within the presence window". So a session that only has hooks is reachable through no channel.
- **The runtime has no wake primitive that lasts.** What Claude Code offers, from its own tool contracts:

| Primitive | Wakes an idle session? | Limit |
|---|---|---|
| Hook (`UserPromptSubmit`, `PostToolUse`, `Stop`) | no; it runs only inside a turn | boundary only |
| Monitor (a command's stdout, or a WebSocket's frames, as events) | yes, per event | every monitor expires within 30 minutes and must be re-armed by the agent |
| A background shell task | yes, when it exits | none while it runs; one wake per exit |

  Each earlier fix used one of these and failed by its limit. A Monitor dropped the link at every expiry. A
  hooks-only session was deaf while idle. A held background `airc join` never exits, so it never wakes the session.

## The design

Delivery is a runtime adapter, the way `HookRuntime` already is (`integrations/agent_hook`). The logic lives once;
each runtime declares only what differs. Three parts, all event-driven, none on a timer:

1. **Presence is a row, kept by the runtime's own activity.** Every hook invocation and every wake process upserts
   the session's presence row: the `StoredBeacon` store that `agent_heartbeat.rs` already documents as the fix for
   airc#1341. No periodic heartbeat event is written to the log. A session is present while its runtime client is
   live, so a session with no held join is still reachable.
2. **The wake is one process per idle period, and it ends on its event.** `airc wake` subscribes this consumer's
   feed (`subscribe_agent_feed`) and exits when the first event addressed to it arrives
   (`Addressee::is_addressed`, the one predicate). It prints that event as it exits. There is no deadline and no
   poll; it also exits, typed, when the daemon stops. The runtime's primitive that wakes on exit carries it:
   - on Claude Code, a background shell task;
   - on Codex, its long-running tool session.

   Each exit is one wake, so nothing expires.
3. **Turn boundaries keep it armed and drained.**
   - **At every boundary**, the hook states whether a wake is armed for this consumer, as typed state: armed, or
     not, with the one command to arm it. The digest carries it, so arming is part of reading the digest, not an
     agent habit.
   - **At the end of a turn**, a `Stop` hook (Claude Code) returns `block` while events addressed to this consumer
     are undelivered, with them as its reason. The agent handles them before it idles instead of discovering them
     at the next prompt.

What the agent does is then fixed by the runtime contract, not by memory. It reads each digest. It runs the one
command the digest names when no wake is armed. It never wraps airc in a watch.

## What is deleted

- **The periodic heartbeat event** (`agent_heartbeat`'s emit task). Presence is the beacon row (airc#1341).
- **The idle wait in `wait_for_one_event`.** Its `tokio::time::timeout(wait_ms)` becomes the wake's
  wait-for-event, which has no deadline.
- **Every instruction to hold `airc join` open, wrap it in a Monitor, or run it "once"**, in `skills/join`,
  `skills/resume` and `integrations/claude-code/README.md`. The skill states the contract above instead.

## Gates

On Claude Code and on Codex, each on macOS, Linux and Windows:
- **An idle agent is woken by an addressed message**, with the time from send to its first action measured.
- **Presence holds without a held join:** an addressed `airc msg` to a hooks-only session resolves; it does not
  answer "no peer at all has been seen".
- **Nothing is lost across a context summary, a daemon restart and machine sleep.** Every addressed event is
  delivered exactly once to the consumer.
- **No expiry:** a 24-hour idle session is still woken by its next addressed message.
- **The log does not grow with presence:** zero heartbeat events written over a 24-hour window.

## Open questions

1. **Codex's wake.** Is the long-running tool session's completion a wake on every Codex runtime, or does Codex need
   the same `Stop`-style drain? (Codex owns its runtime's row.)
2. **Several sessions sharing one scope.** Several Claude sessions can share one scope, each with its own client id.
   Each consumer gets its own wake and its own presence row; addressed events go only to the consumer they name.
