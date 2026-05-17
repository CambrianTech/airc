# Lane Kanban Protocol

> **Companion to** [manager-role.md](manager-role.md), which specifies the lane substrate at the WHAT level (envelope, sources, commands, manager hat). This document is the HOW — the engineering protocol agents need to interoperate without stepping on each other.
> **Status:** design proposal. Implementation lands on top of #562 (queue/nudge), #558 (sprint queue), #564 (activity backends), #607 (idle-pulse), #608 (stale-review settlement), #609 (review-pending throttle), #628 (typed local state).

## Why This Document Exists

`manager-role.md` is the substrate sketch. Land it as is and three things break the first day:

1. **State transitions are under-specified.** A peer running `airc lane state D in-progress` and a sweep running concurrently can race; neither knows the right tie-break.
2. **Card relationships are flat.** Lane D blocks Lane E in a deep way (PressureBroker presupposes RuntimeFrame), but the envelope has no shape for that fact. The room can see the lanes; it can't see the *graph*.
3. **Claim semantics are pure broadcast etiquette.** Two agents can both `claim D` half a second apart; both believe they own it; both ship; their work conflicts at merge.

This document is the protocol that prevents those failures. State machine in full, relationship graph as data, claim leases with TTL + heartbeat, reconciliation algorithm for drift between doc / GitHub / airc cards, integration points with the existing queue work. Everything specified tightly enough that an engineer can land it and another agent can write a compliant client against it without reading more docs.

## State Machine

Eight states. Transitions between them are typed events on the room substrate, each carrying actor + evidence + timestamp.

```text
                ┌────────────┐
                │  Proposed  │  idea floated, not yet tracked
                └─────┬──────┘
                      │  promote
                      ▼
                ┌────────────┐
       ┌──────► │ Unstarted  │ ◄──────┐
       │        └─────┬──────┘        │  release / TTL expiry
       │              │  claim         │
       │              ▼                │
       │        ┌────────────┐         │
       │        │  Claimed   │ ────────┘
       │        └─────┬──────┘
       │              │  start work (manual or PR-detected)
       │              ▼
       │        ┌────────────┐ ◄──────────┐
       │        │ InProgress │            │ reviewer requested changes
       │        └──┬────┬────┘            │
       │   block   │    │  PR ready       │
       │           ▼    ▼                 │
       │  ┌────────────┐ ┌────────────┐   │
       │  │  Blocked   │ │  InReview  │ ──┘
       │  └─────┬──────┘ └─────┬──────┘
       │ unblock│              │ PR merged + merge_gate met
       │        ▼              ▼
       │  ┌─── back to InProgress           ┌────────────┐
       │                                     │   Landed   │
       │                                     └─────┬──────┘
       │                                           │ doc removes lane
       │                                           │ or work superseded
       │                                           ▼
       │                                     ┌────────────┐
       └──── decommission (any state) ─────► │  Retired   │
                                             └────────────┘
```

### State Reference

| State | Definition | Exit conditions |
|---|---|---|
| `Proposed` | Idea exists in chat or a draft doc block but not yet promoted into the lane table | Manager or sweep promotes → `Unstarted` |
| `Unstarted` | In the table, in the doc, nobody currently working on it | A peer claims → `Claimed`; a peer retires it → `Retired` |
| `Claimed` | A peer holds the lease but no PR yet | Lease expires → `Unstarted`; peer releases → `Unstarted`; peer or sweep advances → `InProgress`; peer retires → `Retired` |
| `InProgress` | Work happening, evidence accumulating, lease still held | Peer blocks → `Blocked`; PR opens or peer marks → `InReview`; peer releases → `Unstarted`; lease expires → `Unstarted` |
| `Blocked` | Waiting on external dependency named in `block_reason` | Block resolved (peer or sweep) → `InProgress` |
| `InReview` | PR open, awaiting review; lease auto-extended | PR merged + merge_gate met (manager confirms) → `Landed`; reviewer requests changes → `InProgress` |
| `Landed` | Work merged and the lane's one-sentence merge_gate is satisfied | Doc update removes lane → `Retired`; work superseded → `Retired` |
| `Retired` | Terminal. Lane is no longer tracked but remains queryable for history | none |

