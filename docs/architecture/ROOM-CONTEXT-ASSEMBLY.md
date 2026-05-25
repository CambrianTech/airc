# Room context assembly — budgeted evidence for managers, hooks, and RAG consumers

Closes work card **d3930e42** (P1, first slice).

Anchor in the autonomous-development roadmap (`docs/architecture/AUTONOMOUS-DEVELOPMENT-ROADMAP.md`):
the manager loop and consumer integrations (Continuum,
OpenClaw, Hermes per #1002 / #1003) need to bundle the
relevant room evidence into a bounded payload they can hand to
a prompt, a planner, or a RAG retriever. Doing that against
the raw event store today requires multi-query stitching and
no shared budget discipline. This module gives them one typed
call.

## API

```rust
use airc_lib::{Airc, ContextBudget};

let slice = airc.room_context(ContextBudget {
    max_items: 64,
    max_age_ms: Some(60 * 60 * 1000),
}).await?;
```

Types ship from `airc_lib::room_context`:

- `ContextBudget { max_items, max_age_ms }`
- `ContextSlice { room_id, room_name, assembled_at_ms, budget, items, totals }`
- `ContextItem::{Event, WorkCard, ActiveClaim}` tagged union.
- `ContextTotals` reports `*_seen` vs `*_kept` per type so
  consumers can detect truncation and re-query with a larger
  budget.

The whole tree is `Serialize + Deserialize` so the CLI
(`airc context --json`) emits it verbatim for shell/jq
consumers, and in-process Rust consumers link it as typed
values without parsing.

## CLI

```
airc context --max-items 32 --max-age-ms 3600000
```

Output: one line of JSON, ready for `jq`.

## Determinism

Same store + same budget produces the same slice. Ordering
within the slice:

1. **Events:** descending `lamport` (newest first).
2. **Work cards:** ascending `priority` (P0 first), then
   state bucket (Open → Claimed/InProgress → Blocked →
   Review → Merged → Closed), then `updated_at_ms`
   descending, then `card_id` for tie-breaking.
3. **Active claims:** ascending `claim_expires_at_ms`
   (about-to-expire first — most actionable for managers).

Items are interleaved in the output by type group in that
order, fill-stopping when `max_items` is hit. The
`ContextTotals` field reports what was seen vs kept.

## Scope cut (this PR ships)

- Evidence types: room events, work cards, active claims.
- Budgets: `max_items` (deterministic fill order) and
  `max_age_ms` (drop everything older than the window).
- Deterministic ordering documented + tested.
- CLI: `airc context --json` for shell consumers.
- Integration tests cover: budget invariant, lamport
  ordering, work-card/active-claim inclusion, age cap.

## Explicit non-scope (each is a follow-up card)

- **Token budget.** Needs a tokenizer dependency. Consumers
  that care can apply their own tokenizer over the JSON-
  serialised slice for the first iteration. Adding a typed
  `ContextBudget::max_tokens` is a follow-up once a tokenizer
  primitive ships.
- **PR / CI status integration.** Waits on the local-git +
  pull-request observation primitives (already in
  `airc-work` but not yet exposed as a room-scoped queryable
  surface). Add `ContextItem::PullRequest` then.
- **Roadmap-gap evidence.** Depends on the markdown-sync
  card (`fe57c6fa`) landing first; once doc anchors map to
  cards, those cards already surface through the
  `WorkCard` evidence type.
- **Capability state.** Depends on the capability
  advertisement shape from the Hermes audit (#1003
  follow-ups).
- **Hook prompt-boundary wiring.** Belongs in the
  runtime-planning hooks card 1702d553 — `airc context
  --json` is the surface; wiring it into Codex/Claude
  hooks is the consumer side.

## How consumers use it

- **Manager loop** (card 878cd7cb) — call `room_context`
  each tick with a generous budget; inspect cards + claims
  to decide whether to create/refine/notify/claim.
- **Codex/Claude hooks** at prompt boundary — call
  `room_context` with a tight budget and inject the JSON
  into the system-prompt addendum.
- **Continuum / OpenClaw / Hermes** — link `airc-lib`
  directly per the consumer audits; subscribe to live
  events on top of the slice for delta updates.

## Cross-references

- AIRC structured publish: PR #990 (`airc_lib::publish`).
- AIRC room-scope mutation guard: PR #993 / #1004.
- Manager loop card: 878cd7cb (in flight by Codex).
- OpenClaw consumer audit: PR #1002.
- Hermes consumer audit: PR #1003.
- Autonomous roadmap anchor:
  [`AUTONOMOUS-DEVELOPMENT-ROADMAP.md`](AUTONOMOUS-DEVELOPMENT-ROADMAP.md).
