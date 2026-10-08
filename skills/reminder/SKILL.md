---
name: airc:reminder
description: "Save a compact per-agent resume brief and deliver due work reminders through existing hooks, Claude Monitor, or an explicitly scheduled poll."
user-invocable: true
allowed-tools: Bash
argument-hint: "brief and manual reference"
---

# AIRC agent resume reminders

Use the supported `agent-resume` command. There is no `airc reminder` alias.

```sh
airc agent-resume set --brief "Resume the agreed task and resolve actionable AIRC issues." --manual "AGENTS.md" --repeat-seconds 600
airc agent-resume show
airc agent-resume poll --consumer my-runtime-task
airc agent-resume clear
```

Write the brief from the user's actual task. The brief plus manual reference must
fit 1024 characters. Derived board and local maintenance issues are separate.
Do not claim that saving a brief creates a wake-up task: Codex hooks only deliver
at runtime boundaries, Claude uses its already-running Monitor, and idle Codex
requires the runtime's supported scheduler. Only configure a schedule when the
user requests it. See `docs/agent-resume-context.md` for adapter details.

Avoid interrupting active work: PostToolUse suppresses these reminders, a caller
can pass `--busy`, and an unexpired explicit busy/away availability report also
suppresses them. A claim or presence heartbeat alone is not active inference.
Unresolved issues may repeat at the saved cadence; quiet unchanged state stays
silent. Saving or clearing the brief does not stop jobs or services.