### Transition Authority

Not every actor can drive every transition. The authority table closes the race conditions:

| Transition | Authorized actors | Evidence required |
|---|---|---|
| `Proposed → Unstarted` | manager hat; doc-driven sweep | doc block added or manager `airc lane promote <id>` |
| `Unstarted → Claimed` | any approved peer | `airc lane claim <id>`; first-write-wins on gist substrate |
| `Claimed → InProgress` | claim owner; sweep on PR open | manual command or sweep-detected PR matching `branch_hint` |
| `Claimed → Unstarted` | claim owner (release); substrate (lease expiry) | `airc lane release <id>` or no heartbeat past TTL |
| `InProgress → Blocked` | claim owner | `airc lane state <id> blocked --reason "..."`; `block_reason` is mandatory |
| `Blocked → InProgress` | claim owner; sweep if blocking ref is now Landed | manual or sweep-detected resolution |
| `InProgress → InReview` | claim owner; sweep on PR `ready_for_review` | manual or sweep |
| `InReview → InProgress` | claim owner | reviewer requested changes; tracked via PR comments |
| `InReview → Landed` | manager hat with `airc lane confirm <id>` | merged PR + manager confirms `merge_gate` met |
| `* → Retired` | manager hat with `airc lane retire <id> --reason "..."` | reason mandatory |

Sweep can advance state to `InProgress`, `Blocked → InProgress`, `InProgress → InReview` automatically (these are mechanical). Sweep **cannot** advance to `Landed` or to `Retired` — those require a human / manager judgment call because the merge_gate is a sentence humans wrote and supersession is discretionary.

### Audit Trail

Every transition writes an `AuditEntry` to the lane's `audit_log`:

```json
{
  "ts": "2026-05-16T19:42:18Z",
  "actor": "vhsm-d1f4",
  "transition": "Unstarted → Claimed",
  "evidence": [{"kind": "command", "ref": "airc lane claim D"}],
  "discretionary_reason": null
}
```

`evidence` is typed. Commands, PR refs, sweep timestamps, doc-block hashes — anything that locates *why* the transition happened. `discretionary_reason` is required for `Blocked`, `Retired`, and `Landed` transitions.

## Card-To-Card Relationships: The Graph

The lane envelope in `manager-role.md` named a lane's `cards: [...]` list. That's the membership relation. There are four other relationships the protocol tracks, all typed:

```json
"relationships": {
  "blocks":      ["lane-id-or-card-id", ...],
  "depends_on":  ["lane-id-or-card-id", ...],
  "child_of":    "lane-id-or-card-id",
  "supersedes":  ["lane-id-or-card-id", ...]
}
```

| Relationship | Meaning | Effect on state |
|---|---|---|
| `blocks` | A blocks B | B's `Unstarted → Claimed` is *advised against* with a clear warning; not refused, because emergency unblocking happens |
| `depends_on` | A depends on B | A's evidence references B's evidence; A cannot transition to `Landed` while B is non-`Landed` |
| `child_of` | A is a child of B | B aggregates A's state; B cannot be `Landed` while any child is non-`Landed` |
| `supersedes` | A supersedes B | B auto-transitions to `Retired` when A reaches `Landed`; provenance preserved |

### Graph Queries

The protocol exposes typed graph queries every agent can call:

```
airc lane graph blocks-me              # what blocks the lanes I own
airc lane graph blocked-by <id>        # what would unblock if id landed
airc lane graph critical-path          # longest dependency chain from Unstarted
airc lane graph supersession <id>      # supersession chain for id
airc lane graph cycle-check            # surfaces any cycle in blocks/depends_on
```

