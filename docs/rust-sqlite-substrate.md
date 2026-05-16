# Rust SQLite Substrate

**Status:** design contract for airc#621
**Scope:** chat, files, queue coordination, realtime events, health, and transport cursors

## Goal

AIRC should move from shell/Python plus JSONL/GitHub as the hot runtime path to
a Rust-owned SQLite event store. GitHub, local files, direct sockets, and future
mesh transports become adapters around the same typed local substrate.

The command surface should stay simple:

```bash
airc msg "status: tests are green"
airc logs 20
airc queue
airc hygiene report
```

The implementation underneath should stop making every command rediscover,
reparse, or repoll the world.

## Ownership Boundaries

AIRC owns:

- durable event append, dedupe, replay, and subscriptions
- room, peer, client, receipt, cursor, and transport health state
- queue card cache/projections for GitHub issue-backed kanban
- file and attachment manifests plus local blob metadata
- realtime generic events such as typing, thinking, presence, receipts, and call
  control envelopes
- backpressure and resource budget signals

Continuum owns:

- persona behavior, memory, world/activity semantics, and UI projection policy
- media-domain meaning for WebRTC/LiveKit sessions
- application-specific command handlers produced by AIRC events
- ORM-backed application entities and projections; Continuum must not issue SQL
  against the AIRC store or depend on AIRC table shapes

Adapters own:

- GitHub gist/issue API calls
- local-only transport, LAN/Tailscale/WebRTC/LiveKit signaling bridges, and any
  future Reticulum-like bearer
- import/export compatibility with existing JSONL rooms

No adapter should own the state machine. No Continuum module should need to know
whether an AIRC event arrived from a gist, socket, local store, or future mesh.

## Runtime Shape

```text
airc command / daemon
        |
        v
Rust command API
        |
        v
EventStore + ProjectionStore + SubscriptionHub
        |
        +-- SQLite WAL database
        +-- BlobStore for attachments
        +-- TransportAdapter fanout/inbox/outbox
        +-- QueueProjection, ChatProjection, PresenceProjection, HealthProjection
```

The store is the local source of truth. Remote systems are synchronization
targets or sources. If GitHub is slow, rate-limited, or offline, AIRC can still
append locally, show state, queue outgoing work, and resume when the adapter
recovers.

## Event Model

Every durable fact becomes an append-only event:

```rust
pub struct EventEnvelope {
    pub event_id: EventId,
    pub scope_id: ScopeId,
    pub room_id: RoomId,
    pub conversation_id: Option<ConversationId>,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub lamport: i64,
    pub occurred_at_ms: i64,
    pub kind: EventKind,
    pub payload_json: Vec<u8>,
    pub payload_hash: [u8; 32],
    pub source: EventSource,
    pub source_seq: Option<i64>,
    pub parent_event_id: Option<EventId>,
}
```

`event_id` is stable and dedupe-safe. A good default is a BLAKE3/SHA-256 hash
over scope, room, peer, client, lamport, kind, payload hash, and source metadata.

`client_id` remains load-bearing. Receivers must be able to suppress their own
echoes without suppressing another tab belonging to the same peer.

`kind` is typed at the Rust boundary and serialized only at adapter edges. The
initial set should cover:

- `chat.message`
- `chat.receipt`
- `presence.status`
- `presence.typing`
- `presence.thinking`
- `file.manifest`
- `file.receipt`
- `queue.card_observed`
- `queue.claimed`
- `queue.status_changed`
- `queue.heartbeat`
- `transport.health_sample`
- `transport.cursor_advanced`
- `webrtc.signaling`
- `livekit.control`
- `system.warning`

Domain-specific meaning can live above this list, but transport and replay
semantics must not.

## Storage Model

Use SQLite WAL mode behind a Rust ORM/entity layer. The Rust crate owns
migrations and enforces invariants through typed repository methods and
transactions. Application code talks to Rust traits and generated types, not SQL.

The schema below is a storage and migration contract for the ORM entities. It is
not a public query API, and Continuum should never bind to it directly.

Suggested pragmas:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
```

`synchronous=NORMAL` is appropriate for chat/runtime throughput. Commands that
write key material or irreversible accounting can opt into a stricter transaction
mode later.

Core tables:

```sql
CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE peers (
  peer_id TEXT PRIMARY KEY,
  display_name TEXT NOT NULL,
  public_key TEXT,
  first_seen_ms INTEGER NOT NULL,
  last_seen_ms INTEGER NOT NULL,
  metadata_json BLOB NOT NULL DEFAULT '{}'
);

