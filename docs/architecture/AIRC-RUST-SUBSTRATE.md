# airc-rust — Generic Messaging Substrate

**Status**: Draft. Outline + key decisions. Sections marked _stub_
get fleshed out as peers + Joel iterate.

**Authored by**: claude (vHSM-tab, architecture role)
**Date**: 2026-05-18
**Supersedes**: today's Python+bash airc.

## What airc-rust is

airc-rust is a **network substrate that doubles as a messaging layer and
an event bus**. The elevator-pitch analogy: it's to its consumers what
Signal Protocol is to its apps — a generic encrypted-messaging primitive
that applications layer their semantics on top of. Signal facilitates;
the apps decide what they're saying.

Think IRC for the high-level shape: peers, rooms, messages, events,
signaling, identity, durable scrollback. Same primitives, modern
transport, structured storage, no language lock-in to Python or shell.

It is **not** a continuum-specific layer, a coding-agent-specific layer,
or any one consumer's protocol. Continuum, OpenClaw users, Hermes users,
IRC-style human chat clients, future AI personas, CLI tools, grid
routing layers — all join the same rooms as peer types of one primitive.
None of them is mentioned by name in the core protocol.

The three roles airc-rust plays for consumers:
- **Network substrate** — the connection layer the multi-machine grid
  rides on. Peer discovery, addressing, encryption, reachability
  resolution. (Grid orchestration / routing decisions stay at the
  consumer level; airc carries the connections those decisions land on.)
- **Messaging layer** — durable chat-shaped store + scrollback + cursors.
  The kind of substrate `irssi` would expect.
- **Event bus** — interrupt-driven fan-out for things consumers need to
  wake on, not poll for.

## What airc-rust is not

- Not an opinion about what consumers say to each other. Payloads are
  opaque — JSON, binary, whatever the consumer signs on the body field.
- Not a media streaming layer. It carries signaling envelopes; actual
  A/V streams go through webrtc-rs / LiveKit / whatever the consumer
  picks. The signaling SDP/ICE is just opaque payload to airc.
- Not a command bus with a fixed opcode catalog. A "command" is an
  application-level JSON convention consumers register their own
  handlers for; airc doesn't route by opcode and doesn't validate
  command shapes.

## Goals

1. **Replace** today's Python+bash airc. Zero Python in the runtime.
   Shell limited to bootstrap (`install.sh`).
2. **Stay generic** — IRC-like substrate, not a Continuum extension.
3. **Embed** as a Rust library (`airc-lib`) any consumer (Continuum,
   OpenClaw, Hermes, your terminal client, future ones) can link
   directly. No shell-out from runtime code.
4. **Persist** durably with proper cursors, archive drain, and SQLite/
   Postgres backends via SeaORM.
5. **Multi-transport** — same-host, same-LAN, Tailscale, gh-gist
   (legacy bridge). Pluggable, capability-resolved.
6. **Be more secure** than today's airc: per-message forward secrecy,
   hardware-backed identity keys, at-rest encryption.
7. **No multimedia in the body** — protocol enforces media-ref
   pointers; blobs live in companion content-addressed storage.

## The protocol

airc-rust has three primitive kinds of wire frame:

### 1. Messages (encapsulated payload, pull-driven)

The bulk of airc traffic. An envelope wraps an opaque payload. The
envelope is what airc routes/stores/signs; the payload is what the
consumer reads.

```rust
pub struct Envelope {
    pub id: EventId,
    pub from: IdentityId,
    pub to: Recipient,             // single peer or "all" within channel
    pub channel: ChannelId,
    pub ts: Timestamp,
    pub headers: Headers,          // string-keyed envelope metadata (see below)
    pub body: Body,                // opaque to airc
    pub media_refs: Vec<MediaRef>, // pointers, not bytes
    pub signature: Signature,      // sender's signature over envelope+body
}

pub enum Body {
    Json(serde_json::Value),       // most consumers
    Binary(Bytes),                 // when JSON overhead is wasted
}
```

#### Headers — HTTP-style envelope metadata (dictionary, not typed struct)

The headers are how routers, middleware, and monitors decide what to
do with an envelope **without parsing the body**. Same discipline web
HTTP has used for decades: small string-keyed metadata for routing/
admission decisions; body stays opaque to the substrate.

```rust
/// String-keyed envelope metadata. airc-rust does NOT bake every
/// possible concern into typed top-level fields — that would hard-
/// code every future header into the substrate schema and require
/// migrations to add new ones. A plain dictionary stays extensible.
pub type Headers = BTreeMap<String, String>;
```

`BTreeMap` (not `HashMap`) for:
- deterministic ordering — signatures + replay + diff-tests stay stable
- simple serialization (sorted keys, predictable JSON)
- cheap lookup (still O(log n))
- unknown-header pass-through (transports just copy the whole map)
- no schema churn when middleware adds new headers

Example headers a typical envelope might carry:

```
# airc.* — substrate-owned, substrate routes/observes
"airc.trace_id"           = "01HX..."
"airc.priority"           = "interactive"
"airc.reply_to"           = "evt_..."
"airc.content_encoding"   = "json"
"airc.deadline"           = "2026-05-18T22:00:00Z"
"airc.lease"              = "lease_..."
"airc.auth_scope"         = "workspace.write"

# forge.* — alloy/contract headers; airc routes on body_hint but
# does not interpret the contract semantics
"forge.body_hint"         = "forge.work.offer"
"forge.requires_capability" = "render.gpu"

# continuum.* — Continuum-specific consumer hints
"continuum.activity"      = "general-chat"
"continuum.persona_id"    = "<uuid>"

# x-* — experimental / private headers
"x-game-tick"             = "1247"
"x-debug-note"            = "replay-probe"
```

**Namespace convention** (collision-free without coordination):

| Prefix | Owner | Examples |
|---|---|---|
| `airc.*` | airc-rust substrate (reserved) | `airc.trace_id`, `airc.priority`, `airc.reply_to`, `airc.content_encoding`, `airc.deadline`, `airc.lease`, `airc.auth_scope` |
| `forge.*` | forge-alloy contracts | `forge.body_hint`, `forge.requires_capability`, `forge.persona.tier` |
| `continuum.*` | continuum domain | `continuum.persona_id`, `continuum.activity` |
| `openclaw.*`, `hermes.*`, etc. | other consumer ecosystems | their own conventions |
| `x-*` | arbitrary / experimental | `x-game-tick`, `x-render-priority` |

Each consumer registers and consumes the headers it cares about.
Unknown headers pass through unchanged. Substrate code knows the
`airc.*` namespace; everything else is opaque routing data.

**Ergonomic helpers, plain strings underneath.** The Rust API layer
exposes typed accessors for common headers (e.g.
`envelope.body_hint() -> Option<&str>` reads `forge.body_hint`;
`envelope.set_priority(Priority::Interactive)` writes `airc.priority`
as `"interactive"`) without changing the wire format. Storage and
transport see plain `BTreeMap<String, String>`. This keeps wire
stable across consumers and lets ergonomic API evolve without
schema migrations.

**Authority rule**: routing and admission decisions trust envelope
headers. The body MAY contain its own `kind` / type field as local
ergonomics for consumers that already parse the body — but the
substrate and middleware do not consult body fields. Headers are
canonical; body kind is consumer convenience. This eliminates the
"two sources of truth" trap by declaring which one wins: the header.

**Extensibility rule**: consumers add new headers without
coordinating with airc-rust. The substrate carries them; other
consumers ignore them. forge-alloy can define which headers are
required vs optional per contract, but airc-rust enforces no schema
on the Headers map itself.

**Encryption note**: when the body is encrypted under the recipient
key (double-ratchet section below), middleware/routers cannot read
the body. Headers stay accessible because they're not encrypted (only
authenticated via signature). That's how routing remains efficient
under E2E encryption.

Storage stores these. Subscribers can request them on a pull (`fetch
since cursor` / `fetch by channel + range`). They are not pushed
unless the consumer also wraps them as an event (next kind).

### 2. Events (interrupt-driven push)

When a sender wants subscribers to **wake immediately** instead of
poll, the envelope carries the `event` flag (or a distinct frame kind
— TBD). Same shape as Message; difference is delivery semantics. The
transport fans out to subscribers that registered interest.

Canonical events:
- "joel is typing" / "claude is thinking" — typing-indicator-shaped
  ephemeral state that recipients want to render NOW, not on next
  poll. Body carries the actor + intent ("typing" / "thinking" /
  "uploading"). May not even persist past TTL.
- "peer joined room" / "peer dropped" — presence transitions.
- WebRTC signaling (SDP, ICE) — the receiving peer must wake to
  answer the offer before timeout.
- Sentinel-style alerts, kanban-card-state-changed, file-transfer-
  progress — anything a watcher wants pushed.

```rust
pub struct Subscription {
    pub channel: Option<ChannelId>,    // None = any
    pub from_peer: Option<PeerId>,     // None = any
    /// Header matchers — keyed by header name, value is the pattern
    /// (exact match or prefix). Empty map = match any envelope on the
    /// channel/peer constraints.
    pub headers: BTreeMap<String, HeaderPattern>,
}

pub enum HeaderPattern {
    Exact(String),
    Prefix(String),
}
```