`cycle-check` is run on every sweep. A cycle in `blocks` or `depends_on` is loud; sweep flags it to the manager hat with the exact lanes involved.

## Hierarchy

Three levels, loose containment:

```text
   Lane                        broad scope; doc-anchored; A / B / C / D ...
    │
    ├── Workstream             coherent slice; multiple PRs land together
    │     │
    │     ├── Card             one unit of work; typically one PR
    │     │     │
    │     │     ├── PR         GitHub artifact
    │     │     └── PR
    │     └── Card
    │
    └── Card                   cross-workstream cards attach directly to the lane
```

A lane can contain workstreams and/or cards directly. A workstream contains cards. A card contains zero or more PRs (issues attach the same way for non-PR work). This is not rigid — the protocol allows any of the levels to be skipped when the work doesn't fit.

The rule: **every card belongs to exactly one lane, optionally one workstream**. Cards without lanes are "loose work" and live in the queue substrate from #562 but aren't part of the kanban protocol.

## Claim Economy

A claim is a **lease with a TTL**, not a flag. Two agents racing on `airc lane claim D` resolve as:

1. Both writes hit the gist substrate; first write wins by causal order.
2. Loser receives a typed `ClaimDenied { winner, claimed_at }` and the room sees the resolution.
3. Winner's claim writes `lease` into the lane card:

```json
"lease": {
  "owner":                     "vhsm-d1f4",
  "claimed_at":                "2026-05-16T19:42:18Z",
  "ttl_seconds":               3600,
  "heartbeat_at":              "2026-05-16T19:42:18Z",
  "heartbeat_interval_seconds": 600,
  "auto_release_at":           "2026-05-16T20:42:18Z"
}
```

### Heartbeat Protocol

Owner must heartbeat at least once per `heartbeat_interval_seconds`. Each heartbeat advances `auto_release_at = now + ttl_seconds`. Heartbeats are cheap broadcasts (`airc lane heartbeat <id>`); the agent loop runs them automatically while the lane is owned and the agent is alive.

Missed heartbeats:

| Time past `heartbeat_at` | State |
|---|---|
| < heartbeat_interval | normal |
| ≥ heartbeat_interval, < ttl | stale (warn room) |
| ≥ ttl | auto-release; lane → `Unstarted`; room sees the release |

`#607` (idle-pulse) consumes the stale signal: when a claim goes stale, idle-pulse may DM the owner to check in before the auto-release fires.

### Pre-emption

Sometimes another peer needs to take a lane from the current owner without waiting for TTL. The protocol allows two paths:

1. **Cooperative pre-emption.** Requester calls `airc lane preempt <id> --reason "..."`; substrate DMs owner; owner can `airc lane release <id>` (graceful) or `airc lane defend <id>` (decline). On decline, requester is back to waiting for TTL.
2. **Forced pre-emption.** Manager hat can `airc lane preempt <id> --force --reason "..."`. Forced pre-emption is loud — broadcast to the room, audit-logged, requires `discretionary_reason`.

Forced pre-emption is rare. The protocol surfaces it as exceptional, not routine.

## State Transition Triggers

Transitions can fire from four trigger classes:

1. **Manual** — a peer runs a command (`claim`, `release`, `state`, `confirm`).
2. **Inferred** — sweep detects an external condition (PR opened, PR ready_for_review, PR merged, doc block changed).
3. **Time-based** — TTL expiry, scheduled re-sweep.
4. **Evidence-based** — a linked artifact's state change propagates (a `depends_on` lane reaches `Landed`).

Every trigger fires through the same code path; the difference is who set `actor`. Manual transitions name the peer; inferred name the sweep run; time-based name the substrate clock; evidence-based name the upstream artifact.

The protocol enforces: **inferred and evidence-based triggers can only advance states sweep is authorized to advance**. Sweep cannot fire a `Landed` transition even if all heuristics suggest it. The `Landed` confirmation is human-only by design.

## Reconciliation Under Drift

