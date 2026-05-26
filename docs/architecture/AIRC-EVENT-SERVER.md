# AIRC Event Server — Architecture

**Status:** implementation design for `rust-rewrite`. The efficient, ORM-backed,
push-based event-routing server that *is* the AIRC substrate.
**Date:** 2026-05-26
**Cross-system model (canonical):** Continuum's
`docs/architecture/GRID-BUS-ARCHITECTURE.md` (#1439) — *how* Continuum/grid/agents
use AIRC (the cut: airc = event-log + transport; ORM = entities; `Commands.execute`
/ `Events.emit` extend onto airc as a third transport). **This doc does not restate
that** — it designs the airc-side server that provides the substrate that doc
consumes.

## 0. What this is

One **machine-account owner daemon**: a purpose-built, in-memory-hot,
ORM-durable, push-based **event-routing server**. It carries *generic envelopes*
— a chat message, a `data:*` event, a `screenshot` command, a WebRTC signaling
frame are all the same primitive, distinguished by `kind` + `delivery_class`,
never by living on different buses. Every consumer (Continuum's Rust edge,
Claude/Codex agents, Hermes/OpenClaw, other towers) attaches as a **session**.

Non-negotiable bar (Joel, 2026-05-26): **an extremely quick, efficient router +
data access + caching + cursors — a good efficient server.** `frames.jsonl` +
50ms polling violated every one of those and is **deleted**, not optimized.

## 1. Topology

```
            ┌─────────────────── machine account ───────────────────┐
  Continuum edge ─┐                                                  │
  Claude tab    ──┤  IPC (attach/subscribe/publish/ack)              │
  Codex run     ──┼──────────────►  OWNER DAEMON  ◄── grid transports │── LAN/Tailscale/
  Hermes/OpenClaw ┘                 (this server)     (single client)  │   relay/WebRTC
            │                          owns: router • hot ring •        │   to other
            │                          ORM • presence • cursors         │   machine owners
            └────────────────────────────────────────────────────────┘
```

- **Exactly one owner daemon per machine account.** It owns all coordination
  state. Clients are ephemeral sessions; opening/closing one is a no-op for
  everyone else. (Deletes the N-daemon-per-scope + leaked-daemon classes.)
- **The daemon is the single client of cross-machine transports.** Remote owners
  are peers; routing between machines is the daemon's job, not each tab's.

## 2. The envelope (generic; opaque payload)

```rust
struct Envelope {
    event_id: Uuid,            // stable across replay
    channel: RoomId,           // the room/stream (Uuid)
    from: (PeerId, ClientId),  // sender identity + session
    target: Target,            // All | Endpoint(addr) | Peer(id) | Reply(correlation)
    kind: Kind,                // Message | Event | Command | CommandResult | Signal | StreamChunk | Control
    delivery: DeliveryClass,   // Durable | EphemeralLatest | EphemeralWindow | RequestResponse | StreamChunk
    lamport: u64,              // logical order (assigned by owner on publish)
    occurred_at_ms: u64,
    correlation_id: Option<Uuid>, // command ↔ result, request ↔ response
    coalesce_key: Option<String>, // for EphemeralLatest
    headers: BTreeMap<String,String>, // routable metadata; airc routes on these, never parses payload
    payload: Bytes,            // OPAQUE. consumer-typed (Continuum JTAG/GridFrame, agent, …)
}
```

The server routes on `channel` / `target` / `headers` / `delivery` and **never
interprets `payload`**. That opacity is what keeps it generic across towers.

## 3. The layers (the "good efficient server")

### 3.1 Router — hot path, in-memory, sub-µs
Routing is a memory operation; it never touches the DB.

- **Subscription index:** `channel → SmallVec<SubscriberHandle>`, plus a
  header/kind predicate compiled per subscription. Lookup is O(1) on channel +
  cheap predicate eval. Pattern subscriptions (continuum's wildcard/elegant)
  compile to a predicate, so one subscription spans many rooms.
- **Endpoint/peer routing table:** `endpoint → route`, `peer → transport` —
  cached, folded from manifests once (not re-queried per send). Cross-machine
  routing is a table lookup.
- **Sharding:** the channel→subscriber map is sharded (e.g. `DashMap` or N
  mutex-striped maps keyed by `channel`) so unrelated rooms never serialize on
  one lock. No lock is ever held across an `.await`.
- **Dispatch:** publish → assign lamport → index lookup → push into each
  subscriber's bounded channel. Sub-µs for in-process; one hop for IPC clients.

### 3.2 Hot ring — per-channel recent-event cache
- Each active channel has a fixed-capacity in-memory ring of recent envelopes.
- Serves **live fan-out** and **tail-N / replay-of-recent** entirely from RAM.
- The ORM is consulted **only** for cold/deep replay past the ring. Common case
  (live + recent backfill on widget/persona mount) never hits disk.
- Idle channels keep a tiny ring (or drop to zero) — many rooms stay cheap.

### 3.3 ORM durable tier — `airc-store`, done right
- `events(channel, lamport, event_id PK, occurred_at, kind, delivery, headers,
  payload)`; index `(channel, lamport)`; SQLite WAL.
- **Single writer = the owner daemon** → no write-lock contention (this is what
  makes SQLite fast here). Prepared-statement cache. **Batched appends** (group
  commit on a short timer / N-events).
- **Deliver-first, persist-async:** publish fans out + rings *before* the ORM
  write; a write-behind task persists `Durable`-class envelopes. Delivery latency
  is the broadcast, never the fsync. Durability source of truth is the ORM; a
  crash loses only un-flushed tail, replayable from peers.
- **Only `Durable` events become rows.** This is the efficiency keystone:
  high-frequency ephemerals never hit the DB (see 3.4).

### 3.4 Presence / ephemeral cache — coalesced, in-memory
- `EphemeralLatest` (presence, typing, resource-pressure, signaling churn,
  media keepalives) is coalesced **latest-wins by `coalesce_key`** in an
  in-memory map with TTL — **not** one row per update. 1000 typing updates → one
  latest value. The firehose that would kill a DB never reaches it.
- Rebuildable from recent events; it's a projection, not a log.

### 3.5 Cursor engine — efficient replay
- Cursor = monotonic `(lamport, event_id)`; durable per-subscriber position.
- **One atomic contract:** "deliver everything strictly after my cursor, then go
  live" — no poll gap, no double-delivery (recent served from ring, deep from
  ORM via `(channel, lamport)` index, then attach to the live broadcast under a
  lock that guarantees no event is missed or duplicated at the seam).
- Lagged subscribers get an explicit lag signal + forced resume from store —
  never silent loss on the durable path.

### 3.6 IPC — sessions attach
- `attach{filter, from_cursor}` → server replays-then-streams. `publish{envelope}`
  → routed. `ack{cursor}` → durable cursor advance. Length-framed, the existing
  v4 protocol. The push stream is the `attach` long-response.

### 3.7 Grid transport — the daemon is the single grid client
- Envelopes whose `target`/route resolves remote are handed to the selected
  transport (LAN/Tailscale/relay/WebRTC); inbound remote envelopes are verified
  once and re-injected into the local router (so a remote `data:*` reaches local
  subscribers identically to a local one). The `grid-router-daemon` policy
  (BGP-style, from GRID-BUS) lives here.

## 4. Data flow

**Publish (hot):** `publish(env)` → assign `lamport` (atomic) → verify signature
**once** → router fan-out to local subscribers + append to channel ring (sub-µs)
→ if remote target, enqueue to transport → if `Durable`, enqueue to write-behind
ORM batch; if `EphemeralLatest`, update coalesced cache. Return receipt
(`event_id, lamport`) immediately.

**Subscribe:** `attach(filter, cursor)` → register predicate in the sharded
index → replay `(cursor, now]` from ring (recent) or ORM (deep) → flip to live
broadcast at the seam (no gap/dup) → push thereafter.

**Cross-machine:** local publish with remote target → transport → remote owner
verifies + re-injects → its local subscribers receive it. Symmetric inbound.

## 5. Concurrency model

- Tokio multi-thread. Per-channel sharding for the router; one **dedicated ORM
  writer task** draining a batch queue (single writer, group commit). Broadcast
  via bounded per-subscriber channels (backpressure on `Durable`, drop-and-count
  on ephemeral). **No lock held across `.await`; no global mutable state** (the
  reliability invariant — also what makes tests deterministic).

## 6. Performance targets (gated, not claimed)

- p95 publish→subscriber **< 20 ms** (in-process sub-ms; IPC low-ms).
- Idle subscription loop **< 1% CPU** (push, not poll — idle is truly idle).
- Room-per-activity: thousands of channels, idle ones ≈ free (cursor pays only
  for consumed events; no per-room file or poll).
- Ephemeral burst (typing/presence/signaling) does **not** touch the ORM.
- Bench: extend `crates/airc-lib/tests/fanout_bench.rs` with many-rooms,
  burst-ephemeral, deep-replay, and idle-CPU cases.

## 7. What this deletes / replaces

- **`frames.jsonl` / `LocalFsAdapter` same-machine path — deleted.** Same-machine
  delivery is owner in-memory fan-out + IPC push.
- **N daemons per scope → one owner daemon.** Tabs/personas are sessions.
- **Continuum dual-write / mirror stack** (`src/system/airc-chat/*`,
  `continuum-airc-bridge.mjs`) — deleted per GRID-BUS; Continuum's Rust edge
  speaks generic envelopes to this server.

## 8. Maps onto existing crates

- `airc-store` — the ORM tier (reuse + harden: batching, prepared stmts, indexes).
- `airc-daemon` — becomes the owner daemon (router + ring + cursor engine + IPC).
- `airc-lib` — embeddable client + the in-process fast path (Continuum's edge,
  agents).
- `airc-ipc` — the attach/publish/ack session protocol (v4).
- Foundation already merged (L1/L2 PRs #1443-1456): EventClass registry,
  `AircEventTransport` seam, command scope, peer manifests, realtime-over-daemon-
  IPC, **lamport cursors**, contract-chain primitives. We finish on top of these.

## 9. Reliability invariants

Deterministic convergence (account-canonical identity → stable room_id); no
shared mutable global state (runtime or tests); tests run against isolated
embeddable owner instances with explicit lifecycle (every spawned daemon reaped);
durable path never loses an event, ephemeral drops counted; **every CI job green
before merge** (this repo doesn't enforce required checks — the merging agent
verifies each, esp. `windows-latest`; no `--auto`).

## 10. Implementation slices (top-down, each benchmark + reliability gated)

1. **Owner daemon core:** in-memory router (sharded) + per-channel hot ring +
   ORM durable tier (batched, single-writer) + IPC attach/publish + cursor
   contract. **Delete the `frames.jsonl` same-machine path.** Bench: fan-out p95,
   many-rooms idle, deep replay.
2. **DeliveryClass + ephemeral coalescing** (`realtime_latest`, TTL) — keep the
   firehose off the ORM.
3. **Sessions:** Continuum Rust edge + agent sessions attach; one client API.
4. **Grid transport + router-daemon:** remote targets, manifest folding,
   BGP-style policy; cross-machine re-injection.
5. **Cross-machine convergence:** account-registry publish/refresh; same account
   converges across machines.
6. **Continuum embed + delete dual-write:** finish GRID-BUS migration steps;
   `src/system/airc-chat/*` deleted.

Slice 1 is the keystone and dissolves the file-poll, the leaked-daemon class, and
the macOS test-flake class structurally.
