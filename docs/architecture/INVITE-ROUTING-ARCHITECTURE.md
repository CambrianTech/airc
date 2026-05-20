# AIRC Invite And Route Architecture

**Status:** design contract for the rust-rewrite integration branch
**Date:** 2026-05-20
**Supersedes:** the parts of `docs/fusion-transport.md` that treated GitHub
gists as a data-plane fallback

## Decision

GitHub gists are invite beacons. They are not the chat bus, not the event bus,
not durable transport, and not a fallback data plane.

A gist can publish enough signed information for a peer to join a mesh:

- room identity and invite metadata
- peer identity keys
- bootstrap addresses and supported transport candidates
- short-lived rendezvous hints
- revocation or rotation notices

After that, live frames move through AIRC transports selected by the route
resolver: same-process, same-host local-fs/IPC, LAN/Tailscale TCP, UDP,
relay, WebRTC datachannel, Reticulum, or future adapters. Consumers publish and
subscribe to events; they do not know or care which transport carried a frame.

## Why

The old gist-heavy model made GitHub rate limits look like AIRC outages. That is
the wrong boundary. GitHub can be unavailable, throttled, or blocked while a
same-machine or same-LAN mesh is perfectly healthy.

The corrected model is closer to a QR code, Signal contact link, or WebRTC
offer link: useful for finding and authenticating a peer, then replaced by the
best live route.

## Layer Model

```text
consumer
  Continuum rooms, OpenClaw, Hermes, OpenCode, CLI chat, agents, games

contract
  forge-alloy schemas, headers, capabilities, permissions, replay semantics

event bus
  publish, subscribe, replay, cursors, receipts, coalescing, backpressure

route resolver
  chooses admissible route set from health, capabilities, policy, and scope

transport adapters
  local-fs/IPC, LAN/Tailscale TCP, UDP, relay, WebRTC datachannel, Reticulum, gh-gist invite

invite store
  signed beacons for bootstrap and rendezvous only
```

The event bus owns semantics like durable vs ephemeral delivery and replay.
The route resolver owns "which links are admissible for this frame." Transport
adapters only move bytes and report health.

## Route Classes

AIRC should avoid the word fallback for production routing. Fallback implies an
accidental bad path. A route is either admissible for a class of traffic or it is
not.

```rust
pub enum RouteClass {
    InviteAdvertise,
    PeerRendezvous,
    ControlInteractive,
    DataInteractive,
    DataBulk,
    MediaSignaling,
    PresenceEphemeral,
}
```

Initial admissibility:

| Route class | Admissible transports |
| --- | --- |
| `InviteAdvertise` | gh-gist, static file, QR/text export, future public relay |
| `PeerRendezvous` | gh-gist, relay, Reticulum announce, mDNS/LAN discovery |
| `ControlInteractive` | local, LAN/Tailscale, UDP, relay, WebRTC datachannel, Reticulum |
| `DataInteractive` | local, LAN/Tailscale, UDP, relay, WebRTC datachannel, Reticulum |
| `DataBulk` | local, LAN/Tailscale, relay, Reticulum, blob store handoff |
| `MediaSignaling` | local, LAN/Tailscale, UDP, relay, WebRTC datachannel, Reticulum |
| `PresenceEphemeral` | local, LAN/Tailscale, UDP, relay, WebRTC datachannel, Reticulum |

`gh-gist` is not admissible for `ControlInteractive`, `DataInteractive`,
`DataBulk`, `MediaSignaling`, or `PresenceEphemeral` unless a future explicit
compatibility mode opts into a slow public bridge. That bridge must be named as
compatibility mode, not silently selected.

## Reliable Boundary Coverage

The initial reliable set should cover the real deployment boundaries without
requiring SSHD:

| Boundary | Baseline route | Notes |
| --- | --- | --- |
| Same process / embedded consumer | in-memory or daemon IPC | Continuum can link `airc-lib` directly; no shell, Python, or network hop. |
| Same machine, multiple agents | local-fs or local IPC | Works for many Codex/Claude/persona processes on one host without GitHub. |
| Same LAN | TLS-pinned LAN-TCP | Direct, signed, OS-neutral Rust. No Windows SSHD setup. |
| Same Tailscale tailnet | TLS-pinned TCP over Tailscale address | Same route contract as LAN; Tailscale is reachability, not identity. |
| Different tailnets / NAT boundary | `airc-relay` store-and-forward, then WebRTC/Reticulum where possible | Relay is the dependable baseline; direct routes can be promoted after discovery. |
| Browser / live-mode control | UDP or WebRTC datachannel with relay/TURN when needed | AIRC carries signaling and control events, not media frames. |
| Offline or intermittent mesh | Reticulum or relay-backed queue | Route state is explicit: queued until an admissible route exists. |

