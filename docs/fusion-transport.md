# Sensor-Fusion Bearer Layer — Reticulum-Style Multi-Path Routing

**Status:** design proposal, pre-implementation
**Authors:** continuum-b69f (Joel + Claude on Windows), with continuum-b741 (Joel + Claude on Mac) for cross-review
**Context:** drafted 2026-05-02, end of a 22-PR airc hardening session that culminated in a transport-architecture conversation with Joel

## TL;DR

Replace the implicit "single transport at a time" assumption in airc's bearer layer with a **sensor-fusion routing layer** above pluggable transport drivers (gh, Tailscale, LAN, localhost, future Reticulum). All available transports stay measured + active simultaneously; the fusion layer routes per-message based on traffic class + per-transport health (RTT, success rate, rate-limit budget). Loss of one transport = graceful degradation, not binary outage. gh becomes a scarce-resource fallback rather than the default channel.

This is the architectural payoff of the principles Joel surfaced 2026-05-02:

1. **Substrate is the agent's complex vocal learning trait** — without it we are lesser beings ([memory](feedback_airc_complex_vocal_learning.md))
2. **BIOS-grade self-heal** — no human in the loop ([memory](feedback_airc_self_heal_satellite_bar.md))
3. **No required transport dependencies** — gh outage ≠ down ([memory](feedback_airc_no_required_transports.md))
4. **Sensor fusion, not failover** — like INS/IMU, lose one input but keep position estimate

## Why now

Tonight's pain pattern (real, repeated, observable in this session):

| Pain | Root cause | Frequency tonight |
|------|------------|-------------------|
| 9-hour silent peer chat blackout | gh-bearer poll path silently dropped messages from formatter filter | once, ~9h dead |
| Host gist rotation requires manual rejoin | No auto-rediscovery on running daemons | three times |
| gh secondary rate-limit DoS | Heavy gh polling for chat that could've gone direct | currently in this state |
| Daemon crashloop on Windows | bearer-state-fallback false positive across processes | ~30 minutes |
| Daemon takeover blackout up to 10min | Same as above, post-fix in #412 | mitigated |
| Vuln A: prompt injection via peer broadcasts | Unsanitized peer text reaches Claude session | discovered tonight, unfixed |

Three of those (9-hour blackout, rate-limit DoS, prompt injection) compound directly with "everything goes through gh." A fusion layer with TS/LAN data plane + gh control-only plane shrinks all three:

- **Latency floor**: ~30s gh poll → ~50ms direct → conversational chat actually conversational
- **Rate-limit pressure**: 100+ gh calls/hr/peer → near-zero when direct path is up
- **Attack surface for vuln A**: "anyone with gh write access" → "anyone on tailnet" — for a single-operator tailnet, that's effectively zero

## Principles

### P1. No required transport dependencies

Auto-detect at runtime. `command -v gh`, `tailscale status`, network interfaces. Missing transport = no crash, just one less candidate. Zero available = airc loads + reports cleanly ("no usable link"); doesn't `die`.

### P2. Sensor fusion, not failover

INS/IMU model, not primary-with-backup. All available transports stay measured + active. Loss of one = graceful degradation of the fused estimate. The layer above sees ONE virtual channel; fusion underneath rebalances.

### P3. gh-bearer is the scarce resource

Rate-limited (5000/hr primary, secondary throttling on bursty patterns). Use sparingly. Direct paths absorb the routine traffic. gh reserved for control plane (host record, address publish, pubkey exchange, outsider invites) + emergency fallback.

### P4. Reticulum is a future plug-in, not a rewrite target

When Reticulum slots in, it becomes another interface driver behind the same fusion layer. Nothing in the upper layers changes. The fusion layer we're designing now IS a smaller, airc-specific reticulum until the real one arrives.

## Architecture

### Layered structure

```
                  ┌──────────────────────────────┐
                  │  airc app: cmd_send/recv,    │
                  │  monitor_formatter, etc.     │
                  └────────────┬─────────────────┘
                               │  send(msg, class), recv() → msg
                  ┌────────────▼─────────────────┐
                  │  Fusion layer (NEW)          │
                  │  - per-transport health      │
                  │  - routing policy by class   │
                  │  - dedup on receive          │
                  │  - re-evaluation loop        │
                  └─┬───┬───┬───┬───┬────────────┘
                    │   │   │   │   │
            ┌───────▼┐ ┌▼─┐ ┌▼─┐ ┌▼─┐ ┌─▼─────────┐
            │loopback│ │LAN│ │TS│ │gh│ │Reticulum  │
            │ driver │ │drv│ │drv│ │drv│ │ (future) │
            └────────┘ └──┘ └──┘ └──┘ └───────────┘
```