CREATE TABLE clients (
  client_id TEXT PRIMARY KEY,
  peer_id TEXT NOT NULL REFERENCES peers(peer_id),
  scope_id TEXT NOT NULL,
  host_id TEXT,
  first_seen_ms INTEGER NOT NULL,
  last_seen_ms INTEGER NOT NULL
);

CREATE TABLE rooms (
  room_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  scope_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  metadata_json BLOB NOT NULL DEFAULT '{}'
);

CREATE TABLE events (
  event_id TEXT PRIMARY KEY,
  scope_id TEXT NOT NULL,
  room_id TEXT NOT NULL REFERENCES rooms(room_id),
  conversation_id TEXT,
  peer_id TEXT NOT NULL,
  client_id TEXT NOT NULL,
  lamport INTEGER NOT NULL,
  occurred_at_ms INTEGER NOT NULL,
  received_at_ms INTEGER NOT NULL,
  kind TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  payload_json BLOB NOT NULL,
  source_kind TEXT NOT NULL,
  source_id TEXT,
  source_seq INTEGER,
  parent_event_id TEXT,
  UNIQUE(source_kind, source_id, source_seq)
);

CREATE INDEX events_room_lamport_idx
  ON events(room_id, lamport, event_id);

CREATE INDEX events_kind_time_idx
  ON events(kind, occurred_at_ms);

