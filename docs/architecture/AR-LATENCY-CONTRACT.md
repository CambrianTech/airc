# AR Latency Contract

**Status**: Draft contract for the AR-consumer slice of airc. The
substrate has to carry Continuum's AR pose/spatial state alongside
chat-shaped traffic without making either suffer. Numbers below are
target budgets; actual measurement is pending instrumentation
(Phase 3.6 benchmarks). All TBD lines are explicit and need
Continuum-side measurement to lock down.

## Why this exists

airc was designed as a generic event substrate. AR is the
highest-latency-sensitivity consumer we know about — sub-frame
spatial sync at 60–90Hz, per-headset pose streams, optionally
multi-user shared anchors. If the bus can carry AR cleanly, it
trivially carries chat / commands / events. The reverse is not
true.

This doc pins the contract AR consumers can rely on, and what they
must not assume.

## Consumer profiles

Different AR workloads hit the substrate differently. The contract
distinguishes:

### Profile A: per-headset pose stream

- **Cadence**: 60–90Hz steady state; bursts to ~120Hz acceptable.
- **Payload**: tens of bytes per frame (pose + timestamp + a few
  flags). Strict upper bound: 512 bytes including envelope.
- **Topology**: many-to-one or many-to-few — one headset's pose
  emitted to whichever peers render the scene.
- **Loss tolerance**: high. A dropped frame is invisible at 60Hz;
  even a 100ms gap is recoverable. Ordering matters; reordering
  doesn't (timestamps determine display).
- **Latency budget**: e2e < 25ms p99 within Tailnet/LAN; < 8ms p99
  same-machine.

### Profile B: shared spatial anchor / world state

- **Cadence**: bursty. ~5–20Hz steady, spikes to 60Hz during
  active manipulation.
- **Payload**: hundreds of bytes to a few KB. Anchors carry
  position + orientation + version + manipulating peer.
- **Topology**: full mesh among participants in the same scene.
- **Loss tolerance**: low. Lost anchor updates manifest as visible
  desync; must be retried or merged-via-CRDT.
- **Latency budget**: < 80ms p99 same-fleet; visible drift above.

### Profile C: AR event/command stream

- **Cadence**: sparse, event-driven. Mode changes, button presses,
  AI commands.
- **Payload**: a few hundred bytes, occasionally larger (a
  tool-call argument).
- **Topology**: request/reply, typically with correlation IDs.
- **Loss tolerance**: zero — these are commands.
- **Latency budget**: < 200ms p95 for human-perceptible
  interactivity.

## What airc provides today

| Need | Status | Mechanism |
|---|---|---|
| Local-fs same-machine fan-out | shipped | `local-fs` adapter; tail-loop CPU bounded by SQLite WAL |
| Signed envelopes (Ed25519) | shipped | per-frame `Signature::Ed25519` |
| Per-channel ordering | shipped | Lamport clocks + event_id tiebreak |
| Multi-room subscription | shipped | `subscribe_subscribed_filtered` (#876) |
| Replay on attach | shipped | `replay_wire_once` (post-#905 skip-and-warn) |
| LAN-TCP transport | shipped | `lan-send` / `lan-listen` |
| Route resolution (LAN / Tailnet / relay / etc.) | partial | route graph + resolver; some routes still planning per ROBUSTNESS-INTEGRATION-PLAN |

## What AR consumers need that isn't there yet

These are the gaps. Each is a follow-up phase, not a Phase 3.5
deliverable.

### Bounded broadcast capacity for high-cadence streams

The broadcast channel is fixed at 1024 events (LiveLag surfaces
lag). At 60Hz with 5 concurrent subscribers, 1024 events ≈ 3.4
seconds of slack — fine for chat, marginal for sustained AR pose
streams.

**Open**: dedicated high-rate broadcast lane per profile, OR a
per-subscriber backpressure signal so AR consumers can rate-limit
their publish.

### Latency-class headers

The route resolver currently picks routes by health. AR pose
streams want "local-fs or LAN-TCP only, never relay." Today the
consumer can't express that.

**Proposed**: `airc.route_class` header (`local-only`,
`lan-allowed`, `any`). Resolver respects the class; falls back
within the allowed set.

### Per-stream priority

If chat + AR pose share the same wire, a flood of chat shouldn't
delay pose frames. Need a priority lane on the broadcast channel
OR per-channel cadence isolation.

**Open**: priority is consumer-hint or substrate-policy?

### Payload-size budget enforcement

A buggy consumer could send a 10MB blob on a pose channel. The
substrate doesn't enforce per-channel size limits. AR profiles
need this hard-capped.

**Proposed**: `airc.max_payload_bytes` channel metadata read at
join. Send-side rejection if exceeded.

### Drop policy

AR Profile A explicitly accepts loss. Today every event is
durable in `events` table. For pose streams, durable persistence
is overkill (and a write-rate concern at 60Hz × N peers).

**Proposed**: `TranscriptKind::Ephemeral` variant that broadcasts
without persisting. Subscribers see it live; replay never sees it.

## Measurement requirements (before locking targets)

The numbers above are target budgets. To lock down a real
contract, AR consumers + airc need:

1. **End-to-end latency histogram** at each route — local-fs / LAN /
   Tailnet / relay — at 1Hz / 10Hz / 60Hz / 120Hz cadences.
2. **Memory pressure at sustained 60Hz × N=10 subscribers** — does
   the broadcast Arc-clone work, or does `event.clone()` (audit
   Phase 3.6 hotpath #2) bite?
3. **Tail-loop CPU** during sustained AR-rate publishes — is the
   SQLite-WAL writer the bottleneck?

Benchmarks live in `crates/airc-lib/benches/` (new in Phase 3.6).
Numbers feed back into PERF-BASELINES.md (TODO).

## Doctrine notes for AR consumers

- **Same-machine first, same-fleet second, public p2p last.** The
  doctrine non-negotiable #3 (local route wins) maps directly to AR
  latency requirements. Building an AR product on relay-only routes
  is not supported.
- **gh-gist is never on the AR data plane.** Per non-negotiable #2,
  routine traffic doesn't depend on GitHub. AR is routine traffic
  at scale; gh exists for invite/rendezvous only.
- **Headers, not payload, carry routing intent.** AR consumers
  should set `airc.route_class`, `airc.priority`, `airc.deadline_ms`
  on the envelope. The substrate routes on headers; it doesn't
  parse payloads.

## Open questions for Continuum

To finalize this contract:

1. What's the actual pose-stream cadence? 60? 90? 120? (Affects
   broadcast-capacity sizing.)
2. What's the actual payload envelope size? (Affects
   max-payload-bytes constant.)
3. Multi-user shared anchors — how many concurrent participants in
   one scene before degradation? (Affects mesh fan-out cost.)
4. Drop tolerance per profile — is the ephemeral-kind proposal
   correct, or does Profile A still want durable replay for
   debugging?
5. What's the failure mode when latency budget is missed? Drop the
   frame, queue it, surface the lag to the application?

## See also

- [GRID-SUBSTRATE-AUDIT.md](GRID-SUBSTRATE-AUDIT.md) — Phase 3.6
  perf hotpaths that block AR-scale workloads.
- [ROBUSTNESS-INTEGRATION-PLAN.md](ROBUSTNESS-INTEGRATION-PLAN.md)
  — route graph (LAN / Tailnet / relay) data-plane companion.
- [DATA-MODEL-REFERENCE.md](../DATA-MODEL-REFERENCE.md) — `events`
  table schema; relevant for ephemeral-kind discussion.
