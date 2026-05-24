# WebRTC Media Tracks — API Sketch and Scope

Status: **baseline implemented.** #960 added inbound media-track
delivery and the per-peer registry. #961 added the outbound
connection builder, closed-set Opus/VP8 codec surface, and writable
sample-track handles. The remaining work is hardening the media path
for consumer integration, not proving the basic API exists.

## Goal

Continuum/Hermes/OpenClaw avatars need to **join humans in live
video conferencing** through the AIRC substrate. The DataChannel
piece (#957) gives those avatars a typed-event control channel. This
PR extends the same per-peer WebRTC connection with **audio and
video media tracks** so the avatar can also send/receive AV.

<img src="https://raw.githubusercontent.com/CambrianTech/continuum/main/docs/images/live-session-avatars.png" alt="Continuum live room with one human and AI personas represented as avatars in a shared video conversation" width="100%"/>

The image above is a consumer example, not a substrate dependency.
AIRC provides the signed channel, peer identity, subscriptions,
WebRTC signaling, DataChannel control plane, and media-track
connection lifecycle. Continuum renders the room, owns persona state,
and decides how avatar audio/video maps to cognition.

Substrate stays generic: it owns the WebRTC connection lifecycle,
codec negotiation, track attachment, and the inbound-track surface.
Consumers (Continuum) own:

- Producing audio samples from TTS / synthesis
- Producing video frames from avatar render
- Consuming inbound audio for STT / persona awareness
- Consuming inbound video for face/scene understanding

## Scope cuts (this PR)

**Baseline now in-tree:**
- Pre-connect track attachment (tracks added before the offer/answer
  exchange — frozen for the lifetime of the connection)
- Opinion-ated defaults for the common case: Opus audio, VP8 video
- Outbound: a sample-writer handle returned to the caller
- Inbound: a callback or per-peer stream of incoming `TrackRemote`
- Integration test exercising audio/video negotiation between two
  Airc instances on a shared signaling wire

**Still deferred to follow-ups:**
- **Renegotiation** — adding/removing tracks after the connection is
  established. This needs the signaling state machine to handle
  re-offer events on an existing PC, which is a substantial expansion
  of #957's state machine.
- **ICE restart** — recovering connectivity after network change.
- **Simulcast / scalable video coding** — multiple encodings per
  track. The opinion-ated API picks single-encoding defaults.
- **Codec negotiation** beyond the opinion-ated defaults. Callers
  who need H.264 or AV1 get an escape hatch (see Open Questions).
- **Lip-sync / audio-video synchronization beyond what webrtc-rs
  provides out of the box.**
- **Track muting / enabled-toggle at runtime.**

## Proposed SDK API

```rust
// in airc-lib/src/webrtc_media.rs

pub struct WebRtcConnectionBuilder {
    target: PeerId,
    audio_tracks: Vec<OutgoingAudioTrack>,
    video_tracks: Vec<OutgoingVideoTrack>,
}

pub struct OutgoingAudioTrack {
    pub label: String,        // human-readable, e.g. "avatar-voice"
    pub stream_id: String,    // groups related tracks (a/v share a stream_id)
    // Codec is fixed to Opus in this PR's opinion-ated path; an
    // `OutgoingAudioTrack::with_codec(MIME_TYPE_*)` escape hatch is
    // an open question (see below).
}

pub struct OutgoingVideoTrack {
    pub label: String,
    pub stream_id: String,
    // Codec fixed to VP8 in opinion-ated path.
}

impl Airc {
    /// Builder entrypoint replacing the current `open_webrtc_to`
    /// for callers that want media tracks. Existing
    /// `open_webrtc_to(peer_id)` stays as the data-channel-only path.
    pub fn webrtc_connection(&self, target: PeerId) -> WebRtcConnectionBuilder { ... }
}

impl WebRtcConnectionBuilder {
    pub fn with_audio_track(self, track: OutgoingAudioTrack) -> Self { ... }
    pub fn with_video_track(self, track: OutgoingVideoTrack) -> Self { ... }
    /// Drive the handshake. Returns handles to write samples to
    /// the outgoing tracks. Inbound tracks are surfaced via the
    /// `Airc::set_incoming_track_handler` callback.
    pub async fn open(self) -> Result<OpenedWebRtcConnection, AircError> { ... }
}

pub struct OpenedWebRtcConnection {
    pub outgoing_audio: Vec<Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>>,
    pub outgoing_video: Vec<Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>>,
}

impl Airc {
    /// Register a global handler invoked when a remote track is
    /// negotiated on any peer connection. Handler receives the
    /// peer_id this track came from and the TrackRemote.
    pub async fn set_incoming_track_handler<F>(&self, handler: F) -> Result<(), AircError>
    where
        F: Fn(PeerId, Arc<dyn webrtc::media_stream::track_remote::TrackRemote>) + Send + Sync + 'static,
    { ... }
}
```

### Sample-writing (caller-facing)

The returned `Arc<TrackLocalStaticSample>` is webrtc-rs's own type.
Caller writes samples directly:

```rust
use webrtc::media_stream::media::Sample;
opened.outgoing_audio[0].write_sample(&Sample {
    data: opus_encoded_bytes,
    duration: Duration::from_millis(20),
    ..Default::default()
}).await?;
```

We don't wrap or hide `TrackLocalStaticSample` — exposing webrtc-rs's
type directly is the cleanest substrate boundary. Continuum already
needs to know about samples to encode them.

### Inbound surface

Two shapes considered:

1. **Global callback** (proposed above). Single handler registered
   on the Airc handle, fires for every inbound track on every peer
   connection. Simple, no per-peer plumbing. Downside: handler can't
   distinguish "expected" from "unexpected" tracks beyond the
   `(peer_id, kind)` it gets.

2. **Per-peer mpsc receiver.** `OpenedWebRtcConnection.incoming_tracks:
   mpsc::Receiver<Arc<dyn TrackRemote>>`. More structured, callers
   own the receive loop. Downside: doesn't capture inbound tracks for
   peers that connected via `accept_webrtc_offers` (where there's no
   `OpenedWebRtcConnection` returned to the caller).

Recommendation: **start with #1 (global callback)** because it covers
both initiator and responder paths uniformly. #2 can be added later
as ergonomic sugar for the initiator path.

## Opinion-ated codec defaults

| Track | MIME | clock_rate | channels |
|-------|------|------------|----------|
| Audio | `audio/opus` | 48000 | 2 |
| Video | `video/VP8`  | 90000 | — |

These come from `webrtc::peer_connection::configuration::media_engine::{MIME_TYPE_OPUS, MIME_TYPE_VP8}`.

Rationale:
- **Opus + VP8** are the WebRTC-mandatory baseline; every browser
  and every conferencing system supports them.
- **VP8 over H264** for the default: H264 has licensing
  ambiguities; VP8 is royalty-free.
- Continuum-side avatar rendering can always re-encode if a specific
  hardware encoder is preferred.

## Remaining Design Decisions

1. **Codec expansion.** `WebRtcMediaCodec` is intentionally a
   closed enum, not a string MIME escape hatch. Add H264 / AV1 /
   G.711 as explicit variants only when a consumer proof needs them.

2. **Track count limits.** A single PC can carry multiple audio/video
   tracks (e.g. avatar voice + ambient audio). The proposed builder
   accepts a `Vec`. Is unbounded fine, or should there be a sanity
   cap?

3. **Inbound surface shape.** Global callback and per-peer inspection
   are in-tree. Per-peer receiver streams remain optional ergonomic
   sugar if Continuum wants dedicated tasks per peer.

4. **Failure mode when peer doesn't accept the track kinds.** SDP
   negotiation can refuse a codec/track. Surface as an `AircError`
   from `.open()` or panic? Recommend explicit `AircError::Transport`
   with the rejected codec name.

5. **Renegotiation timing.** Confirmed deferred to follow-up? The
   non-renegotiation constraint means avatar's tracks have to be
   declared at session start — they can't go from "voice-only" to
   "voice + video" mid-call without dropping and reconnecting.

## Implementation Record

1. #960 added `airc-lib/src/webrtc_media.rs` inbound track types,
   handler registration, per-peer runtime registry, and `on_track`
   forwarding from offerer/answerer handlers.
2. #961 added `WebRtcConnectionBuilder`, `OutgoingAudioTrack`,
   `OutgoingVideoTrack`, `OpenedWebRtcConnection`, and
   `OutgoingSampleTrack`.
3. `Airc::open_webrtc_to(peer_id)` now delegates through the builder
   so the no-media path and media path share the same connection
   lifecycle.
4. The integration proof negotiates tracks over the existing AIRC
   signaling wire, returns writable local sample handles, and verifies
   the responder sees inbound `TrackRemote` handles.

## Next Consumer Proof

The next useful proof is not another substrate type. It is a
consumer-shaped fixture:

1. A Continuum-like avatar peer opens a WebRTC connection with Opus
   audio and VP8 video tracks.
2. A second peer accepts the connection, receives both tracks, and
   records per-track metadata without parsing AIRC internals.
3. A control event travels over the DataChannel during the same
   session, proving media and command/control share the same peer
   lifecycle without conflating media frames with transcript bodies.
4. The test asserts no GitHub, no shell, and no consumer-specific
   code in `airc-lib`.

## Why this scope is honest

The DataChannel orchestration (#957) was tractable in one PR because
the API surface was small: `create_data_channel("airc", None)`,
register the adapter, route execution dispatches to it.

Media tracks are a fundamentally bigger SDK surface — codecs,
encoders, sample shapes, inbound delivery, multiple tracks per PC.
Trying to ship "the full thing" in one PR risks landing a half-baked
surface that Continuum then has to wrestle with for months. Pinning
the API surface in this doc, getting Joel's read on the open
questions, and then implementing against the approved shape is the
right discipline here.
