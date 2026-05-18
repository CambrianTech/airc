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
pub struct Message {
    pub id: Uuid,
    pub from: PeerId,
    pub to: Recipient,             // single peer or "all" within channel
    pub channel: ChannelId,
    pub ts: Timestamp,
    pub body: Body,                // opaque to airc
    pub media_refs: Vec<MediaRef>, // pointers, not bytes
    pub signature: Signature,      // sender's signature over envelope+body
}

pub enum Body {
    Json(serde_json::Value),       // most consumers
    Binary(Bytes),                 // when JSON overhead is wasted
}
```

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
    pub channel: Option<ChannelId>,   // None = any
    pub from_peer: Option<PeerId>,    // None = any
    pub body_hint: Option<String>,    // optional payload-kind tag for filtering
}
```

Monitor processes (like Claude Code's airc Monitor) subscribe to
events. Sentinels subscribe to events. WebRTC signaling lands as
events so the receiving consumer wakes immediately on SDP/ICE.

The `body_hint` is a peer convention (consumers agree on the string,
e.g. "signaling.webrtc.sdp" or "kanban.card.claimed") — airc doesn't
interpret it, just lets subscribers filter on it.

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
| `messages` | id, channel_id, ts, from_pubkey, to (string), body_kind (json/binary), body_blob, body_hint, is_event (bool), signature | The wire log |
| `media_refs` | message_id, sha256, mime, size, blob_path | Pointers; blobs live in `airc-blobs` |
| `channels` | id, name, host_pubkey, created_at, archive_partition_id | Rooms |
| `peers` | pubkey, nick, role_hint, last_seen_at, attest_sig | Identity |
| `cursors` | peer_pubkey, channel_id, last_read_sig, last_read_ts | Per-peer read state |
| `subscriptions` | peer_pubkey, channel_id_or_null, from_peer_or_null, body_hint_or_null | Event fan-out registry |
| `archive_partitions` | id, drained_at, range_start_ts, range_end_ts | Bounded active DB; older messages moved to archive DB |
| `key_attestations` | pubkey, paired_at, attest_sig, paired_with_pubkey | Identity audit trail |

`messages.body_blob` is opaque to airc. Consumers serialize/deserialize
their own payload shapes into/out of it. The optional `body_hint` is a
string convention for routing/filtering — airc never validates it.

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

## Transport resolver

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
gh-gist stays as a legacy bridge for cross-network peers who haven't
paired via Tailscale yet, but it's not the only transport.

Each transport is a `Transport` trait impl exposing `send(envelope)`
and `receive() -> Stream<Envelope>`. The resolver multiplexes.

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
  `pii=true` (via the `body_hint`) which triggers extra retention/
  redaction policy. Audit log marks redaction events.

## How consumers plug in

Any consumer — Continuum, OpenClaw, Hermes, a CLI client, a CI bot,
a future persona engine — links `airc-lib` and:

1. Registers identity (creates pubkey, picks initial nick).
2. Joins channels (`JOIN` control frame).
3. Sends messages: `airc.send(channel, body, ?to)` — body is whatever
   JSON/binary the consumer chooses.
4. Sends events: `airc.send_event(channel, body, ?body_hint, ?to)`.
5. Subscribes to events: `airc.subscribe(filter, handler)`.
6. Reads scrollback: `airc.fetch(channel, since)`.
7. Uploads blobs: `airc.blobs.put(bytes) -> MediaRef`.

Consumers define their own payload schemas (commands, recipes,
typed events, replay records, telemetry, whatever) inside the body.
airc has no opinion.

## WebRTC efficiency (airc as network-substrate)

WebRTC needs efficient UDP across the same boundary airc already owns
(peer addressing, NAT traversal, encryption keys). Punting that to
consumers means every consumer that wants real-time A/V re-implements
ICE candidate gathering, TURN fallback, STUN reachability. That's
substrate-level work; airc handles it.

`airc-rtc` crate ships:
- ICE candidate gathering reusing airc's transport reachability data
  (we already know which peers are LAN-direct vs Tailscale vs
  gh-bridge; ICE candidates derive from the same probe)
- UDP socket multiplexing alongside airc control traffic
- STUN/TURN integration (consumer can provide their own TURN server
  or use a default airc-supplied one for the mesh)
- webrtc-rs as the underlying media-engine integration
- Per-stream key derivation from the airc identity ratchet (same
  forward-secrecy properties as airc messages)

Consumers hand `airc-rtc` media tracks (audio/video frames or pre-
encoded media); it handles peer-direct UDP where possible, falls back
to TURN-relay when NATs require it. The signaling (SDP/ICE) flows as
airc events alongside; airc-rtc and the consumer's signaling handler
share the same envelope substrate.

This is what "network related and fine" means for WebRTC: the
substrate owns the network primitive even though the application
owns what the media bytes represent.

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
5. **Phase 5 — Python/shell deletion.** All `airc_core/*.py` and
   `cmd_*.sh` deleted. Only `install.sh` remains in shell.
6. **Phase 6 — Security upgrade.** Double-ratchet + SQLCipher
   landed once stable. Pairing flows updated to produce attestation.

Each phase reversible until phase 5.

## Open questions

- Vector storage for consumer-side embedding indexes: sqlite-vss
  (single-binary story) vs. pgvector (Postgres backend only). Both
  belong in the consumer, not in airc-core, but airc-store could
  expose an optional capability flag.
- WebRTC: signaling shape (SDP/ICE JSON content) stays application-
  level — consumers wrap it in event bodies with `body_hint="rtc.sdp"`
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
    body_hint: None,
    from_peer: None,
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
let envelope = airc.send_event(
    "#cambriantech",
    Body::Json(json!({
        "op": "git/pr-create",
        "args": { "branch": "feat/foo", "title": "..." },
        "correlation_id": uuid::Uuid::new_v4(),
    })),
    Some("rpc.command"),       // body_hint for filtering
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