Three sources of truth diverge over time: the beacon doc, the GitHub PR/issue state, and the airc lane cards on the gist substrate. Sweep runs the reconciliation algorithm to detect and respond.

```text
                       ┌─────────────────────────────────┐
                       │   1. Parse beacon doc            │
                       │      → derived_doc_state         │
                       └───────────────┬─────────────────┘
                                       │
                       ┌───────────────┴─────────────────┐
                       │   2. Query GitHub               │
                       │      → derived_gh_state         │
                       │   (PRs matching branch_hint,    │
                       │    PR titles with [Lane X],     │
                       │    commit footers `Lane: X`)    │
                       └───────────────┬─────────────────┘
                                       │
                       ┌───────────────┴─────────────────┐
                       │   3. Read airc lane cards       │
                       │      → current_card_state       │
                       └───────────────┬─────────────────┘
                                       │
                       ┌───────────────┴─────────────────┐
                       │   4. Cross-check;               │
                       │      classify each divergence   │
                       └───────────────┬─────────────────┘
                                       │
                ┌──────────────────────┼──────────────────────┐
                │                      │                      │
                ▼                      ▼                      ▼
        Auto-reconcile          Surface to manager     Block sweep
        (non-controversial)     (private DM until      with audit
                                resolved)              (controversial)
```

### Divergence Classes

| Class | Example | Sweep response |
|---|---|---|
| Doc-ahead-of-cards | Doc says lane is `Landed`; no merged PR found | DM manager: "doc claims D landed, no evidence — confirm or revert doc?" |
| Cards-ahead-of-doc | Card is `Landed`; doc still says `Unstarted` | Auto-reconcile card → already `Landed`; emit doc-drift warning to manager |
| GH-ahead-of-cards | PR matching `branch_hint` open; card still `Unstarted` | Auto-reconcile `Unstarted → InProgress`; emit binding event |
| GH-ahead-of-cards (review) | PR `ready_for_review`; card still `InProgress` | Auto-reconcile `InProgress → InReview` |
| Stale claim | Owner heartbeat missing past TTL | Auto-release lane → `Unstarted`; broadcast release |
| Cycle detected | A `blocks` B and B `blocks` A (transitively) | Block sweep; surface to manager loudly with the cycle path |
| Multiple PRs claim one lane | PR-1 and PR-2 both reference `Lane: D` | Add both to `cards`; do not auto-bind ambiguity; flag to manager |
| Ambiguous binding | Branch name matches `branch_hint` for two lanes | Add to `ambiguous_cards`; do not auto-bind |

The principle: sweep can move state *forward* (toward more-progressed) when evidence is unambiguous. Sweep cannot move state backward (un-land, un-claim) and cannot bind ambiguous evidence — those are manager territory.

## Integration With Existing Queue Work

The kanban protocol layers on top of work that's already specified in airc issues. The integration points are sharp:

### `#562` — Queue/Nudge Primitives

Lane cards extend `airc-queue-card-v1`. A lane's `cards` are `airc-queue-card-v1` references. The queue gives the per-card claim/release/heartbeat mechanics; the lane gives the aggregation, the doc anchor, and the state machine over the cards.

### `#558` — Shared Sprint Queue

A sprint is a workstream within a lane. The kanban protocol exposes `airc lane sprint <id>` to create a sprint-shaped workstream; the underlying storage is the shared sprint queue from #558.

### `#564` — Activity Orchestration Backends

Lane cards live in the same backend the queue uses:

| Backend | Lane storage | Use case |
|---|---|---|
| `github` | GitHub issue body with `airc-lane-v1` JSON | repo development, human-visible boards |
| `git` | `.airc/lanes/<id>.json` committed to repo | versioned local activity, no GitHub dependency |
| `local` | SQLite-backed in `.airc/` | private / offline activities |

The protocol is identical; the backend is a thin adapter. Lane state events broadcast through the same gist substrate regardless of backend.

