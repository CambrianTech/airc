# AIRC Robustness and Integration Plan

**Status:** binding design audit for the Rust rewrite  
**Date:** 2026-05-20  
**Target branch:** `rust-rewrite`

## Purpose

AIRC has to become the reliable substrate that Continuum, OpenClaw,
Hermes, opencode, Codex, Claude, and future grid peers can all use for
chat-shaped messaging, live events, subscriptions, presence, replay, and
work coordination. It cannot remain a shell/Python/GitHub-polling chat
tool with a Rust CLI around parts of it.

This document is the robustness plan for making AIRC work again as a
real system:

- Continuum must be able to replace its chat and event bus with AIRC
  without losing persona inboxes, replay, UI updates, or activity scope.
- OpenClaw must be able to bridge its channel/session model into AIRC
  without adopting Continuum semantics.
- Local peers, LAN peers, tailnet peers, and future Reticulum peers must
  all see one delivery contract. Transport choice is below the consumer
  boundary.
- Runtime behavior must be recordable, replayable, inspectable, and
  fail closed. No silent fallback paths.

## Audit Findings

### What is solid enough to build on

- `airc-core`, `airc-protocol`, `airc-store`, `airc-transport`,
  `airc-daemon`, `airc-lib`, `airc-work`, and `airc-cli` now exist as
  Rust crates.
- Envelopes have the right generic shape: UUID ids, peer/client ids,
  channel, target, headers, opaque body, media refs, and signatures.
- Headers are a `BTreeMap<String, String>` and already support
  header-based filters. This is the correct primitive for routing
  `forge.*`, `continuum.*`, `openclaw.*`, `hermes.*`, and `x-*`
  metadata without parsing encrypted or large bodies.
- `airc-store` has durable append/page/resume semantics over
  `(lamport, event_id)` and channel filtering.
- `airc-lib` now proves a consumer can embed Rust directly, join a
  room, send, subscribe, filter by headers, and replay without shelling
  out.
- CI now runs normal Clippy/test gates plus a production-only strict
  gate that rejects `unwrap`, `expect`, panic macros, todo stubs,
  unimplemented stubs, and wildcard enum matches in library/binary
  code.

### What is still too brittle

- `airc-lib` is still local-fs centered. `send_frame` constructs a
  `LocalFsAdapter` directly, and the in-process subscriber path also
  wires `SignedTransport<LocalFsAdapter>` directly. That proves a
  local demo, not a robust multi-transport event bus.
- `airc-daemon` also owns only local-fs transports today. Its state is a
  `HashMap<PathBuf, SignedTransport<LocalFsAdapter>>`, not a route
  graph, route resolver, or delivery scheduler.
- There is no consumer-facing route contract. Callers cannot ask for
  "send this durable message to this channel with these delivery
  requirements" and receive `Delivered`, `Queued`, `Rejected`, or
  `NoRoute` with an audit reason.
- Presence is not a durable Rust projection. Rooms can show configured
  or historical participants, but they cannot reliably answer "who is
  online, on which endpoint, with which capabilities, and since when."
- Subscriptions are split between live broadcast streams, store queries,
  daemon inbox calls, monitor hooks, and legacy log/polling paths. The
  destination must be one subscription model over the event store plus
  live wakeups.
- `airc-cli` still owns too much policy. It should be a thin command
  frontend over `airc-lib`/`airc-daemon`, not the place where
  integration semantics accumulate.
- GitHub still appears as runtime messaging in older docs and paths.
  It must become bootstrap/mirror/migration only. Same-device and
  same-LAN communication must never depend on GitHub budget.
- There are remaining runtime defaults that should be reviewed even if
  they compile, especially `unwrap_or(...)` in timestamp, limit, route,
  and adapter code. Defaults must come from typed config or explicit
  policy, not local convenience branches.

## Required Architecture

### One Consumer Contract

Consumers should depend on one Rust API surface:

