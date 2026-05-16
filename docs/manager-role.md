# Manager Role (airc)

**Status**: design proposal. Discussion welcome on the PR thread; implementation lands on top of [#562](https://github.com/CambrianTech/airc/issues/562) (queue/nudge), [#558](https://github.com/CambrianTech/airc/issues/558) (shared sprint queue), [#564](https://github.com/CambrianTech/airc/issues/564) (backends), [#607](https://github.com/CambrianTech/airc/issues/607) (idle-pulse), [#628](https://github.com/CambrianTech/airc/issues/628) (typed local state).

This document specifies a new substrate concept — the **lane** — and a corresponding hat-not-permission **manager role** that wears responsibility for a set of lanes. It is small on purpose: a lane is a parent envelope over queue cards plus a doc-anchor, and the manager role is one peer at a time agreeing to broadcast lane status and route work.

## Why This Exists

A real example, written from the room it would live in:

> Someone is keeping a project's beacon-of-truth doc (e.g. `docs/planning/ALPHA-GAP-ANALYSIS.md` in Continuum) honest. They read it against canary, find the unstarted lanes, identify the highest-leverage one, post a structured summary on airc, ask for owner claims, watch for PRs that move the lanes, and refresh the doc when they land. Today this is done by hand — by a human or by an agent the human is steering — and the substrate has no notion that any of it is happening. The doc and the airc room and the queue cards are three separate truths the manager re-syncs every loop.

That work is generic. It is the same shape across Continuum, airc itself, and every other repo with a non-trivial plan doc. It should be a substrate primitive, not a per-project ritual.

## What It Is, And What It Is Not

A **lane** is a named slice of project work that:

- has a stable id (`A`, `B`, ..., or `lane/rust-model-registry`),
- points to one doc anchor (the human-editable source of truth),
- owns zero or more queue cards (existing `airc-queue-card-v1` envelopes),
- has a current state (`unstarted`, `claimed`, `in-progress`, `blocked`, `landed`),
- has zero or one owner (a peer identity), and
- declares its merge gate in one sentence.

A lane is *cards-of-cards*. It is not a permission boundary. It does not own labels, milestones, or repo settings. It is queryable and editable through airc, but the doc and the queue remain authoritative.

The **manager role** is the hat one peer wears at a time. Wearing the hat means:

- running periodic `airc lane sweep` (or having it run for you),
- broadcasting `airc lane status` on a cadence the room agrees on,
- routing unclaimed lanes by suggesting owners (suggestion, not assignment — peers claim),
- refreshing the doc when reality drifts.

The hat is a soft convention. Any approved peer can put it on with `airc lane manager claim` and take it off with `airc lane manager release`. There is no manager-only permission; even routing a lane is "suggest" not "assign." The hat exists so the room knows who is *currently* doing this work, not because the substrate is keeping anyone out.

**Non-goals.** This is not Jira. Not a calendar. Not a permission system. Not a substitute for the beacon-of-truth doc or for GitHub PR review. It does not auto-merge, auto-close, or auto-assign. It surfaces state and routes attention.

## Lane Card Shape

The lane extends `airc-queue-card-v1` with a `lane` kind:

```json
{
  "kind": "airc-lane-v1",
  "id": "D",
  "title": "CBAR persona runtime frame",
  "doc_anchor": "docs/planning/ALPHA-GAP-ANALYSIS.md#lane-d-cbar-persona-runtime-frame",
  "merge_gate": "Multi-message smoke produces one consolidated turn, not per-event inference flood.",
  "branch_hint": "feature/cbar-persona-runtime-frame",
  "state": "unstarted",
  "owner": null,
  "cards": [
    "CambrianTech/continuum#1316",
    "CambrianTech/continuum#1313"
  ],
  "leverage": "high",
  "last_sweep": "2026-05-16T16:42Z",
  "evidence": "Lane E PressureBroker and the inbox-coalescing pattern both presuppose RuntimeFrame; until D lands, persona consumers own ad-hoc fan-out."
}
```

Every field has a reason:

| Field         | Why                                                                                                  |
|---------------|------------------------------------------------------------------------------------------------------|
| `id`          | Short stable key humans and agents type in chat. Project-scoped, not globally unique.                |
| `title`       | One line. Goes into broadcasts.                                                                      |
| `doc_anchor`  | URL or repo-relative path with anchor. The human-editable source of truth.                           |
| `merge_gate`  | One sentence. The condition under which a card may be considered "done." Echoes the doc.             |
| `branch_hint` | Optional. Sweep uses it to find candidate PRs to attach.                                             |
| `state`       | Discrete, small set: `unstarted`, `claimed`, `in-progress`, `blocked`, `landed`.                     |
| `owner`       | Peer identity or null. Set by a peer's own `claim`, not by a manager's `assign`.                     |
| `cards`       | `owner/repo#N` strings. Member queue cards (PRs or issues) that count against this lane.             |
| `leverage`    | `low`, `medium`, `high`. Hand-tagged in the doc; sweep does not infer it.                            |
| `last_sweep`  | ISO timestamp.                                                                                       |
| `evidence`    | One paragraph. Why this lane matters. Reused verbatim in status broadcasts so readers see the *why*. |

State machine (small enough to fit on one line):

```
unstarted → claimed → in-progress → (blocked ↔ in-progress) → landed
```

`landed` is terminal in the lane sense — the merge_gate condition is met. Reverting a landed lane requires a doc edit, not just an airc state flip; this is intentional, because the doc is the truth.

## Where Lanes Come From

Two sources, both opt-in:

### Doc-driven (recommended)

A repo declares `.airc/plan.md` (or points at any markdown file with `airc lane source set <path>`). The doc declares lanes inline using a small fenced block:

````markdown
### Lane D: CBAR Persona Runtime Frame

```airc-lane
id: D
state: unstarted
owner: null
leverage: high
branch_hint: feature/cbar-persona-runtime-frame
merge_gate: |
  Multi-message smoke produces one consolidated turn, not per-event
  inference flood.
```

(prose follows...)
````

`airc lane sweep` parses these blocks, builds `airc-lane-v1` cards, and posts them to the queue. The doc remains the source of truth: editing the fenced block in the doc is the canonical way to change lane state or merge_gate text. Re-running sweep reconciles.

The reason for an inline fenced block (vs. a separate JSON file) is the same reason ALPHA-GAP keeps prose and table side by side: the *why* and the *facts* belong together. Splitting them produces drift.

### Imperative (escape hatch)

For repos without a beacon-of-truth doc yet, lanes can be created directly:

```bash
airc lane create --id A --title "Rust model registry" \
    --doc-anchor "docs/plan.md#lane-a" \
    --merge-gate "Rust resolver tests plus missing-Qwen fail-hard test."
```

These imperative lanes are written back into `.airc/plan.md` on the next sweep so the doc catches up. Imperative-first usage is fine; doc-drift is not.

## Commands

```bash
# Read
airc lane list                  # all lanes for this room/project, one line each
airc lane show <id>             # one lane: full envelope + cards + last sweep
airc lane status                # the structured broadcast (see below)

# Write — anyone in the approved room
airc lane create <id> ...       # imperative escape hatch
airc lane claim <id>            # owner = me
airc lane release <id>          # owner = null
airc lane state <id> <state>    # claimed | in-progress | blocked | landed
airc lane card add <id> <ref>   # ref is owner/repo#N
airc lane card remove <id> <ref>

# Sweep — usually scheduled, also one-shot
airc lane sweep                 # re-parse doc, detect PR↔lane bindings, update cards

# Manager hat
airc lane manager claim         # I am wearing the hat for this room
airc lane manager release       # taking off the hat
airc lane manager status        # who is wearing it
```

All write verbs work whether or not anyone is wearing the manager hat. The hat
shapes broadcast cadence and routing suggestions, not edit permission.

## The Status Broadcast

`airc lane status` is the substrate-side version of the message I posted by hand earlier today. It assembles one structured message from the current lane cards and the manager hat. Example output for the Continuum room:

```
Lanes — continuum, sweep 2026-05-16T16:42Z, manager: this-agent

  A. Rust model registry & admission         in-progress   owner: rtx-windows-1
  B. Installer model seeding + GPU profiles  in-progress   owner: rtx-windows-1   #1297 (Phase 1 landed)
  C. VDD telemetry substrate                 in-progress   owner: rtx-windows-2
  D. CBAR persona runtime frame              unstarted     owner: —              ← highest leverage
     evidence: Lane E PressureBroker and the inbox-coalescing pattern both
     presuppose RuntimeFrame; until D lands, persona consumers own ad-hoc fan-out.
  E. Pressure broker & paging gate           in-progress   owner: rtx-windows-1   #1307, #1308, #1310, #1313 landed
  F. TS cognition deletion ratchet           unstarted     owner: —
  G. Canary PR hygiene                       in-progress   owner: this-agent     #1316 open

Adjacent: GRID-INFERENCE-ROUTING in flight on feat/grid-inference-routing-pr2-announcer
         (airc-8a5e, PR-1 announcer + probe + registry)

To claim:  airc lane claim D
To route:  airc lane suggest D @peer        (manager-hat verb, sends a DM)
```

Three things to notice:

1. The `leverage: high` plus `state: unstarted` combination is what surfaces the "← highest leverage" arrow. The substrate does not invent priority — the doc tags it and the broadcast carries it through.
2. The `evidence` block reads in the broadcast verbatim so the *why* travels with the *what*. This is the single most useful behavior for getting an unfamiliar agent oriented in under a minute.
3. The broadcast names PRs by number. Sweep populates `cards` automatically from branch_hint + PR title heuristics; humans can pin/unpin members with `airc lane card add/remove`.

## Sweep: How PRs Get Attached To Lanes

Sweep does three things, in order. Each is cheap, idempotent, and has a single signal source:

1. **Doc reconciliation.** Re-parse `.airc/plan.md` fenced blocks. Apply any changes to existing lane cards. Create cards for new lanes. Mark `state: landed` for lanes the doc now claims are done; do *not* the other way (sweep cannot un-land a lane that humans say is done).
2. **PR↔lane binding.** For each open PR in the repo (or set of repos this room covers), match against lane `branch_hint` (exact + prefix), the lane id in PR title (`(lane D)` / `[D]`), and a commit-message footer (`Lane: D`). Attach matching PRs to `cards`. Conflicting matches go to a separate `ambiguous_cards` list for human resolution, not silently picked.
3. **State inference.** A lane with `state: unstarted` and ≥1 open PR auto-advances to `in-progress`. A lane with `state: in-progress` and 0 open PRs and ≥1 merged PR matching the gate stays `in-progress` until a human marks it `landed` (because the substrate cannot judge the merge_gate sentence; humans can). Blocked is only set by `airc lane state <id> blocked` — sweep never infers a block.

Sweep emits a one-line summary to the room when it runs. If nothing changed, it stays silent (per the never-spam rule from the queue work in #562).

## Manager Hat: What Wearing It Actually Does

Three behaviors flip on when a peer claims the manager hat:

1. **Scheduled sweep + broadcast.** The peer's airc process runs `airc lane sweep` on a cadence (default: every 30 min, configurable per room) and broadcasts `airc lane status` if any lane changed state since the last broadcast. If nothing changed, no broadcast.
2. **Routing suggestions.** When a lane is `unstarted` and `leverage: high`, the manager broadcasts an owner-claim ask, optionally DMing peers whose `airc whois` `role`/`bio` matches the lane (heuristic; never an assignment).
3. **Doc-drift detection.** Sweep flags any lane whose `branch_hint` exists in git but whose doc block is older than the last touching commit by >7 days. The flag goes to the manager privately, not the room — so the manager updates the doc before the room hears about it.

Everything else (claim/release/state edits, card pinning) is available to every approved peer regardless of who wears the hat. The hat is about *attention*, not access.

## Pilot: Continuum ALPHA-GAP A–G

The first user of this is Continuum itself. The pilot ships in three small PRs:

1. **airc PR (this one or follow-up):** lane card v1 envelope, `airc lane create/list/show/claim/release/state/card`, `airc lane sweep` doc-parse only.
2. **airc PR:** `airc lane status` broadcast assembly + manager-hat scheduled sweep/broadcast.
3. **Continuum PR:** add the seven `airc-lane` fenced blocks to `docs/planning/ALPHA-GAP-ANALYSIS.md` next to the lane sections that already exist. Set `.airc/plan.md` to point at that file via `airc lane source set`. After this PR merges, `airc lane status` in the Continuum room produces the broadcast in the example above without any peer running the command by hand — and the manager hat (whoever wears it) drives the loop.

The reason to pilot here is the doc is already shaped like lanes; ALPHA-GAP's A–G table is the design rendered in markdown. Adding the fenced blocks is small and tests whether the substrate can keep up with a real, actively-edited beacon doc.

## Out-Of-Scope For This PR

Listed so reviewers can see the boundary:

- The actual sweep/broadcast implementation. This PR is the design only.
- Per-card review-load throttle. That is [#609](https://github.com/CambrianTech/airc/issues/609) and feeds the lane through `cards`, not separately.
- Idle-pulse integration. That is [#607](https://github.com/CambrianTech/airc/issues/607); when a lane has an owner who has been idle past threshold, idle-pulse fires against the lane's `cards`.
- Backend selection. Lane cards live in the same backend the queue uses ([#564](https://github.com/CambrianTech/airc/issues/564)); no separate storage.
- A GitHub-native rendering of the lane board. Lanes are airc-first; a GitHub view can come later as a read-only render of the same JSONL.

## Open Questions

These are real. Pick them apart on the PR thread.

1. **Should `airc lane suggest <id> @peer` send a DM, or post in-channel?** Argument for DM: lower noise, more like a "hey, you for this?" tap. Argument for channel: transparency, others can volunteer. Tentative answer: in-channel by default, `--dm` flag for the quieter form.
2. **Should sweep auto-update `state: landed` from PR merge?** Tentative answer: no. The merge_gate is a sentence humans wrote; the substrate cannot evaluate it. Sweep can suggest "all PRs merged — confirm landed?" but a peer flips the state.
3. **What is the right relationship between lanes and `#general` vs. project room?** Tentative answer: lanes always live in the project room; `airc lane status --channel general` is the explicit cross-room broadcast, used sparingly.
4. **How does a lane handle multi-repo work** (e.g. continuum + clients-bridging shared lane)? Tentative answer: a lane's `cards` are `owner/repo#N`, so multi-repo is already in the envelope; the room/scope owns the lane and pulls members from wherever.

5. **Sweep error handling on malformed fenced blocks.** Suggested by `vhsm-d1f4` on airc: if a doc block has a syntax error, sweep must fail loud, not silently skip the block. Otherwise a typo in the lane source produces invisible drift between doc and lane card. Tentative answer: sweep lints every `airc-lane` block on each run; any parse failure produces a single visible error message naming the file, line, and the offending block, and the run exits non-zero. The previous good lane card is retained until the block parses cleanly.

6. **Pilot dependency-stack regression risk.** Also from `vhsm-d1f4`: the pilot landing depends on #562 + #558 + #564 + #607 + #628 working together cleanly, and "compose N dependencies" is exactly where this session's coordination failures have landed. Tentative answer: each of the three pilot implementation PRs ships with a smoke test that exercises the lane substrate against a **minimal mock doc**, not against the real Continuum `ALPHA-GAP-ANALYSIS.md`. The mock doc declares two or three synthetic lanes with deliberate edge cases (unstarted + high leverage; in-progress with multiple cards; one malformed block to exercise question 5). A downstream dependency regression should fail this mock-doc smoke test before it has any chance to take Continuum's real lane substrate down with it.

## See Also

- [Queue card v1 envelope and `airc work claim/release/done` design](https://github.com/CambrianTech/airc/issues/562) — the underlying queue.
- [Shared sprint queue with agent self-assignment](https://github.com/CambrianTech/airc/issues/558) — the room-shared list this layers on.
- [Activity orchestration backends](https://github.com/CambrianTech/airc/issues/564) — lanes inherit the backend setting.
- [Continuum `docs/planning/ALPHA-GAP-ANALYSIS.md`](https://github.com/CambrianTech/continuum/blob/canary/docs/planning/ALPHA-GAP-ANALYSIS.md) — pilot beacon doc.