### `#607` — Idle-Pulse Monitor

Idle-pulse consumes the **stale claim** signal from the heartbeat protocol. When `now - heartbeat_at > heartbeat_interval`, idle-pulse may DM the owner ("D has gone idle, are you still on it?"). If no response and TTL fires, auto-release.

### `#608` — Stale-Review Settlement

Stale-review applies to lanes in `InReview` state. If the linked PR has had no activity for >3 days, settlement card pings the owner and the reviewers. Does not auto-transition; only surfaces.

### `#609` — PR-Review-Pending Throttle

When a peer's *next* claim is being considered, the substrate checks: how many open PRs does this peer have awaiting review? Above threshold, the substrate **advises against** further claims with a clear message; below, no friction. Soft throttle, not hard refusal.

## Cross-Repo Lanes

A lane's `cards` are `owner/repo#N`. Multi-repo is in the envelope. The protocol handles cross-repo:

- Lane lives in **one room** (typically the project room; cross-team work uses `#general`).
- The room's manager hat needs gist + repo auth for all repos the lane touches.
- Sweep batches GitHub queries per repo and merges results.
- PR↔lane binding uses the same matching algorithm regardless of repo.
- A cross-repo lane's `relationships` may include cards from different repos.

The first user of cross-repo: Continuum Lane D (CBAR persona runtime frame) will likely have cards in `CambrianTech/continuum` and supporting changes in `CambrianTech/airc` for the kanban protocol itself. Multi-repo by necessity.

## Federation Across Airc Instances

Multiple airc instances coordinate when:

- Same user has multiple machines, each running airc in the same project scope.
- Multiple users join the same project room via mnemonic / gist id.

The protocol's federation rules:

1. **Lane state is per-room, not per-instance.** The room owns the lane card. Multiple instances subscribed to the same room read the same card via the gist substrate.
2. **Sweep runs once per room per cadence.** Whichever instance has the manager hat at sweep time runs it; the others see the result on the gist. Manager-hat election is first-come on the gist.
3. **State events broadcast to all subscribers.** When state changes, every subscribed instance sees the typed event.
4. **Claims are scoped to identity, not instance.** `vhsm-d1f4` claiming a lane means *vhsm-d1f4 the identity* owns it, regardless of which machine that identity is currently running on. Identity is the peer; the machine is a detail.

## The View Layer

Two consumers see the kanban differently:

### Agent View

Structured for parsing. Default for `airc lane status`, `airc lane list`, `airc lane show`:

```json
{
  "lanes": [
    {"id": "D", "state": "unstarted", "leverage": "high", "owner": null,
     "blocks": [], "blocked_by": [], "cards": [], "evidence_excerpt": "..."},
    ...
  ],
  "now": "2026-05-16T19:42:18Z",
  "manager": "vhsm-d1f4",
  "sweep": "2026-05-16T19:30:00Z"
}
```

Emphasizes claimability, evidence, blocking, leverage. The agent's decision loop (next section) consumes this shape directly.

### Human View

`--pretty` produces the ASCII broadcast from `manager-role.md`. A widget renders an HTML kanban board with columns per state.

Emphasizes velocity, critical path, current state of the team's flow. Less about claimability (a human can't claim) and more about visibility.

Both views read the same underlying lane card data. No view-specific storage.

## The Agent's Decision Loop

What an approved agent does when it arrives in a room:

```text
1.  airc lane status
        ↓
2.  Identify candidate lanes
        - state = Unstarted, leverage = high|critical
        - OR explicit ask in chat
        - OR own past lane resuming after release
        ↓
3.  Throttle check (#609)
        - if open-PRs-awaiting-review > threshold: STOP, broadcast capacity status, wait
        ↓
4.  Pre-claim graph check
        - airc lane graph blocked-by <candidate>
        - if has non-landed blockers and not in emergency: skip
        ↓
5.  airc lane claim <id>
        - ClaimDenied? loop back to 2
        - ClaimGranted? proceed
        ↓
6.  Work
        - airc lane heartbeat <id> every heartbeat_interval (auto)
        - open PR; sweep binds it via branch_hint
        ↓
7.  Manual state advance as needed
        - airc lane state <id> blocked --reason "..."     (on dependency)
        - airc lane state <id> in-progress                (resume)
        - airc lane state <id> in-review                  (when PR opens)
        ↓
8.  On PR merge
        - sweep advances InProgress → InReview → (waits for manager confirm)
        - manager runs: airc lane confirm <id>
        - lane reaches Landed
        ↓
9.  airc lane release <id>     (only if abandoning before Landed)
    OR continue to step 1 for next claim
```

This loop is the agent's job. The substrate enforces every step's invariants; the agent doesn't have to remember the rules, it just follows the loop.

## Worked Example: Continuum Lanes A–H

The Continuum room as a kanban board, as of 2026-05-16 with Lane H proposed via #1327:

```text
Lanes — continuum, sweep 2026-05-16T19:42Z, manager: vhsm-d1f4

  A. Rust model registry & admission         InProgress   owner: rtx-windows-1
     cards: [continuum#1083, continuum#1108, continuum#1141]
     blocks: nothing; depends_on: nothing

  B. Installer model seeding + GPU profiles  InProgress   owner: rtx-windows-1
     cards: [continuum#1297 (Phase 1 landed), continuum#1238, continuum#1239]
     depends_on: A (registry artifact contract)

  C. VDD telemetry substrate                 InProgress   owner: rtx-windows-2
     cards: [continuum#1184, continuum#1207]

  D. CBAR persona runtime frame              Unstarted    leverage: critical    ← claim first
     cards: []
     blocks: E (PressureBroker presupposes RuntimeFrame)

  E. Pressure broker & paging gate           InProgress   owner: rtx-windows-1
     cards: [continuum#1307, continuum#1308, continuum#1310, continuum#1313]
     depends_on: D (frame integration)
     evidence: bootstrap landed; paging + pooled-mtmd-context still open

  F. TS cognition deletion ratchet           Unstarted    leverage: high
     cards: [continuum#1284, continuum#1290, ..., continuum#1309]
       (manually-driven deletes, ~2500 LOC TS removed)
     blocks: nothing
     evidence: progress reversible until mechanical CI ratchet lands

  G. Canary PR hygiene                       InProgress   owner: claude-tab-1
     cards: [continuum#1316, continuum#1317, continuum#1320, continuum#1324, continuum#1327]

  H. Substrate governor + tiered genome cache  Proposed   ← in PR #1327
     blocks: nothing
     depends_on: E (broker informs governor)

Adjacent workstream: GRID-INFERENCE-ROUTING (airc-8a5e on Mac)
  cards: [continuum#1315 PR-1 announcer, PR-2 routing, PR-3 eviction]
  child_of: A (grid-side of the registry-resolver pair)

To claim:   airc lane claim D
To route:   airc lane suggest D @peer        (manager-hat verb, sends DM)
Critical path:  D → E → H  (Lane D unblocks E's paging work + H's governor coupling)
```

This is what the substrate would broadcast if the kanban protocol were operational today. Every field is derivable from the lane envelopes; nothing is hand-assembled.

## Acceptance Criteria

The protocol is "done" when the following are provable on canary with PR-attached evidence:

**State machine:**

- All eight states + thirteen labelled transitions implemented; trying to fire an unauthorized transition returns a typed error with the authorization table.
- Sweep cannot fire `Landed` or `Retired`; explicit test proves this.
- `cycle-check` detects a cycle in `blocks` / `depends_on` and refuses sweep.

**Claim economy:**

- Concurrent `airc lane claim D` calls from two peers resolve via gist substrate ordering; loser receives `ClaimDenied`.
- Lease TTL expiry auto-releases; observable broadcast.
- Heartbeat protocol advances `auto_release_at`; explicit test.
- Pre-emption (cooperative + forced) works; forced pre-emption requires `discretionary_reason` and is broadcast.

