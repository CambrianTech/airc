# Grid Substrate Audit

Status: active steering document for `rust-rewrite`.

This audit locks the intended product boundary:

- AIRC is the grid substrate: identity, trust, routes, presence,
  typed envelopes, subscriptions, replay, and transport selection.
- Agents are one consumer class, not the substrate's center.
- Continuum, OpenClaw, Hermes, opencode, Claude, Codex, games,
  live rooms, render nodes, and model hosts all use the same substrate
  surface.
- Consumer vocabularies live above AIRC as typed contracts
  (`forge.*`, `continuum.*`, `openclaw.*`, etc.). AIRC routes by
  headers and subscription filters; it does not understand those
  domains.

The short version: AIRC must be a local-first, tailnet-capable, p2p-
ready event and command substrate that can carry Continuum's room,
persona, model, LoRA, render, game, and tool traffic without becoming
Continuum-specific.

Priority order matters:

1. **Same machine / same user account:** must be boring, fast, and
   automatic. This is the critical path for Codex, Claude, local
   Continuum, local OpenClaw, local Hermes, and per-machine grids.
2. **Tailnet / LAN remote:** the next practical route. We expect
   Tailscale and LAN transports to cover most multi-machine personal
   grids before broader public p2p is required.
3. **Grid-to-grid p2p:** important for multi-human Continuum/OpenClaw
   grids, but later. Do not block local or tailnet reliability on
   global discovery/federation.

## Non-Negotiables

1. `airc join` is the default recovery and live setup verb.
   No required `--attach`, `watch`, `monitor`, or per-runtime flag for
   the normal path.
2. Routine same-machine traffic never depends on GitHub. GitHub is
   invite/rendezvous metadata only.
3. Local route wins when available. Tailnet/LAN routes are next.
   Relay/public p2p routes exist to cross boundaries, not to replace
   the local fast path.
4. No silent fallback. A route may be selected by policy; a fallback
   must be explicit, observable, and testable.
5. Rust substrate is the source of truth. Shell/Python are install or
   migration tools only and should trend to zero runtime ownership.
6. Consumer integrations must be thin. Continuum/OpenClaw/Hermes
   should call `airc-lib`, not parse CLI text or reach into AIRC state
   files.
7. Cursor replay and live push are first-class. Anything visible in
   production should be inspectable, replayable, and debuggable
   without a full UI stack.
8. Code organization must preserve the substrate boundary. If a file
   starts naming consumers, move that surface into an integration
   crate/module.
9. Durable substrate data uses the store/ORM boundary. JSON is allowed
   for wire payload encoding, install/config bootstrap, and external
   invite documents; it is not acceptable for runtime cursors, trust
   state, subscriptions, presence registries, or replay checkpoints.

## Current Drift

The Rust rewrite rebuilt important primitives, but recent work drifted
into agent-shaped seams:

- `airc-cli` owns Codex-specific modules and runtime heuristics.
- Join live-feed decisions currently sit in `commands.rs`.
- Hook/monitor behavior is still easier to reason about as agent
  plumbing than as a general subscription consumer.
- Lifecycle events exist implicitly in logs and status output, not as
  typed events consumers can subscribe to.
- Command/request-response behavior is still a convention, not a small
  SDK primitive.

These are fixable, but they must be fixed as substrate cleanup, not as
more agent patches.

## Phase 1 — Integration Boundary Extraction

Goal: stop making the substrate name Codex/Claude directly.

Work:

- Add `integrations-common` crate/module for runtime detection,
  runtime client identity, store-backed cursor ownership, and hook/feed
  conventions shared by agent integrations.
- Move Codex-specific surfaces out of `airc-cli`:
  - `codex_*.rs`
  - Codex hook JSON/config mutation
  - Codex-specific feed/cursor policy
- Move generic runtime context detection out of `commands.rs`.
  `airc join` should call a small substrate-facing classifier, not own
  env/process heuristics inline.
- Keep `airc-cli` as a thin command surface over `airc-lib` plus
  integration install commands.

Acceptance gates:

- `airc-cli/src/commands.rs` no longer contains Codex/Claude-specific
  runtime marker lists.
- Generic runtime context is represented by a typed enum, not boolean
  soup:

  ```rust
  pub enum RuntimeContext {
      InteractiveTerminal,
      Agent { kind: AgentRuntimeKind, client_id: RuntimeClientId },
      Automation,
      TestHarness,
  }
  ```

- `airc join` behavior is selected from `RuntimeContext`.
- Existing Codex and Claude behavior remains green in public install
  proof tests.

## Phase 2 — Substrate Events And Subscription API

Goal: make lifecycle and room membership observable to every consumer,
not hidden in CLI prose.

Work:

- Add typed lifecycle events:
  - `PeerArrived`
  - `PeerDeparted`
  - `WireEstablished`
  - `WireLost`
  - `RoomJoined`
  - `RoomParted`
  - `SubscriptionAdvanced`