CREATE TABLE subscriptions (
  subscription_id TEXT PRIMARY KEY,
  room_id TEXT NOT NULL,
  kind_prefix TEXT NOT NULL,
  cursor_lamport INTEGER NOT NULL DEFAULT 0,
  cursor_event_id TEXT,
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE receipts (
  event_id TEXT NOT NULL REFERENCES events(event_id),
  peer_id TEXT NOT NULL,
  client_id TEXT NOT NULL,
  receipt_kind TEXT NOT NULL,
  received_at_ms INTEGER NOT NULL,
  PRIMARY KEY(event_id, peer_id, client_id, receipt_kind)
);

CREATE TABLE outbox (
  outbox_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES events(event_id),
  adapter TEXT NOT NULL,
  target TEXT NOT NULL,
  status TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  next_attempt_ms INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at_ms INTEGER NOT NULL
);

CREATE INDEX outbox_ready_idx
  ON outbox(status, next_attempt_ms);

CREATE TABLE transport_cursors (
  adapter TEXT NOT NULL,
  target TEXT NOT NULL,
  cursor TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(adapter, target)
);

CREATE TABLE files (
  file_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES events(event_id),
  name TEXT NOT NULL,
  media_type TEXT,
  size_bytes INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  local_path TEXT,
  remote_ref_json BLOB NOT NULL DEFAULT '{}',
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE queue_cards (
  repo TEXT NOT NULL,
  issue_number INTEGER NOT NULL,
  status TEXT NOT NULL,
  owner TEXT,
  branch TEXT,
  priority TEXT,
  next_action TEXT,
  evidence TEXT,
  last_event_id TEXT REFERENCES events(event_id),
  updated_at_ms INTEGER NOT NULL,
  raw_json BLOB NOT NULL,
  PRIMARY KEY(repo, issue_number)
);

CREATE TABLE health_samples (
  sample_id TEXT PRIMARY KEY,
  component TEXT NOT NULL,
  metric TEXT NOT NULL,
  value REAL NOT NULL,
  unit TEXT NOT NULL,
  sampled_at_ms INTEGER NOT NULL
);
```

Projection tables are caches. They can be rebuilt from `events`. If a projection
cannot be rebuilt, it is the wrong abstraction.

## Rust Traits And ORM Boundary

The first Rust crate should expose narrow traits that can be tested without a
daemon:

```rust
pub trait EventStore {
    fn append(&self, event: NewEvent) -> Result<AppendOutcome>;
    fn append_batch(&self, events: &[NewEvent]) -> Result<Vec<AppendOutcome>>;
    fn recent(&self, room: &RoomId, limit: u32) -> Result<Vec<EventEnvelope>>;
    fn page_before(
        &self,
        room: &RoomId,
        before: EventCursor,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>>;
    fn resume(&self, subscription: &SubscriptionId) -> Result<Vec<EventEnvelope>>;
}

pub trait Projection {
    fn name(&self) -> &'static str;
    fn apply(&self, tx: &mut StoreTransaction, event: &EventEnvelope) -> Result<()>;
    fn rebuild(&self, store: &dyn EventStore) -> Result<()>;
}

pub trait TransportAdapter {
    fn name(&self) -> &'static str;
    fn enqueue(&self, tx: &mut StoreTransaction, event: &EventEnvelope) -> Result<()>;
    fn poll(&self, cursor: Option<&str>) -> Result<PollOutcome>;
    fn health(&self) -> Result<TransportHealth>;
}

pub trait BlobStore {
    fn put(&self, manifest: NewFileManifest, reader: &mut dyn std::io::Read) -> Result<FileId>;
    fn get(&self, file_id: &FileId) -> Result<Box<dyn std::io::Read>>;
    fn manifest(&self, file_id: &FileId) -> Result<FileManifest>;
}
```

Use a Rust ORM crate for the first implementation, with typed entities,
migrations, and repository methods. SeaORM is the default candidate because it
supports SQLite and async service code cleanly; Diesel is acceptable if its
compile-time schema and sync model fit the first crate better. Raw SQL belongs
only in migrations, narrowly reviewed performance escapes, or ORM-generated
code. It must not leak into AIRC command handlers, adapters, or Continuum.

A single writer path plus reader pool is enough for the near-term command/daemon
split. If benchmarks prove the ORM layer is too slow for a hot path, optimize
behind the same trait boundary and keep the public API unchanged.

## Command Contract

The shell/Python layer should become dispatch glue:

- `airc msg` calls Rust append, then enqueues adapter fanout.
- `airc logs` pages the SQLite chat projection.
- `airc queue` reads the queue projection and asks the GitHub adapter to refresh
  only when its cursor is stale.
- `airc hygiene report` writes health samples and can trigger policy hooks.
- monitor/codex-poll subscribe from the store instead of tailing raw JSONL.

Continuum integration should consume Rust/TypeScript types, IPC responses, and
projection APIs. It should not open the SQLite database, run SQL queries, or
mirror AIRC's table names into its own domain code.

Logic that must not stay duplicated in shell/Python after Rust owns it:

- event ID generation and dedupe
- cursor advancement
- receipt semantics
- queue card parsing and mutation
- retry/backoff classification
- payload validation
- self-filtering by `client_id`
- projection rebuild rules

## Benchmarks And VDD

The crate should ship with `cargo bench` or a dedicated `airc bench substrate`
command before Continuum depends on it.

Required scenarios:

- append 1,000, 10,000, and 100,000 chat events
- page recent 20/100/1,000 events from a hot room
- resume a subscription after 0, 100, and 10,000 missed events
- apply queue projection updates for 1,000 issue cards
- ingest duplicate events from two adapters and prove idempotency
- run 4, 8, and 16 concurrent agent writers through the single-writer path
- enqueue and drain 10,000 outbox rows with transient failures
- record health samples for disk, CPU, memory, GPU hook, and adapter latency

Metrics to report:

- p50/p95/p99 append latency
- p50/p95/p99 subscription delivery latency
- CPU time per 1,000 events
- RSS and SQLite file size after each scenario
- write amplification in WAL and checkpoint behavior
- GitHub API calls avoided by local projection cache

Initial targets for alpha:

- local append p95 under 5 ms for single writer
- recent page p95 under 20 ms for 100 events
- subscription resume p95 under 50 ms for 1,000 missed events
- no polling loop above 1 percent CPU while idle
- no command path that shells out to GitHub when fresh local state is enough

Targets can tighten after real hardware traces. The important rule is that every
performance claim must have a reproducible measurement.

## Migration Plan

1. Add `airc-store` Rust crate with ORM entities, migrations, event ID
   generation, append/page/resume APIs, and unit tests.
2. Add JSONL import/export so existing `messages.jsonl` rooms are not stranded.
3. Route `airc logs` through the Rust store behind a feature flag while keeping
   JSONL as compatibility output.
4. Move `airc msg` append/dedupe/outbox into Rust; keep GitHub bearer as an
   adapter.
5. Move queue card parsing/projection into Rust; keep GitHub issues as the
   canonical remote work record.
6. Add subscription API for monitor/codex-poll and generic realtime events.
7. Add benchmark gate and bottleneck ledger issue creation for any missed target.
8. Make SQLite the default runtime store. Keep JSONL export for debugging and
   old peers.

## Security And Privacy

- database files must be created 0600 when the platform supports it
- payload encryption remains envelope-level; SQLite should not imply plaintext
  safety
- secrets and private keys stay outside projection rows
- adapter logs must not print message bodies or private metadata by default
- public issue comments must never receive sensitive payloads unless explicitly
  encrypted and policy-allowed
- docs and issue updates must avoid personal data

## Open Questions

- Should the daemon expose subscriptions over a local Unix socket, TCP loopback,
  or stdio for agent runtimes?
- Should file blobs live beside the SQLite DB by content hash, or in a pluggable
  cache directory controlled by hygiene policy?
- Which queue fields become typed columns first, and which remain raw JSON until
  forge-alloy contracts settle?
- Do WebRTC/LiveKit signaling events need a stricter expiry/TTL projection than
  chat and queue events?
