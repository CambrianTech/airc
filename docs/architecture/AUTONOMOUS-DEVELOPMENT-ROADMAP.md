# Autonomous Development Roadmap

Status: strategic roadmap for turning AIRC from agent chat into the
generic coordination substrate for low-friction, low-cost autonomous
development.

## North Star

AIRC is the room/event/work substrate. It is not Continuum-specific,
Claude-specific, Codex-specific, or GitHub-specific. It gives humans,
agents, apps, and grid nodes a shared signed event stream with rooms,
subscriptions, trust, replay, work cards, claims, routes, and typed
evidence.

Continuum, OpenClaw, Hermes, opencode, Codex, Claude, forge-alloy, and
sentinel-ai are consumers above that substrate. They should be thin at
their edges: each speaks typed contracts over AIRC instead of owning a
separate chat bus, work queue, monitor loop, or ad hoc coordination
protocol.

The long-term economic goal is practical: make development mostly
local/grid-owned and increasingly cheap. Instead of paying for every
increment of agent labor, we should be able to run and improve
cooperative dev/persona agents on reasonable hardware, then use
Continuum grid resources, forge-alloy contracts, sentinel scoring, and
LoRA training to make those agents better.

## Layer Model

1. **AIRC substrate**
   - Identity, rooms, trust, event store, replay cursors, routes,
     work cards, claims, roster, availability, and manager
     recommendations.
   - Owns generic transport and delivery semantics.
   - Never imports Continuum, Hermes, OpenClaw, Codex, or Claude
     domain policy.

2. **Contract layer**
   - forge-alloy / sentinel-ai define schemas, capability contracts,
     validation rules, scoring inputs, and replay semantics.
   - Contracts name things such as `forge.work.*`,
     `forge.persona.*`, `forge.hermes.*`, `openclaw.thread.*`, and
     `continuum.lora.*`.
   - AIRC routes by room and headers; consumers validate and interpret
     the body.

3. **Capability hosts**
   - Continuum owns personas, LLMs, LoRA collections, model paging,
     inference routing, activity state, media/avatar state, and
     cognition replay.
   - Hermes owns orchestration workflows and tool/agent commands.
   - OpenClaw owns user-facing workspace/chat/thread surfaces.
   - opencode/Codex/Claude are runtime adapters for agent work.

4. **Training and scoring**
   - AIRC records typed evidence: cards, claims, heartbeats, reviews,
     CI state, stale work, direct questions, handoffs, idle time,
     manager recommendations, and completed work.
   - sentinel-ai and Continuum score that evidence.
   - LoRA/persona training uses the evidence to improve developer,
     reviewer, manager, and coordinator agents.

## Continuum Direction

Continuum should become Rust-owned for runtime-critical coordination.
TypeScript should shrink toward UI, generated bindings, and thin
presentation logic.

Target shape:

- AIRC replaces Continuum chat transport and room event fan-out.
- AIRC subscriptions replace bespoke chat/event polling where the
  semantics are generic: room messages, presence, subscriptions,
  WebRTC signaling/control, persona turn requests, command events, and
  replay cursors.
- Continuum Rust owns the stateful runtime: persona orchestration,
  RAG/cognition replay, LoRA/model paging, capability registry, media
  control, and grid resource policy.
- TS consumes Rust/AIRC-backed typed projections and renders the UI.
  It should not be the source of truth for room membership, event
  replay, persona inboxes, work queues, or distributed inference state.

This is how Continuum gets slimmer: remove duplicate TS buses and
Postgres-shaped chat dependencies where AIRC's SQLite/ORM-backed
event substrate already provides the durable local/grid primitive.

## Runtime Planning Integration

Runtime-specific "planning modes" are adapters, not substrate
primitives.

AIRC should expose runtime-neutral events:

- `forge.plan.requested`
- `forge.plan.proposed`
- `forge.plan.accepted`
- `forge.plan.rejected`
- `forge.work.generated`
- `forge.work.recommended`
- `forge.manager.nudge`

Claude can render those through Monitor. Codex can consume them
through `UserPromptSubmit`, `airc codex-hook poll --wait-ms N`, and a
long-running `airc join` feed until Codex has true wake-on-event
runtime support. Future runtimes should implement the same contract
without AIRC learning their private planning APIs.

The important rule: planning produces typed work events in the room.
It must not stay as prose in a chat transcript.

## Work Flywheel

Every active room should be able to sustain work without a human
typing "what next?"

Loop:

1. Roadmap, issue, PR, and integration adapters observe candidate
   work.
2. The manager projection creates or updates room-scoped work cards
   only when evidence supports them.
3. Live agents publish availability.
4. `WorkManagerStatus` recommends claimable work, stale-claim
   recovery, backlog seeding, or waiting.
5. Agents claim cards, allocate `~/.airc/worktrees/...` leases, work,
   heartbeat, and publish PR/CI/review events.
6. Merged work closes cards and creates follow-up cards when gaps are
   discovered.
7. sentinel-ai and Continuum score the loop for training and
   operational improvement.

Idle time is signal. If a subscribed ready agent sits idle while the
room has claimable work, that is a manager/scoring event, not a hidden
failure.

## Integration Roadmap

### Phase 1: Make Agent Coordination Boring

- `airc join` is the default operating surface.
- Codex and Claude both receive work/plan events without paste relay.
- Work cards, claims, roster, availability, PR state, and CI state are
  typed events.
- No command consumer parses human stdout.

### Phase 2: Bind Real Consumers

- Continuum: replace chat/event reads with AIRC subscriptions and
  replay; move runtime coordination toward Rust.
- OpenClaw: bridge channel/thread/user presence onto AIRC rooms.
- Hermes: issue and correlate agent commands over AIRC request/reply.
- opencode/Codex/Claude: consume the same planning/work events as
  runtime adapters.

### Phase 3: Prove Throughput and Routes

- Same-machine and LAN/Tailnet proofs for chat, commands, and
  work-coordination events.
- Continuum-shaped pose/avatar/persona event benchmark with p50/p99
  latency and drop-rate assertions.
- WebRTC datachannel and UDP route proofs for live/control traffic.
- GitHub remains an invite/PR/work-source adapter, not routine
  message transport.

### Phase 4: Contract Registry and Capability Routing

- forge-alloy publishes schemas and compatibility rules.
- Peers advertise capabilities: models, LoRAs, tools, GPUs, render
  slots, media paths, and workspace capacity.
- Requests route by capability and room, not hard-coded model IDs or
  machine names.
- Failure is explicit when no capability satisfies the request.

### Phase 5: Scoring, Training, and Better Agents

- sentinel-ai computes team and agent scores from typed evidence.
- Continuum records development sessions as replayable room data.
- LoRA/persona training uses successful and failed coordination traces.
- Manager personas learn to seed cards, recover idle agents, request
  reviews, and close loops without human prompting.

### Phase 6: Grid Economics

- Local-first execution is the default.
- Tailnet/LAN/relay/WebRTC routes make multiple machines feel like one
  substrate.
- Continuum shares inference/model/persona capability across the grid.
- Reasonable owned hardware can carry more of the development loop,
  reducing dependence on paid hosted agents.

## Non-Negotiables

- No consumer-specific semantics in AIRC core.
- No stdout parsing as an integration contract.
- No shell/Python sidecars for runtime truth.
- No GitHub-as-message-bus for routine traffic.
- No hidden fallback states.
- No direct SQL access by production consumers; use typed SDK APIs.
- Every generated room/work/context event must have replayable typed
  evidence.
- Every long-lived allocation needs a drain path.

