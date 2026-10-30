# Network Authority: every send is a claim, every grant is measured

Status: design, card e873e7d8. Agreed order (card 5d773e14): this contract first, then the fixes for
addressed routing (Cormac), send cancellation (Cormac), backfill (Codex) and store open (Codex), each as a
consumer of it. Fable reviews every slice.

This is continuum's ONE-BUDGET (`docs/architecture/ONE-BUDGET.md`, contracts locked at 96f6feadc) applied
to the network, inside airc, because airc runs without continuum. Its five safety contracts bind here
unchanged; this doc cites them rather than restating them.

## Why

airc runs on machines and links we have not seen: a Wi-Fi laptop, a 10 GbE workstation, a relay across an
ocean, a phone. Any number tuned on ours is wrong on theirs. At canary 45bcd19:

| What | Count | Measured |
|---|---|---|
| Sites that put bytes on a wire or dial | 21 | 1 governed (gh), which fails open when the governor errors |
| Loops and retries that cause traffic | 12 | 1 paced by a measurement (gh `retry_after`) |
| Queue, cap, page, deadline constants on the network path | ~32 | 0 |
| Paths that drop durable frames when full | 3 | — |
| Paths where a cancelled send tears down the link | 1 | — |
| Byte or throughput counters | 0 | — |

The one measurement we have, the delivery ledger's RTT, never sizes a send: the resolver keeps only
`healthy`, and RTT feeds diagnostics, the suspect purge and the quiet gate.

What that cost on 2026-10-30: enrico's daemon broadcast every addressed frame to every LAN peer. Every
daemon refused frames that were not its own, logging an ERROR per frame. Backfill fired one sealed
command per channel per peer on every connect, retrying its handshake on a borrowed 500 ms timer. The
result was 850 MB of log and a node cut off from the grid while every part reported its own green.

## The contract

1. **One authority per daemon.** `NetworkAuthority` owns all accounting and every decision about what
   goes on a wire, for every transport (LAN/Tailscale TCP, relay, UDP, WebRTC, gh). An embedded `Airc`
   with no daemon owns one too: airc stays separately capable. There is no second manager. `GhBudget`
   becomes the authority's gh resource. It keeps its measured inputs (local count, `retry-after`,
   `x-ratelimit-*`) and loses its own constants.
2. **Every consumer is a typed claim.** `Claim { class, link, bytes, deadline }` receives a `Grant`, a
   typed refusal, or a typed wait. Classes:
   - `Interactive`: a person or mind is waiting;
   - `Command`: addressed command or result;
   - `Control`: acks, handshakes, route control;
   - `Bulk`: backfill, catch-up;
   - `Discovery`: dial, presence, registry.

   Nothing writes to a transport without a grant: `send`, `send_to`, relay `send`, the dialer and gh
   calls take a grant as an argument, so an ungranted send does not compile.
3. **Grants come from measurement.** Per link, the window is the measured delivery rate times the
   measured round trip: acked bytes over time, from flush to typed ack. The delivery ledger already
   times the ack. It gains byte counts, and both sides of every transport report them. Per transport and
   per machine, capacity is the sum over its links, plus what the platform reports.
   - A link with no measurement yet is in the typed state `Unmeasured` (ONE-BUDGET contract 3). Its
     grant is one frame in flight, the least that can produce a measurement. That is a feasibility
     floor (contract 5), not a tuned window.
   - Stale measurements are typed too. The authority never invents capacity to fill a gap.
4. **A shrinking grant is the backpressure** (contract 1). It is a request to the consumer, which
   releases. Nothing is dropped silently.
   - A claim that cannot be granted by its deadline gets `Refused { why }`, and its caller decides.
   - The three silent-drop queues become claims. Their bounds are grants, measured in bytes, not frames.
5. **Classes share by priority, with aging.** `Interactive` and `Command` preempt `Bulk`, and `Bulk` is
   never starved. This is the same aging rule as continuum goal 9.
   - gh pressure affects only the gh resource. It never degrades LAN or relay delivery, which is the
     rule from `INVITE-ROUTING-ARCHITECTURE.md` § GitHub Governor Semantics, now owned here.
6. **Cancellation is per claim, never per link.** A caller's deadline withdraws its frame if the frame
   has not reached the wire. A frame already mid-write completes and its result is discarded. The link
   survives. That deletes the write-loop teardown (`connection.rs:289-293`).
7. **No timer without a missing event named.** Every periodic loop in the inventory either becomes the
   event it stands in for, or becomes a claim whose cadence comes from measured loss or RTT.
   - Identity republish: removed when the identity changes.
   - Route refresh: triggered by connect, disconnect and endpoint change.
   - Backfill: triggered when a peer's cursor is behind, as one claim per peer, not per channel.
   - Presence and heartbeat: become liveness claims sized from measured loss.

   A timer whose missing event cannot be named is the defect to report.
8. **Addressed traffic is unicast to its host.** The directory (identity → host machine, owned
   separately as card 5d773e14 item 1) names the link, and the authority grants it. No frame for one
   identity goes to every peer.
9. **One accounting, visible.** The authority publishes a snapshot (CONCURRENCY-STYLE-GUIDE shape: own
   task, `watch::Sender<Snapshot>`) with, per link and class:
   - queued bytes, in-flight bytes and grants;
   - retransmits;
   - refusals by reason;
   - measured rate and RTT;
   - connection age.

   `airc doctor` and `delivery_stats` read the snapshot. Per-frame log lines become counters on it.

## Ratchets

Each count is driven to zero, not just held flat:
- writer calls outside the authority (21 → 0);
- network-path constants (~32 → 0, apart from protocol limits such as frame-size maxima, each named);
- timers without a named missing event (12 → 0);
- silent drops (3 → 0);
- broadcast for a single-peer target (1 → 0);
- cancellation that tears down a link (1 → 0).

They follow the shape of continuum #5609's ratchet: sites counted from source, CI fails on a rise.

## Slices

Each slice lands with a validator that runs in seconds without live peers, then a 15-minute live read on
two nodes before the next slice.

| # | Slice | Owner | Done when |
|---|---|---|---|
| N0 | This contract agreed | Cormac, Codex, Fable | Codex and Fable approve |
| N1 | `NetworkAuthority` core: claims, grants, `Unmeasured`, snapshot; ledger byte and RTT measurement on LAN and relay | Cormac | Grant-from-measurement unit tests; snapshot visible in `doctor` |
| N2 | LAN and relay writers take grants. Per-claim cancellation. Byte-bounded queues replace the 256-frame mpsc | Cormac | Cancel test: link survives; ratchet counts drop |
| N3 | Directory + addressed unicast (item 1) as consumers | Cormac | Three-machine test: a third party receives no addressed frame |
| N4 | Backfill as a per-peer `Bulk` claim. Its event replaces the per-connect, per-channel fan-out (item 3) | Codex | Reconnect storm test: one claim per peer |
| N5 | gh as the authority's resource. Work and PR gh clients migrated. Fail-open removed | Codex | gh ratchet at 0 |
| N6 | Timers: each replaced by its event or a measured claim | Codex, Cormac | Timer ratchet at 0 |

Item 4 (store open must not migrate or derive before bind, and readers never derive) is store work,
not network work. It stays Codex's under card 5d773e14 and only uses this contract's snapshot.

## Not in this design

- Continuum's inference, memory and disk: those are ONE-BUDGET's.
- The wire format and the seal: those are `airc_protocol::addressed`.
- Trust tiers and authorization: those are the ingress contract.