- Add subscription query API on `airc-lib`:
  - `Airc::subscriptions()`
  - `Airc::is_subscribed(ChannelName)`
  - `Airc::default_room()`
  - `Airc::subscription_cursor(SubscriptionId)`
- Make `Airc::open` accept or derive `RuntimeContext`, so consumers
  do not reimplement runtime identity/client-id logic.

Acceptance gates:

- Continuum can subscribe to room/lifecycle events through `airc-lib`
  without shelling out.
- OpenClaw can list joined rooms and presence from typed API calls.
- Agent monitor/feed code consumes the same lifecycle events.
- `airc status` is a renderer over typed state, not an owner of state.

## Phase 3 — Reliability And Error Shape

Goal: make failures explicit and measurable.

Work:

- Split broad error types into typed sub-enums:
  - transport errors
  - trust errors
  - subscription errors
  - store/replay errors
  - runtime context errors
- Add broadcast backpressure metrics:
  - subscriber lag count
  - dropped event count where loss is allowed
  - oldest retained cursor
  - per-subscriber cursor age
- Replace silent ingest warnings with typed diagnostics that can be
  emitted as events and surfaced in `airc doctor`.
- Audit `Mutex`/lock scopes across `.await`; split or narrow where a
  lock crosses IO.
- Reduce avoidable `event.clone()` in hot broadcast paths.
- Add scheduled drains for stale worktrees, stale beacons, stale
  daemons, and stale temp files.

Acceptance gates:

- No routine substrate error is only printed to stderr.
- Consumers can distinguish route unavailable from auth failure from
  replay cursor expired.
- `airc doctor` can report backpressure and stale resource pressure.
- Workspace drain policy is exercised against real `~/.airc/worktrees`.

## Phase 4 — Command Bus Primitive

Goal: support Continuum/Hermes/OpenClaw command traffic without each
consumer reinventing request/reply matching.

This is not a domain command vocabulary. It is a substrate primitive
for correlation, reply matching, deadlines, and cancellation.

Work:

- Add envelope/header conventions for:
  - `airc.correlation_id`
  - `airc.reply_to`
  - `airc.deadline_ms`
  - `airc.command_kind`
- Add `airc-lib` helpers:

  ```rust
  Airc::request(target, headers, body, deadline) -> PendingCommand
  Airc::reply(reply_to, headers, body)
  Airc::cancel(correlation_id)
  Airc::await_reply(correlation_id, timeout)
  ```

- Keep payload semantics consumer-owned:
  - Continuum may define `continuum.lora.invoke`.
  - Hermes may define `forge.hermes.agent_command`.
  - OpenClaw may define UI/user commands.
  - AIRC only handles correlation and delivery.

Acceptance gates:

- A test consumer can issue a command, receive a reply, and replay the
  transcript by correlation ID.
- Timeout and cancellation produce typed events.
- The primitive can carry Continuum's command-bus contract without
  importing Continuum code.

## Phase 5 — Consumer Proofs

Goal: prove this is a grid substrate, not only an agent chat tool.

Required proofs:

- Codex + Claude: two agents coordinate with `airc join` and `airc msg`
  without paste relay.
- Continuum fixture: room/event bus consumes AIRC events and commands
  through `airc-lib`.
- OpenClaw fixture: user/chat surface maps rooms and presence through
  typed subscription APIs.
- Hermes fixture: command orchestration sends request/reply traffic
  through the command primitive.
- Cross-machine fixture: GitHub/gist is used only to publish registry
  metadata; runtime traffic takes local/LAN/tailnet/relay routes.

Proof order:

1. Same-machine Codex/Claude/Continuum/OpenClaw/Hermes shape.
2. Tailscale or LAN multi-machine proof under one operator account.
3. Multi-human grid-to-grid proof after the local/tailnet path is
   stable.

Acceptance gates:

- No proof parses `airc` human prose.
- No proof depends on GitHub for routine same-machine traffic.
- Every proof has replay data suitable for debugging without the
  original UI or runtime process.

## Immediate PR Queue

1. Extract runtime context detection from `airc-cli::commands` into a
   small typed module. Keep behavior identical.
2. Add store-backed runtime cursors so `airc join` and Codex hook
   consumers never replay the whole backlog and never write cursor JSON
   sidecars.
3. Move Codex hook/feed files behind an integration boundary.
4. Add lifecycle event types and subscription query API.
5. Add first command-bus request/reply helper after reviewing
   Continuum's bus contract.

## Open Questions

1. Should `RuntimeContext` live in `airc-lib` or a new
   `airc-integrations-common` crate?
2. Which lifecycle events does Continuum need first for its room and
   persona event bus?
3. Should the command-bus primitive use only headers for correlation,
   or mirror correlation in body for consumer convenience?
4. What is the minimum cross-machine proof before promoting
   `rust-rewrite` toward canary?