```rust
pub trait AircBus {
    async fn publish(&self, request: PublishRequest) -> Result<DeliveryReceipt, AircError>;
    async fn subscribe(&self, request: SubscribeRequest) -> Result<SubscriptionHandle, AircError>;
    async fn page(&self, request: PageRequest) -> Result<EventPage, AircError>;
    async fn presence(&self, request: PresenceRequest) -> Result<PresencePage, AircError>;
}
```

The consumer does not choose local-fs, LAN-TCP, Tailscale, Reticulum, or
GitHub. It describes the delivery need. The substrate admits, queues, or
rejects.

### Closed Delivery State

```rust
pub enum DeliveryState {
    Accepted { event_id: EventId },
    Delivered { event_id: EventId, route_id: RouteDecisionId },
    Queued { event_id: EventId, reason: QueueReason },
    Rejected { reason: RejectReason },
}

pub enum RejectReason {
    NoHealthyApprovedRoute,
    UnknownPeer,
    UnknownEndpoint,
    SecurityPolicyRejected,
    CapabilityMismatch,
    BudgetExceeded,
    MissingResourceLease,
    UnknownState,
}
```

No transport adapter may turn a failure into success. No consumer should
receive "OK" if the route was merely queued or refused. Queued is a
valid state, but it is not delivered.

### Route Graph, Not Adapter Selection

The runtime needs a route graph:

```rust
pub struct Endpoint {
    pub endpoint_id: EndpointId,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub channels: BTreeSet<RoomId>,
    pub capabilities: CapabilitySet,
    pub last_seen_ms: u64,
}

pub struct RouteEdge {
    pub edge_id: RouteEdgeId,
    pub from: EndpointId,
    pub to: EndpointId,
    pub transport: TransportKind,
    pub health: TransportHealthState,
    pub cost: RouteCost,
    pub role: TransportRole,
}
```

Mixed rooms are normal. A single publish may fan out through local-fs
for same-host endpoints, LAN-TCP for same-LAN endpoints, Tailscale or
Reticulum for remote endpoints, and queue for offline endpoints. This
is still one consumer operation and one transcript.

### Store First, Wake Second

For durable messages and control frames:

1. validate envelope and headers;
2. admit route;
3. append to durable store/outbox;
4. wake live subscribers;
5. dispatch through selected route edges;
6. record delivery outcomes.

Live events may be coalesced or lossy by policy, but the policy must be
visible in headers and delivery class. Interrupts must not get stuck
behind transcript replay, and transcript replay must not be lost because
a live subscriber lagged.

### Presence Is A Projection

Presence is not a static room member list. It is a projection of signed
endpoint events:

- endpoint announced;
- endpoint refreshed heartbeat;
- endpoint capabilities changed;
- endpoint away/back;
- endpoint disconnected or expired;
- endpoint subscribed/unsubscribed from a room/activity.

Continuum can render persona tiles from this. OpenClaw can map it to
session/channel status. Coding agents can use it to know which peers are
actually reachable before routing work.

### Subscriptions Are The Integration Primitive

All consumers should subscribe through the same structure:

```rust
pub struct SubscribeRequest {
    pub channel: Option<RoomId>,
    pub kinds: BTreeSet<TranscriptKind>,
    pub headers_filter: HeaderFilter,
    pub target: TargetFilter,
    pub self_filter: SelfFilter,
    pub cursor: Option<TranscriptCursor>,
    pub delivery: SubscriptionDelivery,
}
```

This replaces bespoke monitor polling, Codex hook cursor files, and
Continuum-specific chat/event listeners. A Codex hook, Claude monitor,
OpenClaw bridge, and Continuum persona inbox differ only in filters and
payload adapters.

## Continuum Integration Shape

Continuum should not query AIRC's SQLite tables and AIRC should not
import Continuum semantics into core crates.

Continuum owns:

- activity definitions;
- persona identity and cognition;
- RAG/working-memory assembly;
- JTAG/EventBridge/GridFrame/LiveKit payload contracts;
- UI projection rules;
- model/resource policy.

AIRC owns:

- envelope id, sender, client, channel, target;
- delivery class and route audit;
- durable transcript and cursors;
- subscription fanout;
- presence projection;
- media/blob pointers;
- replay fixtures.

Recommended headers:

```text
airc.domain              = chat | event | presence | work | live-control
airc.priority            = interactive | normal | background
forge.body_hint          = forge.persona.turn | forge.chat.message | forge.live.signal
continuum.activity_id    = <uuid>
continuum.room_id        = <uuid-or-name>
continuum.persona_id     = <uuid>
continuum.payload_family = jtag | event_bridge | grid_frame | livekit
```

Persona inboxes become AIRC subscriptions:

```text
channel = activity room
headers continuum.activity_id = <activity>
headers forge.body_hint prefix forge.persona.
self_filter = exclude_same_client
cursor = persona-specific durable cursor
```

The RAG/cognition layer receives a batch from the subscription cursor,
coalesces chat updates into one inbox item per activity window, and
records the exact input event ids used for replay. AIRC does not decide
whether a persona speaks.

## OpenClaw Integration Shape

OpenClaw already has the right concepts: channel, account id, thread id,
session key, sender identity, model selection, and session routing. The
AIRC bridge should preserve those as headers, not flatten them into chat
text.

Recommended mapping:

| OpenClaw | AIRC |
|---|---|
| channel name | `openclaw.channel` header |
| account id | `openclaw.account_id` header |
| thread id | `openclaw.thread_id` header |
| session key | `openclaw.session_key` header |
| sender identity | `PeerId` plus `openclaw.sender.*` headers |
| reply/thread binding | `reply_to` plus `openclaw.thread_id` |
| attachment/media refs | `MediaRef` / `airc-blobs` |

OpenClaw should be able to run as:

- an AIRC endpoint representing an OpenClaw gateway;
- a bridge from external channels into AIRC rooms;
- a consumer of AIRC rooms as another channel type;
- a session router that turns AIRC messages into OpenClaw agent
  sessions without losing channel/thread/account metadata.

The first proof should not require OpenClaw to rewrite internals. It
should be a small adapter that converts one OpenClaw inbound message
fixture into an AIRC envelope, persists it, replays it, and converts it
back with the same channel/session identity.

## Robustness Gates

### Gate A: Same-Host Works Without GitHub

Two local peers must:

1. start Rust runtime;
2. discover/pair locally;
3. exchange signed messages;
4. see live presence;
5. replay transcript after restart;
6. do all of this with GitHub unavailable.

### Gate B: Subscription Hub

One daemon must support multiple subscribers:

- CLI monitor;
- Codex hook;
- Continuum bridge;
- OpenClaw bridge;
- work coordinator.

Each subscriber has an independent cursor. Lagging one subscriber must
not block durable delivery or corrupt another subscriber's cursor.

### Gate C: Route Resolver

Given route candidates for local-fs, LAN-TCP, Tailscale/Reticulum, and
GitHub bootstrap, the resolver must:

- reject GitHub for runtime data;
- prefer direct healthy routes;
- queue or reject when no approved route exists;
- record route decisions;
- never silently downgrade security or performance class.

### Gate D: Continuum Bridge Proof

Using production-like Continuum payload fixtures:

1. AIRC carries a chat message into a Continuum activity;
2. AIRC carries a presence/thinking event as coalesced live state;
3. AIRC carries a LiveKit/WebRTC control payload, not media bytes;
4. Continuum reads through subscriptions and replay, not AIRC SQL;
5. persona inbox replay can reconstruct the exact event ids given to
   RAG/cognition.

### Gate E: OpenClaw Bridge Proof

Using OpenClaw channel/session fixtures:

1. channel/account/thread/session metadata survives round-trip;
2. sender identity maps to durable `PeerId` and display headers;
3. reply/thread binding maps to `reply_to` plus headers;
4. attachments become media refs;
5. replay restores the same adapter-visible message.

### Gate F: Failure Injection

CI or local harnesses must simulate:

- local-fs unavailable;
- LAN peer disconnected;
- GitHub rate-limited;
- subscriber lag;
- daemon restart;
- unknown header/body hint;
- unknown transport state;
- oversized body requiring blob lift.

Every case must produce a typed outcome: delivered, queued, rejected, or
failed. No hidden success.

## PR Sequence

### PR 1: Route Contract Foundation

- Add `airc-runtime` or `airc-router` crate.
- Define `PublishRequest`, `DeliveryReceipt`, `DeliveryState`,
  `RejectReason`, `Endpoint`, `RouteEdge`, and `RouteDecisionAudit`.
- Move route policy/resolver out of `airc-lib` into this crate.
- Add tests for GH runtime-data rejection, degraded-route rejection, and
  unknown-state failure.

### PR 2: Transport Registry and Scheduler

- Add a registry that owns adapters behind one runtime contract.
- Replace direct `LocalFsAdapter::new` calls in `airc-lib` and daemon
  send paths with scheduler calls.
- Local-fs remains the first adapter, but it is no longer hard-coded at
  the consumer boundary.

### PR 3: Durable Outbox and Delivery Journal

- Add store tables/entities for outbound work and delivery outcomes.
- `publish` writes an outbox record before dispatch.
- Every route decision records selected edge or refusal reason.
- Daemon restart resumes queued outbound work.

### PR 4: Subscription Hub

- Move live subscriptions into the daemon/runtime as first-class
  durable subscription records.
- Expose independent cursors, self-filtering, header filters, and lag
  telemetry.
- Convert monitor and Codex hook to this subscription API.

### PR 5: Presence Projection

- Add endpoint presence events and a projection API.
- Add `airc-rs presence list` and `airc-lib` presence calls.
- Stop rendering configured peers as "online" unless a fresh endpoint
  says so.

### PR 6: Blob Lift Enforcement

- Enforce body size policy before transport send.
- Lift oversized payloads into `airc-blobs` and attach `MediaRef`, or
  reject if policy disallows lift.
- Add tests proving no oversized frame crosses local-fs, LAN-TCP, or
  gh-gist as inline body.

### PR 7: Continuum Fixture Bridge

- Add a Rust fixture crate or examples under `integrations/continuum`.
- Encode/decode Continuum payload families as opaque bodies with
  headers.
- Prove chat, event bridge, grid frame, and LiveKit control fixtures
  publish/subscribe/replay without Continuum querying AIRC internals.

### PR 8: OpenClaw Fixture Bridge

- Add `integrations/openclaw` fixture adapter.
- Prove channel/account/thread/session metadata round-trips through
  AIRC headers and replay.
- Keep OpenClaw semantics outside AIRC core.

### PR 9: Cross-Machine Direct Transport Proof

- Add Tailscale or Reticulum route discovery/adapter proof.
- Same API as local-fs.
- GitHub may publish bootstrap records, but message delivery must use
  the approved direct route or queue/reject.

## Stop Doing

- Do not add new runtime behavior to Python or shell.
- Do not make consumers shell out to `airc-rs` as their integration
  path.
- Do not add a consumer-specific field to the AIRC envelope when a
  namespaced header will do.
- Do not call `LocalFsAdapter`, `GhGistAdapter`, or `LanTcpAdapter`
  directly from consumer APIs.
- Do not claim a bridge works until a fixture round-trips through
  publish, subscribe, replay, and restart.
- Do not represent online peers from static config.

## Definition Of Working

AIRC is working when:

1. local agents can talk with GitHub disabled;
2. a remote/tailnet agent joins the same room without changing the
   consumer API;
3. Continuum can replace chat/event reads with AIRC subscriptions;
4. OpenClaw can bridge a channel thread into AIRC and back;
5. every delivery has a route decision or refusal audit record;
6. every subscriber has an independent cursor and replay path;
7. every production event can be replayed without the full live system;
8. no runtime Python/shell path owns messaging, subscriptions, queue,
   lane, presence, or transport.