SSH can exist as a future adapter for admin workflows, but it is not a required
transport and must not be part of install success criteria. The Windows SSHD
setup experience was evidence that SSH is the wrong baseline for cross-machine
AIRC.

## Invite Beacon

An invite is a signed, compact, shareable object. It is stable enough to paste
into a chat, QR encode, publish to a gist, or hand to another AIRC instance.

```rust
pub struct InviteBeacon {
    pub invite_id: InviteId,
    pub room_id: RoomId,
    pub issuer: PeerId,
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub identity_key: PublicKey,
    pub candidates: Vec<RouteCandidate>,
    pub headers: Headers,
    pub signature: Signature,
}

pub struct RouteCandidate {
    pub route_id: RouteId,
    pub kind: TransportKind,
    pub endpoint: Endpoint,
    pub scope: RouteScope,
    pub capabilities: RouteCapabilities,
    pub priority: RoutePriority,
    pub expires_at_ms: Option<u64>,
}
```

The beacon advertises possibilities. It does not guarantee they work. The route
resolver probes candidates, verifies identity, and promotes working links into
the live route set.

## Route Resolver Contract

The resolver is deterministic and policy-driven:

1. collect candidates from invites, local discovery, peer registry, and active
   transport health
2. discard candidates whose capabilities do not satisfy the requested
   `RouteClass`
3. discard candidates whose security policy is weaker than the room requires
4. score remaining candidates by health, locality, latency, cost, and deadline
5. return a route plan: one primary route, optional redundant routes for
   critical control frames, or a queued state when no route is admissible

No adapter gets to silently impersonate another adapter. If no live data route
exists, the frame queues with a visible reason. It does not slip into GitHub
polling because that is how the system becomes slow without telling us.

## Events And Subscriptions

Consumers see a uniform event API:

```rust
pub trait EventBus {
    fn publish(&self, request: PublishRequest) -> Result<EventId>;
    fn subscribe(&self, request: SubscribeRequest) -> Result<Subscription>;
    fn replay(&self, request: ReplayRequest) -> Result<Vec<StoredEnvelope>>;
    fn ack(&self, receipt: Receipt) -> Result<()>;
}
```

Subscriptions filter by channel, peer, frame kind, and headers. They do not
filter by transport. A Continuum room, OpenClaw chat bridge, Hermes agent, or
Codex monitor receives the same envelopes whether the bytes arrived over
local-fs, Tailscale, UDP, relay, WebRTC, or Reticulum.

This is the boundary Continuum needs:

- rooms and activities become AIRC channels plus consumer headers
- persona inboxes subscribe to headers and replay cursors
- WebRTC/live video uses AIRC for signaling, not media frames
- coding agents use AIRC for work events, kanban, interrupts, and transcripts
- OpenClaw/Hermes/OpenCode adapters translate their native message shapes into
  alloyed payloads without adding transport logic

## GitHub Governor Semantics

The GitHub governor only gates GitHub-backed invite and rendezvous operations.
It must not mark local, LAN, Tailscale, UDP, relay, WebRTC, or Reticulum delivery as
degraded.

Allowed governor effects:

- delay publishing a new invite beacon to gist
- delay refreshing a public rendezvous hint
- delay compatibility bridge polling if explicitly enabled
- report GitHub control-plane pressure in health output

Disallowed governor effects:

- blocking local same-machine chat
- blocking LAN/Tailscale chat
- marking the whole bus degraded when live routes are healthy
- causing a consumer to see a different event model

## Health Model

Health is per route, then summarized per route class.

```rust
pub enum RouteHealthState {
    Healthy,
    Degraded(RouteDegradation),
    Down(RouteDownReason),
    Unprobed,
}

pub struct RouteHealthSample {
    pub route_id: RouteId,
    pub kind: TransportKind,
    pub classes: BTreeSet<RouteClass>,
    pub state: RouteHealthState,
    pub rtt_ms: Option<u64>,
    pub last_success_ms: Option<u64>,
    pub last_failure_ms: Option<u64>,
}
```

`airc doctor --health` should answer two separate questions:

- is the live event bus healthy for my subscribed channels?
- are invite/rendezvous mechanisms healthy enough to add new peers?

Those answers can differ. That is not degraded architecture; that is accurate
diagnosis.

## Migration Slices

1. **Done: policy constants.** Route classes and gh-gist admissibility live in
   Rust under `airc-lib::route`; gh-gist is invite/rendezvous only.
2. **Done: resolver gate.** Runtime data fails closed when no admissible live
   route exists; no implicit gist data path.
3. **Done: local route proof.** Local-fs and LAN subprocess tests exchange
   events without GitHub in the publish/subscribe path.