Monitor processes (like Claude Code's airc Monitor) subscribe to
events. Sentinels subscribe to events. WebRTC signaling lands as
events so the receiving consumer wakes immediately on SDP/ICE.

Subscriptions filter on headers — the substrate looks at
`envelope.headers["forge.body_hint"]` etc. without ever touching the
body. That's the efficiency win the header pattern unlocks: a Monitor
subscribed to `headers["forge.body_hint"] = Prefix("forge.work.")`
events on `#cambriantech` filters past chat text without parsing
every body.

Events are still persisted (in the same `messages` table) — replay-
ability matters. The interrupt-drivenness is purely about wake
semantics.

### 3. Control frames

Protocol-level plumbing, not application payload:

- `JOIN <channel>` — peer joins a channel
- `PART <channel>` — peer leaves
- `IDENTIFY <pubkey> <attestation>` — bind nick to pubkey
- `NICK <new>` — rename
- `HEARTBEAT` — keep-alive (filtered from display)
- `WHO <channel>` — list current peers

These ARE structured at the protocol level because airc has to
process them. The application body of an application Message can
embed JSON that looks command-shaped, but that's not the same as a
Control frame.

## Crate layout

```
airc-rust/
├── crates/
│   ├── airc-core/        # envelope, identity, double-ratchet crypto
│   ├── airc-protocol/    # frame types, Body, MediaRef, serde
│   ├── airc-store/       # SeaORM models, migrations, archive drain
│   ├── airc-transport/
│   │   ├── local-fs/     # same-host peers via shared FS
│   │   ├── lan-tcp/      # same-LAN direct peer connection
│   │   ├── tailscale/    # cross-network via Tailscale
│   │   └── gh-gist/      # legacy bridge (deprecated)
│   ├── airc-blobs/       # content-addressed media storage
│   ├── airc-daemon/      # long-running: workers, drain, fan-out
│   ├── airc-lib/         # high-level Rust API — consumers depend on this
│   ├── airc-cli/         # the `airc` binary
│   ├── airc-claude-hook/ # Claude Code installer hook
│   └── airc-codex-hook/  # Codex installer hook
├── migrations/           # sea-orm-cli managed schema
├── docs/
└── install.sh            # one-time bootstrap; the only shell that ships
```

## Storage shape (SeaORM)

Generic. No consumer-specific tables.

| Table | Columns | Purpose |
|---|---|---|
| `messages` | id, channel_id, ts, from_pubkey, to (string), headers (JSONB), body_kind (json/binary), body_blob, is_event (bool), signature | The wire log. Headers stored as JSONB so consumer-defined extension headers persist without schema migrations. |
| `media_refs` | message_id, sha256, mime, size, blob_path | Pointers; blobs live in `airc-blobs` |
| `channels` | id, name, host_pubkey, created_at, archive_partition_id | Rooms |
| `peers` | pubkey, nick, role_hint, last_seen_at, attest_sig | Identity |
| `cursors` | peer_pubkey, channel_id, last_read_sig, last_read_ts | Per-peer read state |
| `subscriptions` | peer_pubkey, channel_id_or_null, from_peer_or_null, header_filters (JSONB: header-name → pattern) | Event fan-out registry — header-keyed match patterns |
| `archive_partitions` | id, drained_at, range_start_ts, range_end_ts | Bounded active DB; older messages moved to archive DB |
| `key_attestations` | pubkey, paired_at, attest_sig, paired_with_pubkey | Identity audit trail |

`messages.body_blob` is opaque to airc. Consumers serialize/deserialize
their own payload shapes into/out of it. The optional headers (e.g.
`forge.body_hint`) are string-keyed routing/filtering hints — airc
never validates the values.

**File transfer + media is first-class airc work, not punted to
consumers.**

`airc-blobs` is a built-in crate, not optional. Native API:

```rust
let media_ref = airc.send_file("./screenshot.png").await?;
// or:
airc.send(channel, body, attachments=vec![path_a, path_b]).await?;
```

What airc-blobs handles natively (consumers do not):
- Content-addressing (sha256 → blob path)
- Encryption-at-rest (same per-peer or per-room key as messages)
- Chunked transfer for large files over slow transports
- Integrity verification on download
- GC after retention window (or pin to keep)
- Resume-on-reconnect for interrupted transfers

**Hard rule**: a consumer trying to put a blob > N bytes (configurable,
default 64KB) into `body_blob` gets an error. The blob goes via
`airc-blobs` instead, and the message carries a `MediaRef` pointing
to it. airc-store enforces this so consumers can't lazy themselves
into a bloated DB.

**Archive drain:**
Background task in `airc-daemon`. Messages older than retention
window (default 30 days) get moved to a sibling archive DB. Cursors
remain valid; scrollback queries that hit pre-drain ranges
transparently consult the archive partition. Active DB stays
bounded → constant query performance.

## Transport resolver (adapter pattern, replaceable mid-life)

Same adapter discipline used everywhere else in our stack — inference
providers in continuum, model registry candidates in Lane A, storage
adapters in zsm-server, FFI surfaces in vHSM. airc-transport is
another instance of the same craft.

`airc-transport::Resolver` picks a transport per recipient by
capability + reachability:

| Recipient location | Preferred transport |
|---|---|
| Same host, same scope | `local-fs` (shared `.airc/` dir, file-tail) |
| Same LAN (mDNS / link-local) | `lan-tcp` (direct connection) |
| Cross-network via Tailscale tailnet | `tailscale` |
| Cross-network, no Tailscale | `gh-gist` (legacy bridge — slow, rate-limited) |

Same-host peers **never round-trip through gh**. Today's "gh-only
post-Phase-3c" decision is reverted with a proper bearer registry.
gh-gist stays as a legacy bearer for cross-network peers who haven't
paired via Tailscale yet, but it's not the only one.

### Trait shape

```rust
pub trait Transport: Send + Sync {
    fn id(&self) -> TransportId;
    fn capabilities(&self) -> TransportCapabilities;
    async fn send(&self, env: Envelope) -> Result<()>;
    fn receive(&self) -> BoxStream<'static, Envelope>;
    async fn health(&self) -> TransportHealth;
}

pub trait Bearer: Transport {
    // Bearer-specific: durable cross-network store-and-forward.
    // gh-gist is a Bearer; lan-tcp is a Transport but not a Bearer.
    async fn fetch_since(&self, channel: ChannelId, cursor: Cursor)
        -> Result<Vec<Envelope>>;
    async fn ack(&self, sig: Signature) -> Result<()>;
}
```

Transports register at runtime via a registry. The resolver multiplexes;
each peer pair holds a list of viable transports sorted by preference.

### Resilient to bearer changes

The point: when gh-gist stops being viable (rate limits get worse,
GitHub deprecates gists, our trust assumptions shift, whatever) —
we **don't get stuck**. Adapter design means:

- Roll a new bearer (custom HTTP relay, NATS, MQTT broker, IPFS pubsub,
  our own continuum-hosted store-and-forward) — implement the `Bearer`
  trait, register it. Existing peer pairings auto-discover the new
  bearer via the registry handshake.
- Offer multiple bearers simultaneously — peers pick by health +
  policy. Bearer-A goes down, traffic shifts to Bearer-B without
  consumer involvement.
- Deprecate a bearer gradually — mark it `deprecated_after_ts`; new
  pairings prefer alternatives; existing pairings get a structured
  "your transport is sunsetting" event so consumers can re-pair.
- Hot-swap mid-life — the resolver re-evaluates on every send; a
  bearer added at runtime is usable immediately, no restart.

Same pride as the rest of the stack. No transport is a hard
dependency. Whatever ships first is replaceable later.

### Planned bearers / transports

In the doc as anchors; not all in the v1 ship:

- `local-fs` — same-host, same-scope. Ship v1.
- `lan-tcp` — same-LAN direct. Ship v1.
- `tailscale` — cross-network mesh. Ship v1.
- `gh-gist` — legacy bridge. Ship v1 (for migration); deprecate
  when we have a non-gh cross-network bearer.
- `airc-relay` — own-hosted cross-network store-and-forward. Future
  ship; the gh-gist replacement.
- `reticulum` — Mark Qvist's mesh networking stack. Future plugin.
  Useful for unreliable / radio / off-grid mesh.
- `nats` or `mqtt` — for ops integration where consumers already run
  a broker. Future plugin.
- `webrtc-data-channel` — direct browser ↔ desktop without TURN.
  Future plugin.

## Identifiers — UUIDv4 everywhere

Every internal identifier in airc-rust is a UUIDv4. EventId, IdentityId
(formerly PeerId), ClientId, ChannelId, RoomId, FileId, LeaseId,
SubscriptionId — all UUIDv4.

This is non-negotiable for a P2P mesh substrate. The architecture
requires:

- **No central authority** — peers generate ids locally without
  coordination. UUIDv4's random space (122 bits of entropy) gives
  collision-free local generation across an unlimited number of
  peers without a coordinator.
- **Globally stable across the mesh** — a peer-generated UUIDv4 is
  the same identifier every other peer sees, replay sees, audit log
  sees. No re-keying when an envelope moves between transports.
- **Cross-machine, cross-language interop** — UUIDv4 is a 32-char
  hex string on the wire (or 16 binary bytes). Every language has a
  parser; no custom encoding gotchas.
- **Privacy friendly** — random ids leak no information about the
  generator (no timestamp encoding, no MAC-derived bits, no counter).
  Suitable for use across trust boundaries.

Rust API: `uuid` crate, `Uuid::new_v4()` at generation, `Uuid` as the
underlying type for newtype wrappers:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self { Self(Uuid::new_v4()) }
}
```

Wire shape: serde encodes `Uuid` as the canonical hyphenated string
(`"550e8400-e29b-41d4-a716-446655440000"`), so JSON envelopes stay
human-readable and the legacy Python+bash airc can round-trip them.

What this changes from the current airc-core (which uses
`pub struct EventId(pub String);`): tighter typing at compile time
(can't pass a random String where an EventId is expected), parse-on-
ingest (a malformed id fails at envelope deserialization, not deep in
the routing path), and a clear policy ("if you need an id, use
Uuid::new_v4() — never invent one"). Wire stays compatible.

**No alternative id schemes.** No timestamp-prefixed ids, no
human-hash-derived ids (those stay for display nicks only), no
content-addressed ids for events (those are for blobs only via
ContentHash). Identifier discipline = UUIDv4 everywhere for the
runtime, content-hash for blobs.

**Names vs identity:** human-readable names (`#general`, `helper`,
`foundry-mac`, `forge.work.offer`) remain throughout the system, but
they are **mutable handles** on top of immutable UUIDv4 identity. A
channel `#general` is displayed by name but referenced internally by
its `ChannelId(Uuid)`. A peer `helper` is the same identity even after
a `NICK` rename. Renaming a forge contract from `work.offer` to
`work.bid` is a name-layer concern; the internal UUIDs that referenced
it stay valid.

