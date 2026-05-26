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
    seq: Seq,                  // owner-assigned order = (epoch: u64, counter: u64).
                               // epoch is persisted and bumped on every daemon
                               // start, so post-crash events sort strictly AFTER
                               // anything pre-crash even if the in-memory counter
                               // rewinds (deliver-first can ack a counter the ORM
                               // hasn't flushed yet — see §3.8). A bare u64 counter
                               // is NOT safe here.
    occurred_at_ms: u64,       // owner-stamped via an injectable clock (deterministic tests)
    correlation_id: Option<Uuid>, // command ↔ result, request ↔ response
    coalesce_key: Option<String>, // for EphemeralLatest
    headers: BTreeMap<String,String>, // routable metadata; airc routes on these, never parses payload
    payload: Bytes,            // OPAQUE. consumer-typed (Continuum JTAG/GridFrame, agent, …)
}
```

The server routes on `channel` / `target` / `headers` / `delivery` and **never
interprets `payload`**. That opacity is what keeps it generic across towers.

**Signature scope:** the sender signs the sender-authored fields only. `seq` is
owner-assigned metadata *outside* the sender's signature — otherwise a remote
owner re-injecting an event (§3.7) would invalidate the signature when it stamps
its own seq. Verification covers `{event_id, channel, from, target, kind,
correlation_id, headers, payload}`; `seq`/`occurred_at_ms` are owner-stamped and
covered (if at all) by a separate owner signature.

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
- Cursor = `(seq, event_id)`, `seq = (epoch, counter)`; durable per-subscriber
  position. **Scoped per-owner-per-channel** — a channel's total order is
  authoritative only within one owner daemon. Cross-machine order of a shared
  channel is deliberately NOT assumed here (§9); slice 1 must not bake in a single
  global authority.
- **One atomic contract:** "deliver everything strictly after my cursor, then go
  live" — no poll gap, no double-delivery. **Precondition that makes "no gap"
  true:** a `Durable` ring entry is not evictable until the write-behind confirms
  it is persisted (§3.8), so the seam's deep-replay can never miss an event that
  is neither in the ring nor in the ORM yet. Recent from ring, deep from ORM via
  the `(channel, epoch, counter)` index, then attach to the live broadcast at a
  seam that admits no miss/dup.
- **Slow subscriber = lagged, never a stall.** Fan-out NEVER blocks on a slow
  `Durable` subscriber (that would head-of-line-block the whole shard). It is
  marked lagged, the live push dropped, and it resumes from the store via its
  cursor (§3.8).

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

### 3.8 Crash-safety, ordering & backpressure invariants
The preconditions that make §3.5's cursor contract actually hold under
deliver-first + persist-async. **Slice 1 implements these; it must not assume
them away** — they're where a naive cursor contract silently drops or reorders.

- **Generational order (`seq = (epoch, counter)`).** `epoch` persisted, bumped
  every daemon start; `counter` the in-memory monotonic. Deliver-first acks a
  `counter` before the ORM flushes it, so a crash loses the tail and a counter
  rebuilt from ORM-max would *reissue* numbers live subscribers already observed.
  Bumping `epoch` makes post-crash events sort strictly after anything pre-crash
  regardless of counter rewind. `(epoch, counter, event_id)` is the total order
  within one owner+channel.
- **Ring entries pinned until persisted.** A `Durable` ring entry is not evictable
  until write-behind confirms it's in the ORM — the precondition for "no gap"
  (otherwise an event can be neither in the ring nor persisted at a seam replay).
  Sets a **ring capacity floor ≥ max un-persisted backlog**.
- **Durable receipt + `await_durable` opt-in.** Default receipt `(event_id, seq)`
  returns after fan-out, before persistence — correct for fire-and-forget chat.
  Durability-critical flows (a command/result the publisher acts on) pass
  `await_durable: true`; publish resolves only after the group-commit. Fast path
  stays default.
- **Bounded write-behind + explicit full policy.** Queue is bounded (≥ ring
  floor). When full: durable/`await_durable` publishers **block** (publisher-side
  backpressure); fire-and-forget sheds with a surfaced `WriteBehindSaturated`
  error. Never silently drop a `Durable`; never OOM.
- **WAL checkpoint off the hot path.** Sustained durable writes grow the WAL;
  checkpoints run on the writer task by cadence/size trigger, off the publish hot
  path, so they can't spike publish→subscriber p95 (§6 benches this).

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
  writer task** draining a **bounded** batch queue (single writer, group commit).
- **Backpressure is on the publisher, never on fan-out.** When the write-behind
  queue is full the *publisher* blocks (or sheds with a surfaced error for
  fire-and-forget) — §3.8. Fan-out to subscribers is **never** blocked by a slow
  consumer: a lagging `Durable` subscriber is marked lagged and resumes from the
  store (§3.5); ephemeral drops-and-counts. Neither stalls the shard, so this
  doesn't contradict "no blocking across `.await`."
- **No lock held across `.await`; no global mutable state; injectable clock + seq
  counter** — the reliability invariant, and what makes §9's tests deterministic.

## 6. Performance targets (gated, not claimed)

- p95 publish→subscriber **< 20 ms** (in-process sub-ms; IPC low-ms).
- Idle subscription loop **< 1% CPU** (push, not poll — idle is truly idle).
- Room-per-activity: thousands of channels, idle ones ≈ free (cursor pays only
  for consumed events; no per-room file or poll).
- Ephemeral burst (typing/presence/signaling) does **not** touch the ORM.
- Bench: extend `crates/airc-lib/tests/fanout_bench.rs` with many-rooms,
  burst-ephemeral, deep-replay, idle-CPU, and a **sustained-durable-write** case
  long enough to trigger real WAL checkpoints (not just bursts) — proves
  checkpoint stalls don't blow p95.

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
shared mutable global state (runtime or tests); **injectable clock + seq counter**
so tests are bit-deterministic; tests run against isolated embeddable owner
instances with explicit lifecycle (every spawned daemon reaped); durable path
never loses an event (§3.8), ephemeral drops counted; **every CI job green before
merge** (this repo doesn't enforce required checks — the merging agent verifies
each, esp. `windows-latest`; no `--auto`).

**Cross-machine ordering is an open problem, scoped OUT of slice 1 (and not
precluded).** When account-canonical identity lets two owner daemons write the
same `room_id`, each has its own `(epoch, counter)` authority — there is no single
per-channel total order across owners, so re-injection (§3.7) can interleave/dup.
Slice 1's cursor contract is therefore **per-owner-per-channel** (§3.5); the
cross-machine resolution (hybrid logical clock, or per-channel owner election)
lands in slices 4-5. Slice 1 must not inherit a single-global-authority
assumption that slice 4 then has to tear out.

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
