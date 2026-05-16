# Realtime Event Bus

**Status:** design contract for airc#626
**Builds on:** [`rust-sqlite-substrate.md`](rust-sqlite-substrate.md)

## Goal

AIRC should provide the generic realtime mechanics needed by chat, presence,
agent coordination, and media control:

- subscribe
- fan out
- replay
- acknowledge
- filter self echoes
- coalesce noisy ephemeral state
- measure latency and dropped events
- bridge across local, tailnet, GitHub compatibility, and future mesh transports

AIRC should not define Continuum's domain packet hierarchy. Continuum already
has canonical command/event packages, and AIRC should adapt them.

## Existing Continuum Schemas

The AIRC realtime layer must preserve these package shapes when it carries
Continuum traffic:

- `JTAGRequest<T>` and `JTAGResponse<T>` in
  `src/workers/shared/jtag_protocol.rs`
- `JTAGMessage<T>`, `JTAGEventMessage<T>`, `JTAGRequestMessage<T>`, and
  `JTAGResponseMessage<T>` in `src/system/core/types/JTAGTypes.ts`
- `EventBridgePayload` in
  `src/system/events/shared/EventSystemTypes.ts`
- `GridFrame` and `GridPayload::{Command, CommandResult, Event, StreamChunk}`
  in `src/workers/continuum-core/src/modules/grid/frame.rs`
- `BridgeCommand` and `BridgeEvent` in
  `src/workers/livekit-protocol/src/lib.rs`

These are payload contracts. AIRC adds transport-neutral delivery mechanics
around them.

## Layer Split

AIRC owns:

- event append/replay/cursors
- subscriptions and fanout
- receipts and acknowledgements
- self-filtering by `client_id`
- backpressure, queue depth, retry policy, and dropped-event telemetry
- ORM-backed SQLite indexes for replayable events
- bounded memory state for ephemeral presence
- adapter traits for local IPC, GitHub compatibility, tailnet/grid links, and
  future transports

Continuum owns:

- command names, event names, persona/media semantics, and UI projection rules
- JTAG/EventBridge/GridFrame/LiveKit payload definitions
- when typing, thinking, speaking, or in-call state should be emitted
- which events become persona memory, room state, activity state, or UI state
- its own ORM-backed projections; it must not query AIRC SQLite tables directly

The adapter owns:

- mapping a source payload family into AIRC delivery metadata
- validating enough structure to route safely
- preserving the original payload bytes or canonical JSON
- returning the same source payload family to the consumer

## Realtime Envelope

The AIRC envelope is metadata, not a replacement schema:

```rust
pub struct RealtimeEnvelope<P> {
    pub event_id: EventId,
    pub payload_family: PayloadFamily,
    pub payload: P,
    pub scope_id: ScopeId,
    pub room_id: RoomId,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub correlation_id: Option<String>,
    pub delivery: DeliveryClass,
    pub ttl_ms: Option<u64>,
    pub coalesce_key: Option<String>,
    pub occurred_at_ms: u64,
}

pub enum PayloadFamily {
    AircNative,
    ContinuumJtag,
    ContinuumEventBridge,
    ContinuumGridFrame,
    ContinuumLiveKit,
}

pub enum DeliveryClass {
    Durable,
    EphemeralLatest,
    EphemeralWindow,
    RequestResponse,
    StreamChunk,
}
```

Examples:

- chat messages: `Durable`
- receipts: `Durable`
- typing/thinking/presence: `EphemeralLatest` with a TTL and coalesce key
- LiveKit room commands and participant events: `Durable` control-plane records
- audio/video frames: not AIRC payloads; they stay on WebRTC/LiveKit media paths
- stream progress chunks: `StreamChunk` with bounded replay policy

## Subscription Traits

The Rust core should expose subscription mechanics separately from transport
adapters:

```rust
pub trait EventBus {
    fn publish<P: Serialize>(&self, envelope: NewRealtimeEnvelope<P>) -> Result<EventId>;
    fn subscribe(&self, request: SubscribeRequest) -> Result<SubscriptionId>;
    fn poll(&self, subscription: &SubscriptionId, limit: u32) -> Result<Vec<StoredEnvelope>>;
    fn ack(&self, ack: Receipt) -> Result<()>;
}

pub trait SubscriptionStore {
    fn create(&self, request: SubscribeRequest) -> Result<SubscriptionId>;
    fn resume(&self, subscription: &SubscriptionId) -> Result<SubscriptionCursor>;
    fn advance(&self, subscription: &SubscriptionId, cursor: SubscriptionCursor) -> Result<()>;
    fn lag(&self, subscription: &SubscriptionId) -> Result<SubscriptionLag>;
}

pub trait PresenceStore {
    fn set_latest(&self, presence: PresenceUpdate) -> Result<()>;
    fn current(&self, room: &RoomId) -> Result<Vec<PresenceState>>;
    fn expire_before(&self, now_ms: u64) -> Result<u64>;
}

pub trait RealtimeTransport {
    fn send(&self, envelope: &StoredEnvelope) -> Result<TransportSendOutcome>;
    fn receive(&self, cursor: Option<&str>) -> Result<TransportPollOutcome>;
    fn health(&self) -> Result<TransportHealth>;
}

pub trait SchemaAdapter {
    fn family(&self) -> PayloadFamily;
    fn validate(&self, payload_json: &[u8]) -> Result<SchemaRoute>;
    fn correlation_id(&self, payload_json: &[u8]) -> Option<String>;
    fn coalesce_key(&self, payload_json: &[u8]) -> Option<String>;
}
```