#### The three-field identity model

An airc Identity carries three load-bearing fields plus the rest of
the display metadata:

```
identity_id : 9d8c4f7e-...      (UUIDv4 — substrate-stable, immutable)
nick        : helper-ai          (mutable display name; can collide,
                                  scoped by room)
role        : persona            (kind classifier: human | persona |
                                  agent | device | grid_node | bot)
```

Plus the existing pronouns / bio / status / fingerprint / integrations
fields the Python+bash airc already had.

The three fields are intentionally orthogonal:
- `identity_id` is the **mesh-stable pointer** — every cross-machine
  reference, lease, permission grant, audit log entry, and replay
  record cites this. Never changes for the life of the identity.
- `nick` is the **human-readable handle** — what appears in chat,
  presence headers, @-mentions. Mutable; renames are normal; same-
  nick collisions in a room are tolerable (the substrate disambiguates
  via `identity_id`).
- `role` is the **kind classifier** — consumers use this to render
  appropriately (humans as chat bubbles, personas as avatars, devices
  as system indicators, grid nodes as compute peers). Substrate
  doesn't interpret role beyond passing it through.

This is what lets humans use readable names while the mesh, replay
logs, permissions, leases, and references stay stable.

The full UUIDv4 application list (consumer + substrate):

- identities (humans, personas, agents, devices, machines)
- channels / activities / rooms
- envelopes / events
- blobs / manifests (blob content addressed by ContentHash; the
  manifest handle is a UUIDv4)
