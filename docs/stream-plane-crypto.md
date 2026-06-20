# Stream-plane crypto: sign the handle, not the frame

**Status:** design / spec (not yet built). Motivated by a profiled latency finding
(see `crates/airc-lib/tests/lan_latency_bench.rs` + `airc-diagnostics::timing`).

## The problem

airc wraps every transport in `SignedTransport`, which applies a **per-frame
Ed25519 signature** (`sign_envelope`). Measured cost: **~113µs per frame** (sign),
plus a matching inbound `verify` on the receiver.

For the **control plane** (commands, presence, lifecycle — discrete, low-rate)
this is correct and cheap *enough*: it provides **end-to-end authenticity that
survives relay hops**. A relay forwards a frame but cannot forge a peer's
signature; point-to-point TLS alone cannot give that (the relay terminates TLS).
So the app-level signature is doing real work on relayed control traffic — keep it.

For the **data/stream plane** (WebRTC media, UDP packet streams, high-rate
DataChannels) per-frame Ed25519 is a throughput killer. At media rates (tens of
frames/sec/track × many tracks × many peers) 113µs/frame of asymmetric crypto
dominates and does not scale.

## The model: two planes, keyed off `FrameKind` / `RouteClass`

| Plane | Traffic | Per-frame auth | Why |
|---|---|---|---|
| **Control** | commands, presence, lifecycle (`Message`/`Control` at command rate) | per-frame **Ed25519** | end-to-end authenticity across relays; rate is low |
| **Stream** | WebRTC media, UDP streams, high-rate DataChannels | **symmetric AEAD** under a once-signed session key | per-frame asymmetric crypto doesn't scale to packet rates |

The discriminator already exists: `FrameKind` → `RouteClass` (`route_class_for_frame`).
The signing policy in `SignedTransport` (or a policy layer above it) should branch
on it: **per-frame sign for control classes, session-key for stream classes.**

## Stream plane: sign the handle once, reuse the symmetric key

This is the standard pattern (DTLS-SRTP, QUIC, WireGuard, Noise):

1. **At stream/channel establishment**, run ONE Ed25519-authenticated handshake
   that establishes a shared **symmetric** key (X25519 ECDH → HKDF, authenticated
   by each peer's Ed25519 identity key — the same identities airc already pins).
2. **Per packet**, seal/authenticate with the symmetric key via **AEAD
   (ChaCha20-Poly1305)** — ~tens of ns/packet vs 113µs.

The **reuse** is the session key: pay the asymmetric cost ONCE at the handle, reuse
the symmetric key for every frame. The symmetric tag *is* the proof the packet came
from the authenticated session — identity is not re-proven per packet.

## airc-specific wiring

- **WebRTC media tracks** are already DTLS-SRTP session-keyed by the `webrtc`
  crate. Do NOT layer per-frame app-Ed25519 on media. Instead: at SDP-offer time,
  do ONE Ed25519 binding of the **DTLS fingerprint → airc peer identity** (sign the
  handle), then let SRTP do per-packet auth. This binds the media session to the
  pinned peer identity without per-frame asymmetric cost.
- **DataChannel + UDP envelopes** today go through `SignedTransport` (per-frame).
  Split by class: keep per-frame Ed25519 for the **control** DataChannel; for
  **stream** channels, Ed25519 the channel-open to derive a symmetric key, then
  AEAD per frame.
- **UDP** (`crates/airc-transport/src/udp`) is explicitly "wrapped with
  `SignedTransport`" today — for any stream-rate UDP use, route it through the
  session-key path instead.

## Secondary levers (control plane, only if ever needed)

- **Ed25519 batch verification** — verifying N signatures together is ~2× faster
  than N individual verifies; useful for inbound bursts.
- **Merkle-root signing** — one signature amortized over a burst of frames.

Both are secondary to the two-plane split; the split is the architectural fix.

## Build sequence

1. Introduce a per-stream **session-key** primitive (X25519 + HKDF +
   ChaCha20-Poly1305), authenticated by the existing Ed25519 peer identities.
2. Make the signing policy branch on `RouteClass` (control → per-frame Ed25519;
   stream → session AEAD). Default unchanged (control) so nothing regresses.
3. Bind WebRTC DTLS fingerprint → peer identity at SDP-offer (one signature).
4. Route stream-rate DataChannel/UDP through the session-key path.
5. Extend `lan_latency_bench` with a stream-rate (per-packet) measurement to prove
   the per-frame cost drops from ~113µs to ~tens of ns.

## Non-goals

- Do **not** drop the per-frame Ed25519 on the control plane — it is the relay
  end-to-end authenticity guarantee.
- Do not micro-optimize per-frame Ed25519 as the primary fix; the architectural
  split is the fix.