`SchemaAdapter` is the boundary that keeps AIRC from duplicating Continuum
domain logic. It can inspect routing fields, correlation IDs, and event names;
it must not reinterpret persona/media semantics.

## SQLite Additions

The base substrate already defines `events`, `subscriptions`, `receipts`,
`outbox`, and `transport_cursors`. Realtime adds small projection tables for
ephemeral and subscription health.

These tables are ORM migration targets inside AIRC. They are shown to make the
storage contract reviewable, not to invite application SQL. Consumers use
`EventBus`, `SubscriptionStore`, `PresenceStore`, and generated payload adapters.

```sql
CREATE TABLE realtime_latest (
  scope_id TEXT NOT NULL,
  room_id TEXT NOT NULL,
  coalesce_key TEXT NOT NULL,
  event_id TEXT NOT NULL REFERENCES events(event_id),
  payload_family TEXT NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(scope_id, room_id, coalesce_key)
);

CREATE TABLE subscription_metrics (
  subscription_id TEXT PRIMARY KEY,
  delivered_count INTEGER NOT NULL DEFAULT 0,
  dropped_count INTEGER NOT NULL DEFAULT 0,
  last_delivery_ms INTEGER,
  last_ack_ms INTEGER,
  max_lag_events INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE request_response_index (
  correlation_id TEXT PRIMARY KEY,
  request_event_id TEXT NOT NULL REFERENCES events(event_id),
  response_event_id TEXT REFERENCES events(event_id),
  status TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
```

`realtime_latest` can be rebuilt from recent events with TTL still valid. It is
a projection, not the canonical log.

## Self-Filtering

Every publish path must carry `peer_id` and `client_id`.

Consumers may choose one of three policies:

- `include_all`: diagnostics and transcripts
- `exclude_same_client`: normal UI/agent echo suppression
- `exclude_same_peer`: rare cross-tab suppression for workflows that want one
  visible action per human/agent identity

The default must be `exclude_same_client`, because multiple tabs belonging to
one peer are independent workers.

## Backpressure And Coalescing

Presence and activity state can be noisy. AIRC should coalesce these by
`room_id + peer_id + client_id + state_kind + target_id`.

Recommended defaults:

- typing/thinking TTL: 5 seconds
- speaking/in-call TTL: producer-defined heartbeat, default 10 seconds
- dropped ephemeral events are counted, not replayed forever
- durable control events are never silently dropped
- if a subscriber falls behind a configured window, emit a
  `system.warning`/lag event and force cursor resume from SQLite

## WebRTC And LiveKit Boundary

AIRC carries control-plane state only:

- room intent
- participant joined/left
- agent connected/disconnected
- cognitive state changes
- offer/answer/ICE metadata only when needed by the selected adapter
- reconnect and teardown events
- health and route diagnostics

AIRC does not carry audio/video frames. Live media stays on WebRTC/LiveKit UDP
paths. AIRC can coordinate those paths, record control state, and replay enough
history to recover a session manager.

## Benchmarks And Smoke Tests

Before Continuum integration, the implementation needs:

- two local subscribers exchanging chat plus typing/thinking presence
- deterministic replay after one subscriber restarts
- self-filter test with two clients under one peer
- request/response correlation test for JTAG/GridFrame payloads
- LiveKit control smoke using `BridgeCommand`/`BridgeEvent` JSON fixtures
- coalescing test proving 1,000 typing updates become one latest projection
- fanout benchmark for 4, 8, and 16 subscribers
- idle CPU benchmark for subscription loops
- reconnect benchmark measuring cursor replay latency

Initial targets:

- p95 local publish-to-subscriber latency under 20 ms
- idle subscription loop below 1 percent CPU
- 1,000 coalesced presence updates produce one latest row and bounded disk writes
- replay of 1,000 durable events under 50 ms from local SQLite

## Implementation Order

1. Add Rust `airc-core` types for delivery metadata and subscription requests.
2. Add `SchemaAdapter` fixtures for AIRC-native JSON and Continuum package JSON.
3. Extend `airc-store` migrations with realtime projection tables.
4. Implement local in-process publish/poll/ack tests.
5. Add transport adapter tests with duplicate ingest and self-filtering.
6. Add CLI smoke commands only after the Rust library behavior is covered.
7. Wire Continuum through adapters once benchmarks meet the alpha targets.