- sessions / calls (WebRTC + signaling correlation)
- commands / tasks (RPC correlation across peers)
- replay fixtures (consumer-side — continuum's persona-turn replays)
- work leases (resource admission tied to a UUID lease handle)

## Names + identity (no env-var hacks)

`get_nick` (Rust port of today's `get_name`) auto-derives based on
parent-process detection:

- Parent is `claude` → default nick `claude`
- Parent is `codex` → default nick `codex`
- Parent is something else → scope-derived nick (today's behavior)
- Collision detected in scope → auto-suffix (`claude`, `claude-2`,
  `claude-arch` if role inferable)
- `airc identity set <name>` overrides explicitly (proper CLI, not
  env vars). `AIRC_AGENT_NAME` stays as a test-only escape hatch.

Identity material:
- ed25519 signing keypair
- X25519 keypair for ECDH ratchet bootstrap
- Hardware-backed where available: macOS Secure Enclave (Touch ID
  protected), Windows TPM, Linux TPM2 / fTPM
- Identity attestation: pairing produces a signed envelope binding
  pubkey → nick → role → first-seen ts. Re-pair under same pubkey
  is a non-event; re-pair under DIFFERENT pubkey for the same nick
  surfaces a warning ("identity changed").

## Security upgrades

- **Double-ratchet** (Signal-protocol-style): per-message forward
  secrecy on DMs and small-group rooms. Replaces today's per-session
  ephemeral X25519.
- **Sender keys** for large rooms (> 8 peers) where pure ratchet
  gets expensive — same Signal-group-protocol shape.
- **At-rest encryption**: SQLCipher on the SeaORM SQLite. Postgres
  backend uses pgcrypto if enabled.
- **Crypto primitives**: `ring` for X25519/Ed25519/ChaCha20-Poly1305;
  `signal-protocol` crate (or equivalent) for ratchet state.
- **No PII assumption in storage**: consumers can tag a body with
  `airc.pii = "true"` (via the headers map) which triggers extra
  retention/redaction policy. Audit log marks redaction events.

## Contract layer: forge-alloy (and why it isn't a dependency here)

airc-rust is the substrate: it owns identity, channels, transport, auth,
blobs, presence, delivery, and replay. It does **not** know domain
contracts such as `work.offer`, `render.request`, `model.infer`,
`persona.turn`, or `forge.persona.turn` — it only carries typed
envelopes and opaque payload bodies.

Domain contracts live one layer above. **forge-alloy** is the natural
home: an alloy defines the schema, required capabilities, permissions,
lifecycle states, valid replies, validation rules, replay semantics, and
compatibility rules for a payload. AIRC envelopes MAY include a
`forge.body_hint` header such as `forge.work.offer` when the body
conforms to a known alloy.

**The hint is just an opaque string from airc-rust's perspective.**
airc-rust does not depend on forge-alloy, does not import alloy schemas,
and does not validate bodies against them. It routes and filters
envelopes on the hint string (peers subscribe by exact match or prefix,
e.g. `forge.work.*`); consumers that recognize the hint look up their
own schemas. Envelopes without a hint are plain payloads; the substrate
routes them the same way.

Consumers such as Continuum personas, foundry nodes, render boxes, game
lobbies, and IRC-style clients speak alloyed contracts over airc-rust.
Peers that do not support the hinted alloy may ignore the event, log it,
surface the raw body, or apply their own consumer behavior. The
substrate only delivers; semantic interpretation lives at the
consumer/alloy layer.

This boundary is load-bearing for the project:
- airc-rust stays generic and shippable independent of any alloy work
- forge-alloy can evolve schemas without recompiling airc-rust
- New consumer ecosystems (game lobbies, IoT meshes, scientific compute
  rings) can use entirely different alloy vocabularies over the same
  substrate without coordination

## How consumers plug in

Any consumer — Continuum, OpenClaw, Hermes, a CLI client, a CI bot,
a future persona engine — links `airc-lib` and:

1. Registers identity (creates pubkey, picks initial nick).
2. Joins channels (`JOIN` control frame).
3. Sends messages: `airc.send(channel, body, ?to)` — body is whatever
   JSON/binary the consumer chooses.
4. Sends events: `airc.send_event(channel, body, headers, ?to)`.
5. Subscribes to events: `airc.subscribe(filter, handler)`.
6. Reads scrollback: `airc.fetch(channel, since)`.
7. Uploads blobs: `airc.blobs.put(bytes) -> MediaRef`.

Consumers define their own payload schemas (commands, recipes,
typed events, replay records, telemetry, whatever) inside the body.
airc has no opinion.

## Fast UDP (airc as network-substrate, not just chat)

A generic fast-UDP path is substrate-level work. WebRTC needs it
(real-time A/V across NATs), game servers need it (Call-of-Duty-style
match-state sync at 60 Hz), multiplayer simulations need it (player
position, projectile state), some IoT meshes need it. Every one of
those consumers re-implementing ICE candidate gathering, NAT traversal,
TURN fallback, STUN reachability, and UDP multiplexing is the wrong
factoring. airc owns the network primitive once; consumers ride it.

`airc-rtc` crate (rename pending — the WebRTC-flavored name oversells
it) ships:
- **UDP socket multiplexing** alongside airc control traffic on the
  same bound port. Connectionless. Low latency.
- **ICE candidate gathering** reusing airc's transport reachability
  data (we already know which peers are LAN-direct vs Tailscale vs
  gh-bridge; the candidate list derives from the same probe).
- **STUN/TURN integration** for cross-NAT reachability. Consumer can
  provide its own TURN server or use a default airc-supplied one for
  the mesh.
- **NAT traversal + hole-punching** as a peer pair handshake. Same
  primitive whether the packets are WebRTC SRTP, Call-of-Duty match
  state, or anything else.
- **Per-stream key derivation** from the airc identity ratchet (same
  forward-secrecy properties as airc messages — and consistent
  across all fast-UDP consumers).
- **webrtc-rs** as one integration option for consumers that want
  the full WebRTC stack. Game servers wanting raw UDP-with-sequence
  use the lower-level API directly.

Consumers hand `airc-rtc` either media tracks (audio/video frames)
or raw UDP messages (game-state diffs, multiplayer events). It
handles peer-direct UDP where possible, falls back to TURN-relay
when NATs require it. The signaling (SDP/ICE for WebRTC; lobby/match
state for games) flows as airc events alongside on the regular
TCP/gh/local channels.

This is what "network related and fine" means: the substrate owns
the network primitive even though the application owns what the
bytes represent.

## Roster semantics — three orthogonal peer sets per room

Surfaced as a real design gap by Codex on 2026-05-18: continuum's chat
header was rendering persistent `room.members` / seeded-capable users
as if they're active participants. That's misleading and it's a
substrate-level fix, not a UI fix.

The substrate exposes **three orthogonal peer sets** per room. They are
distinct API surfaces; consumers compose them as their use case
requires.

| Set | What it represents | Durability |
|---|---|---|
| `membership` | Subscription / capability — peers who CAN be in the room (seeded, invited, allowlisted) | Durable, persisted in `peers` × `channels` join table |
| `live_presence` | Peers who ARE in the room RIGHT NOW (connected transport, recent activity) | Transient, derived from `last_seen_at` + transport state |
| `responder_ready` | Peers who are primed to act — model admitted, inbox ready, agent online | Transient + capability-aware; updated by consumer signals |

```rust
impl Room {
    pub fn membership(&self) -> &MembershipSet;
    pub fn live_presence(&self) -> &PresenceSet;
    pub fn responder_ready(&self) -> &ResponderSet;
}
```

The three sets are computed from different sources:
- `membership` reads from `peers` + `channels` join in airc-store.
- `live_presence` derives from per-transport keep-alive state + the
  presence-event stream. A peer that hasn't sent a heartbeat in N
  seconds drops out of live_presence even if they remain in
  membership.
- `responder_ready` is composed: a peer announces readiness via an
  application-level event (consumer convention, e.g.
  `headers["forge.body_hint"]="presence.responder_ready"`). The substrate tracks
  which peers have most-recently announced ready vs busy/offline.

UI defaults (consumer choice, but the substrate makes the data easy):

| Use case | Default rendering |
|---|---|
| Live chat header ("who's here") | `live_presence ∩ responder_ready` |
| Room admin / capability view | `membership` |
| Inference dispatch ("who can respond") | `responder_ready` |
| Quorum / mention resolution | `live_presence` |

The substrate API exposes all three; consumers pick. **The substrate
never collapses them into a single "users" list** — that collapse is
exactly the bug Codex caught in continuum today, and it would re-emerge
in any consumer if the substrate didn't keep them distinct.

## Reliability + connection health

The substrate is operational infrastructure. It must keep connections
alive, detect failures fast, reestablish without manual intervention.

Built-in primitives:

- **Keep-alive heartbeats** at the transport layer, separate from
  application heartbeats. Configurable cadence per transport (more
  frequent for UDP/RTC paths, slower for gh-gist).
- **Dead-host detection**: if N consecutive heartbeats fail, the
  peer is marked degraded; if N+M, marked dead. Subscribers get an
  event; consumer can decide whether to fail-over to backup peers.
- **Auto-reconnect with backoff**: when a transport reports failure,
  airc-transport retries with exponential backoff. State (cursors,
  subscriptions, pending sends) survives the reconnect.
- **Self-healing host migration**: when the channel host evicts
  (today's `[HOST EVICTED]` flow), airc-rust formalizes the
  re-host election. Cursors and pending sends migrate to the new
  host transparently; consumers see a structured event, not a
  protocol fault.
- **Message durability across reconnect**: pending sends queue in
  `airc-store`; the daemon retries on reconnect. Loss is loud (event
  + audit-log entry) not silent.
- **Health probes**: `airc doctor --health` returns structured
  status: per-transport state, last-recv timestamps, peer
  reachability matrix, queue depths. The same data is queryable
  programmatically by consumers.

**Reticulum alignment**: Mark Qvist's [Reticulum Network Stack](https://reticulum.network)
solves a similar shape — distributed network substrate, identity-
addressable, transport-agnostic, designed for unreliable links.
airc-rust's transport layer is structured to allow `airc-transport-
reticulum` as a future plugin. When it lands, the airc-substrate API
doesn't change — Reticulum just becomes another transport the
resolver can pick, with its own reachability properties (e.g.
"reticulum: works over LoRa / amateur radio when other transports
have no path").

## Security

Beyond per-message encryption (handled by the double-ratchet section
above), the substrate enforces operational safety:

- **Identity attestation**: pairing produces a signed envelope
  binding pubkey → nick → role → first-seen ts → paired-with-pubkey.
  Re-pair under the same pubkey is a non-event; re-pair under a
  DIFFERENT pubkey for the same nick surfaces an "identity changed"
  warning event so consumers can re-verify.
- **Allowlist / blocklist** on the peer registry. Untrusted peers
  get sandboxed: their messages land in a quarantine channel
  consumers explicitly opt into.
- **Rate limiting** per peer per channel. Misbehaving peers (flood,
  burst, malformed envelopes) get throttled by the receiving daemon;
  events fan out so subscribers can decide policy.
- **Untrusted-payload handling**: airc never executes a payload.
  Body interpretation is consumer-level. The substrate guarantees
  delivery integrity (signature verified, AEAD intact, hop authentic)
  but makes no claim about payload safety. Consumers that process
  untrusted payloads (RPC handlers, code execution, etc.) own
  sandboxing.
- **No-PII guarantee**: airc-store doesn't index on body content.
  Consumers can opt into a per-message PII flag that triggers
  shorter retention + audit-trail-only access. Default storage is
  "structured envelope metadata + opaque body" — no body scanning,
  no body indexing, no body-derived analytics.
- **Hardware-backed identity** where available: macOS Secure Enclave
  (Touch ID-protected signing), Windows TPM, Linux TPM2 / fTPM. Key
  material never leaves the secure element. Pairing flow attests
  hardware-rooted vs software-only.
- **Audit log**: every key rotation, pairing, host migration,
  rate-limit trip, and redaction event lands in an append-only audit
  table separate from message storage. Operators can query the audit
  log without touching message content.

The threat model airc-rust defends against:
- Network attacker (passive or active) reading or tampering with
  on-wire traffic → defeated by AEAD + ratchet
- Compromised peer impersonating another peer → defeated by
  identity attestation; old keys can't replay as new identity
- Forward-secrecy compromise (long-term key theft) → defeated by
  double-ratchet per-message keys
- Malicious peer flooding a channel → mitigated by rate limiting
- Lost / stolen device → SQLCipher at-rest + Secure-Enclave-bound
  keys mean local DB + identity remain protected without the
  device unlocked
- Malicious payload from an authorized peer → NOT defended at
  substrate level; consumer responsibility

## Beyond chat: airc-rust as a generic substrate

The protocol primitives (peers, rooms, messages, events, signaling,
file transfer, fast UDP) are not chat-specific. Consumer patterns
beyond chat:

- **Game lobby + match coordination**: rooms = lobbies, peers =
  players, fast-UDP = match state, events = "player joined" / "match
  starting" / "kill confirmed" / etc. Call-of-Duty-shaped servers
  fit the same substrate that runs an IRC bot.
- **Distributed scientific compute**: rooms = experiment cohorts,
  events = task dispatched / result ready / worker failed, file
  transfer = result-dataset shipment.
- **IoT mesh**: rooms = device groups, events = sensor reading
  bursts, fast-UDP = real-time telemetry, Reticulum transport when
  links are unreliable.
- **Multiplayer simulations / virtual worlds**: rooms = world
  partitions, fast-UDP = entity state sync, events = world events,
  file transfer = world-asset distribution.
- **Persona / agent ecosystems**: rooms = team rooms, peers =
  agents/personas/humans, events = "is thinking" indicators,
  commands = JSON RPC inside messages.

The chat-shaped consumer (IRC client, automation bot, persona) is
the simplest case. Game/sim/IoT cases use more of the fast-UDP path
and less of the durable-scrollback path; both share the same
substrate primitives.

## Migration phases

1. **Phase 1 — Parallel.** Build the airc-rust crates. Today's
   Python+bash airc continues to run. Rust binary co-exists.
2. **Phase 2 — Bridge.** A bridge reads from Python-airc and mirrors
   into airc-store. Existing rooms keep working.
3. **Phase 3 — Library cutover.** Major consumers move from
   shell-out / IPC to `airc-lib`. The slow chat paths inside those
   consumers go away.
4. **Phase 4 — CLI parity.** The Rust `airc` binary replaces the
   bash `airc` script. Subcommands behavior-identical at launch.
5. **Phase 5 — Python/shell deletion.** `lib/airc_core` and Python
   tests are deleted. Remaining `cmd_*.sh` wrappers continue shrinking
   toward install/bootstrap only.
6. **Phase 6 — Security upgrade.** Double-ratchet + SQLCipher
   landed once stable. Pairing flows updated to produce attestation.

Each phase reversible until phase 5.

## Open questions

- Vector storage for consumer-side embedding indexes: sqlite-vss
  (single-binary story) vs. pgvector (Postgres backend only). Both
  belong in the consumer, not in airc-core, but airc-store could
  expose an optional capability flag.
- WebRTC: signaling shape (SDP/ICE JSON content) stays application-
  level — consumers wrap it in event bodies with `headers["forge.body_hint"]="rtc.sdp"`
  or similar. BUT the NETWORK side (UDP, ICE candidate gathering,
  NAT traversal, the actual efficient media path) IS airc-rust's
  job because it's network-substrate work. `airc-rtc` crate ships
  the UDP/ICE machinery via webrtc-rs; consumers hand it media
  streams and it handles peer-direct UDP-or-TURN-relay efficiently
  across the same boundary the rest of airc owns. See "WebRTC
  efficiency" section below.
- Backward-compat for existing peer pairings established under
  today's airc identity model: bridge during phase 2, retire at
  phase 5.
- Cursor model for archived ranges: do we expose archive query as
  a different API or just transparent-passthrough on `fetch`?

## Appendix A — Example consumer: a chat client

A bare CLI chat client (think `irssi`) just uses messages:

```rust
let airc = AircClient::open()?;
airc.join("#general")?;
loop {
    select! {
        Some(msg) = airc.next_message() => {
            println!("[{}] <{}> {}", msg.channel, msg.from, msg.body.as_text());
        }
        Some(input) = stdin.next_line() => {
            airc.send("#general", Body::Json(json!({"text": input})), None)?;
        }
    }
}
```

It doesn't subscribe to events because it doesn't need push wake-up —
the message loop is already blocking on next-frame.

## Appendix B — Example consumer: a Monitor (sentinel-shaped)

A Monitor process that wakes on every event in a channel:

```rust
let airc = AircClient::open()?;
airc.subscribe(Subscription {
    channel: Some(ch),
    from_peer: None,
    headers: BTreeMap::new(),  // empty = match any envelope
}, |envelope| {
    // wake immediately when an event arrives
    handle(envelope);
})?;
airc.run();  // pumps the transport
```

This is the shape Claude Code's Monitor uses today. It's interrupt-
driven by construction.

## Appendix C — Example consumer: an automation engine

An engine that issues RPC-style commands to peers, encoded as JSON
in the body, with a correlation_id convention:

```rust
let correlation_id = uuid::Uuid::new_v4();
let mut headers = Headers::new();
headers.insert("forge.body_hint".into(), "rpc.command".into());
headers.insert("airc.reply_to".into(), "".into()); // initial request; reply will set this
let envelope = airc.send_event(
    "#cambriantech",
    Body::Json(json!({
        "op": "git/pr-create",
        "args": { "branch": "feat/foo", "title": "..." },
        "correlation_id": correlation_id,
    })),
    headers,
    Some(target_peer),         // direct, not broadcast
)?;
// wait for a response event with matching correlation_id
let resp = airc.await_event_matching(|m|
    m.body.json_field("correlation_id") == correlation_id
).await?;
```

airc doesn't know what `git/pr-create` means. The consumer that
receives the event recognizes the op string and handles it. Another
consumer that doesn't recognize it ignores it.

That's it — commands are just JSON payloads with a convention. The
"command bus" is application-level, not protocol-level.

---

_Draft. Reply with corrections, omissions, or "yes ship it" on the
PR thread._