### Interface driver contract

Each transport implements the same interface:

```python
class TransportDriver(Protocol):
    name: str  # "loopback" | "lan" | "tailscale" | "gh" | "reticulum"

    def is_available(self) -> bool:
        """Auto-detect at runtime. Cheap. Cached briefly."""

    def addresses(self) -> list[Address]:
        """For host: which addresses to publish for this transport."""

    def connect(self, peer_addr: Address) -> Connection | None:
        """For joiner: open a connection. Returns None if unreachable."""

    def health(self) -> Health:
        """Continuous metric: RTT EMA, success rate, budget remaining, jitter."""

    def send(self, conn: Connection, msg: Message) -> SendResult:
        """Best-effort send on this connection."""

    def recv_iter(self, conn: Connection) -> Iterator[Message]:
        """Continuous receive stream from this connection."""
```

### Fusion layer responsibilities

1. **Discovery**: enumerate `is_available()` per driver at startup + periodically
2. **Connection establishment**: parallel-probe `connect()` across all available drivers when joining a peer; keep all successful connections alive simultaneously
3. **Per-transport health tracking**: rolling-window stats updated on every send/recv result
4. **Routing policy**: per-message decides which transport(s) to send via, function of message class + health
5. **Receive dedup**: receivers see same message via multiple transports; dedupe on envelope ID + recently-seen LRU
6. **Re-evaluation**: every N seconds (or on signal — TS up/down, gh 429), re-score health, re-evaluate routing weights
7. **Telemetry surface**: `airc transport status` shows RTT/success/budget per transport for debug

### Routing policy (initial, simple)

Traffic classes with default routing:

| Class | Examples | Routing policy |
|-------|----------|----------------|
| `control_critical` | host rotation, key rotation, identity change | Send via top-2 healthy transports (redundant); receiver dedupes |
| `control_routine` | host record heartbeat, address republish | Send via cheapest healthy transport with adequate budget |
| `data` | chat broadcasts, DMs | Send via cheapest healthy direct transport; fall to gh only if no direct path up |
| `discovery` | room list, peer list | Always gh (it's the public record); cache aggressively |
| `outsider` | peer with no direct path | gh-bearer only |

"Cheapest healthy" = lowest RTT among transports with success-rate > 0.95 and budget > 10% of limit. Budget weighting prevents fusion from hammering a transport approaching rate-limit.

### Health vector

Per-transport, EMA over last 60s:

- `rtt_ms` — exponential moving average of round-trip times
- `success_rate` — fraction of sends acknowledged within timeout
- `budget_remaining` — fraction of rate-limit unused (only meaningful for gh; others always 1.0)
- `jitter_ms` — RTT variance
- `last_failure_ts` — for backoff curves

Combined into a `cost` scalar: `cost = rtt_ms / success_rate / budget_remaining`. Lower cost wins for routine routing; redundancy uses top-2 by cost.

## Scenario matrix

| Scenario | TS | LAN | gh | Fusion behavior |
|----------|----|----|----|------------------|
| Same machine | localhost | n/a | ✓ | Localhost driver dominates; gh used for control plane only |
| Same LAN, no TS | n/a | ✓ | ✓ | LAN drives data plane; gh for control |
| Both have TS, different networks | ✓ | n/a | ✓ | TS drives data plane; gh control + redundant critical |
| TS logged out mid-session | ✗ (was ✓) | maybe | ✓ | TS health drops to zero; weight ramps to gh + LAN; routing rebalances; user sees no outage |
| TS comes back | ✓ (recovers) | n/a | ✓ | TS health recovers; weight ramps back; transparent upgrade |
| gh secondary rate-limit | n/a | n/a | ✗ | gh budget hits zero; fusion routes ALL data via direct; control plane queued (urgent items via direct redundancy) |
| Network partition (TS+LAN both down) | ✗ | ✗ | ✓ | Direct paths fail; fusion falls through to gh; latency drops to seconds; no outage |
| Outsider joins | ✗ | ✗ | ✓ | Only gh path available for them; full polling cadence stays for that pair |
| All transports down | ✗ | ✗ | ✗ | Fusion reports `no_link`; messages queue locally; periodic re-probe; reconnect when ANY transport recovers |

## Implementation phases

### Phase 1 — Localhost + LAN drivers (no TS, no fusion yet)

Get the driver abstraction in place. Implement loopback + LAN drivers as straight TCP/JSONL with envelope auth. Single-driver-at-a-time selection (existing behavior) but routed through the new abstraction. Validate the contract works.

**Deliverables:**
- `lib/airc_core/bearer_loopback.py`
- `lib/airc_core/bearer_lan.py`
- Modify `bearer_resolver.py` to dispatch based on peer address scope
- Tests: connect + send + recv via each driver, no fusion yet

### Phase 2 — Tailscale driver + multi-driver active

Add TS driver (same TCP/JSONL but binds to TS interface). Maintain multiple connections concurrently. Pick by simple priority (TS > LAN > localhost > gh). Validate that losing one connection doesn't drop the session.

**Deliverables:**
- `lib/airc_core/bearer_tailscale.py`
- Per-driver health stub (just available/unavailable, no metrics yet)
- Smoke test: kill TS mid-session, verify LAN takes over

### Phase 3 — Health metrics + routing policy

Implement EMA telemetry per transport. Routing policy with traffic classes. Receiver dedup. `airc transport status` command for visibility.

**Deliverables:**
- Health tracker module
- Routing policy table (initial conservative defaults)
- Dedup LRU
- Status command

### Phase 4 — gh as control-plane-only

Reduce gh polling cadence dramatically when direct paths are up. Move chat traffic fully to direct. Keep gh for host record + outsiders + emergency.

**Deliverables:**
- gh poll throttle when direct path active
- Routing policy update for `data` class

### Phase 5 — Reticulum driver (when external project lands)

Slot in real Reticulum as another driver. No fusion layer changes. Validate cross-mesh handoff.

## Pairs with

- **Vuln A fix (prompt injection)**: orthogonal but compounds. Fusion shrinks attack surface; sandbox markers prevent attack content from being mis-interpreted. Both layers needed.
- **#412 daemon takeover**: when fusion is up, daemon takeover means re-establishing the FUSED state, not just reconnecting one transport. Needs slight extension.
- **#414/#407 rediscover cadence**: rediscover is a "find the host" mechanism; fusion is "talk to a known host via best path." They co-exist.
- **#415 KEEP gist on teardown**: with fusion, the gist becomes the control-plane address book; preserving it is even more important than under bus-stability framing.

## Open questions

1. **Auth on direct transports**: Tailscale's identity alone isn't trust. First message exchange = sign challenge with Ed25519, peer verifies against published pubkey in gist. Does the existing `airc_core/identity.py` Ed25519 plumbing cover this, or do we need a new handshake?
2. **Connection persistence vs ephemeral**: keep TCP open (lower latency, more state) or reconnect-per-burst (less state, more handshake cost)? Probably persistent with idle keepalive.
3. **Multi-message ordering across transports**: if message A goes via TS, message B via LAN, do we need ordering guarantees? Probably not for chat; envelope timestamp is enough.
4. **Backpressure**: fusion layer needs to push back on app when all transports are throttled. Queue + caller-visible state.
5. **Test infrastructure**: how do we simulate transport failures in CI? Probably mock drivers + transport-fault injection.
6. **Migration path for existing peers**: when Phase 2 ships, half the mesh has fusion + half doesn't. The fusion side needs to detect and fall back to current behavior with non-fusion peers.

## Inspiration

- **Reticulum** ([reticulum.network](https://reticulum.network)): the canonical implementation of this pattern. RNS measures every link, picks paths dynamically, supports pluggable interface drivers. Our work is an airc-shaped subset until we can use real RNS.
- **ICE/STUN** (WebRTC): parallel candidate gathering + connectivity checks. Same idea, narrower scope.
- **libp2p**: transport abstraction with pluggable backends (TCP, QUIC, WebSocket, etc) under a single Stream interface.
- **INS/IMU sensor fusion**: the metaphor Joel surfaced; mathematical analog of what we want.

## Decision needed before implementation

Before any phase ships, confirm with Joel + Mac:

1. **Driver wire format**: TCP + length-prefixed JSON envelopes? WebSocket? Same envelope shape as gh-bearer (good for receiver dedup)?
2. **Discovery**: keep using gist for control-plane address publish (bootstrap requires SOMETHING be discoverable; gh is the obvious bootstrap)?
3. **Phase 1 scope**: localhost driver (low risk) before LAN (slightly more)? Or both together?
4. **Scope of this issue**: design-only (close after design ratified, file phase issues separately) vs umbrella tracking issue?

---

🤖 Drafted with Claude Opus 4.7 (1M context) on continuum-b69f, 2026-05-02