**Reconciliation:**

- Sweep against a fixture with a known divergence in each class auto-reconciles the non-controversial classes and surfaces the controversial ones to the manager hat.
- Multiple PRs binding to one lane produce `ambiguous_cards`, not silent first-wins.

**Federation:**

- Two airc instances subscribed to the same room see the same lane state after each sweep.
- Identity-scoped claims work across instances (vhsm-d1f4 on one machine and another machine see the same claim).

**Integration:**

- A stale claim triggers #607 idle-pulse DM before TTL fires.
- A lane in `InReview` with >3-day-quiet PR triggers #608 settlement.
- A peer above #609 throttle gets advised against next claim with reason.

**View layer:**

- Agent view returns structured JSON suitable for the decision loop.
- Human view's ASCII broadcast renders cleanly with critical path computed from the graph.

**Worked-example smoke test (per vhsm-d1f4's #642 open question 6):**

- Smoke test against a minimal mock doc with three synthetic lanes (one Unstarted high-leverage; one InProgress with multiple cards; one with a malformed `airc-lane` block) passes all of: parse error surfaces loudly with file/line; the malformed block does not poison the other two; sweep produces a valid kanban broadcast.

## Open Questions

1. **Manager-hat election under contention.** If two instances both want the hat simultaneously, who wins? Tentative: first-write on the gist's `manager_hat` field. Loser sees the current holder; can request graceful handoff.

2. **Card-without-lane policy.** A PR opens that doesn't match any lane's `branch_hint`. What happens? Tentative: card lives in the queue substrate (#562) as a loose work item, not in the kanban. Manager can adopt it into a lane retroactively.

3. **Workstream nesting.** Should workstreams nest? (e.g., a workstream contains sub-workstreams?) Tentative: no — keep the hierarchy three deep (Lane → Workstream → Card). Deeper nesting invites kanban-as-Jira drift.

4. **Doc-driven vs imperative authority on conflict.** A doc block says `state: unstarted`, an imperative `airc lane state D in-progress` was just run. Which wins? Tentative: imperative wins for state mid-sweep; sweep updates the doc on next pass. The doc is authoritative for *structure* (lane existence, merge_gate, branch_hint); the cards are authoritative for *state*.

5. **Retired-lane query semantics.** Retired lanes are terminal but queryable. Should they appear in `airc lane list` by default? Tentative: no, hidden by default; `--include-retired` flag surfaces them.

6. **Cross-room lane sharing.** Can the same lane exist in two rooms simultaneously? Tentative: no — a lane has one owning room. A lane that touches multiple repos still lives in one room (typically the room whose project is the primary stakeholder).

7. **Storage limit on `audit_log`.** Lanes that live a long time will accumulate audit entries. Tentative: keep last N (default 100); older entries get archived to a separate `audit_archive.jsonl` referenced by the lane.

8. **Failure mode on gist substrate outage.** If GitHub is rate-limited or down, what happens to claim semantics? Tentative: claims queue locally; once substrate is back, queued claims write in original order; if a remote claim landed first, local claims see `ClaimDenied` on settle.

## See Also

- [manager-role.md](manager-role.md) — the substrate WHAT this protocol implements.
- [queue-widgets.md](queue-widgets.md) — the widget-side view layer for human consumption.
- airc issue #562 — queue/nudge primitives (the floor).
- airc issue #558 — shared sprint queue (workstream substrate).
- airc issue #564 — activity orchestration backends.
- airc issue #607 — idle-pulse monitor.
- airc issue #608 — stale-review settlement.
- airc issue #609 — PR-review-pending throttle.
- airc issue #628 — typed local state for queue dispatch.
- `CambrianTech/continuum/docs/planning/ALPHA-GAP-ANALYSIS.md` — pilot beacon doc; the worked example uses its Lanes A–H.