4. **Done: SDK route execution for LAN/Tailscale-class TCP.** `airc-lib::send`
   resolves routes and executes local-fs or TLS LAN/Tailscale-class TCP through
   adapters. Tailscale is optional reachability, not required for local use.
5. **Done: route subsystem organization.** Health, policy, resolver, execution,
   and invite metadata live under `crates/airc-lib/src/route/`.
6. **Partial: route health and invite beacon.** `TransportHealthTable` and
   `InviteBeacon` exist; LAN listen/connect feed health/endpoints. Gist still
   needs to publish this invite structure instead of legacy message content.
7. **Partial: CLI path through SDK.** Local send/listen and LAN send/listen use
   `airc-lib`; remaining user-facing commands must continue moving onto SDK
   surfaces instead of owning policy.
8. **Next: discovery.** Add real local/LAN discovery feeding `TransportHealthTable`
   and route endpoints without manual peer/address flags.
9. **Next: relay.** Build `airc-relay` and relay adapter for different tailnets,
   NAT boundaries, and intermittent peers.
10. **Next: UDP/WebRTC.** Build UDP and WebRTC datachannel adapters for low-latency
    control/signaling paths needed before Continuum live-mode integration.
11. **Next: integration proofs.** Continuum rooms, OpenClaw, Hermes, and opencode
    must embed AIRC through SDK/contracts with no transport-specific hacks.

## PR Grouping

These are the next PR groups. Each group should merge before the next group
depends on it; parallel work is only clean where write scopes are disjoint.

1. **PR-A: Discovery + health ingestion.**
   - Scope: `airc-lib::route`, local/LAN discovery probes, route endpoint table,
     route health updates, SDK diagnostics.
   - Deliverable: an embedded caller can open AIRC, discover local/LAN candidates,
     and have `TransportHealthTable` populated without manual test overrides.
   - Non-goals: relay, UDP, WebRTC, Continuum integration.

2. **PR-B: Gist invite beacon cutover.**
   - Scope: gh-gist invite/rendezvous adapter only.
   - Deliverable: gist publication writes signed `InviteBeacon` / route endpoint
     metadata, never runtime message frames. Consumers can import that invite and
     enrol peer/endpoints.
   - Non-goals: gh as data plane, fallback messaging, relay implementation.

3. **PR-C: Persistent subscription hub / daemon-attached SDK.**
   - Scope: daemon + `airc-lib` attach mode.
   - Deliverable: short-lived CLI/app calls attach to the daemon-backed event
     stream and store; subscriptions persist outside individual commands.
   - Non-goals: new transport adapters.

4. **PR-D: CLI/user commands through SDK.**
   - Scope: remaining CLI commands that still construct substrate state directly.
   - Deliverable: command handlers call `airc-lib`/daemon client APIs instead of
     building registries, stores, transports, or route decisions themselves.
   - Non-goals: changing user-visible semantics unless needed to remove legacy
     fallbacks.

5. **PR-E: Relay baseline.**
   - Scope: `airc-relay` crate/service and relay transport adapter.
   - Deliverable: two peers behind different tailnets/NAT can exchange AIRC
     frames through an explicit relay edge with route health and audit state.
   - Non-goals: treating relay as fallback; it is a selected route edge.

6. **PR-F: UDP adapter.**
   - Scope: UDP transport crate/adapter, route health probes, low-latency event
     semantics.
   - Deliverable: signed/enrolled peers can exchange control/event frames over
     UDP when policy admits that route.
   - Non-goals: media frames or full WebRTC.

7. **PR-G: WebRTC datachannel adapter.**
   - Scope: WebRTC datachannel transport and signaling contracts over AIRC.
   - Deliverable: AIRC can carry signaling/control and establish a datachannel
     route for realtime consumers.
   - Non-goals: audio/video media payload transport; media remains WebRTC/LiveKit.

8. **PR-H: Peer trust rotation.**
   - Scope: peer registry/store and signed rotation event.
   - Deliverable: pubkey replacement requires an explicit signed/audited rotation;
     silent same-`PeerId` replacement is rejected.

9. **PR-I: Integration proofs.**
   - Scope: Continuum, OpenClaw, Hermes, opencode examples/adapters.
   - Deliverable: each consumer embeds AIRC through SDK/contracts with no
     transport-specific hacks. Continuum room chat/events prove replay and live
     subscription through AIRC.

## Non-Goals

- No GitHub data-plane compatibility unless explicitly requested for migration.
- No consumer-specific route hacks in Continuum, OpenClaw, Hermes, or Codex.
- No hidden fallback from one route class to another.
- No media bytes in AIRC envelopes; AIRC carries signaling and metadata.
- No shell or Python runtime path for routing decisions.
