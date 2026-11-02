# Agent resume context (card dbce8408)

AIRC can save a private brief for the current agent and supply current actionable
work separately through existing runtime delivery boundaries:

```sh
airc agent-resume set --brief "Finish owned work, coordinate reviews, preserve active jobs, and report delivery evidence." --manual "AGENTS.md" --repeat-seconds 600
airc agent-resume show
airc agent-resume poll --consumer my-scheduled-task
airc agent-resume clear
```

The complete saved brief, including `Manual/skill: ...`, is limited to **1024
Unicode characters**. Overlength input is rejected, never truncated silently.
AIRC-derived issues do not share that budget. Output lists at most five owned
cards and five available cards, clips titles to 160 characters, and links the
remaining work to board commands. The board is the current room's projection.
Local maintenance reports managed checkout names and current-repository branch
names matching terminal card IDs. It is an inspection hint, not permission to
delete anything. Unknown branches are not attributed to an owner by guessing.

The brief and delivery state use the existing scoped-state ORM facade under this
peer's private User scope. Two runtimes sharing an AIRC peer identity also share
and can overwrite its brief; use distinct agent identities for distinct briefs.
Consumer keys isolate delivery only. These are not broadcast messages. Each runtime/session
has separate delivery state. Writes and stdout flush precede acknowledgment;
failed delivery remains due. The minimum interval is 60 seconds, the default is
10 minutes. Unresolved issues may repeat at that cadence. An unchanged brief
alone on a quiet board does not repeat. Queue and maintenance reads happen only
when evaluation is due, not on every hook invocation.

## Existing adapters

- **Codex:** UserPromptSubmit includes due context. PostToolUse is an active-work
  boundary and suppresses this reminder without consuming it. Existing peer
  message delivery continues. A due brief replaces the redundant generic work
  summary in that hook, while keeping unread peer context.
- **Claude Monitor:** `airc monitor attach` checks due context on a 60-second
  interval inside its existing process. It escapes board content in a distinct
  monitor envelope. No additional daemon is started. The monitor cannot observe
  inference itself: an unexpired explicit `airc work availability --state busy`
  or `--state away` report suppresses reminders. Expiry permits reminding again;
  a claim alone never means that the agent is actively working.
- **Scheduled runtime:** a user-authorized Codex heartbeat or another existing
  scheduler can call `airc agent-resume poll --consumer <stable-task-id>` and act
  on the output. `--busy` explicitly suppresses that call when the adapter knows
  the runtime is working. Creating or scheduling such a task is outside this CLI.

This does **not** wake an idle Codex task by itself, restore exhausted tokens, or
prove that an agent read or acted on delivered text. It is disabled until a brief
is explicitly saved. Use one reader per consumer key, as with runtime cursors.
No live agent configuration or scheduled task is changed by installing the code.
