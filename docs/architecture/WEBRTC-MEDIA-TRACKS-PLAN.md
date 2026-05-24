# WebRTC Media Tracks — API Sketch and Scope

Status: **planning doc, no code yet.** Awaiting Joel's review before
implementation begins. This is the proposed shape for the
`airc-lib` media-track SDK that #957's WebRTC orchestration enables
but doesn't yet expose.

## Goal

Continuum/Hermes/OpenClaw avatars need to **join humans in live
video conferencing** through the AIRC substrate. The DataChannel
piece (#957) gives those avatars a typed-event control channel. This
PR extends the same per-peer WebRTC connection with **audio and
video media tracks** so the avatar can also send/receive AV.

Substrate stays generic: it owns the WebRTC connection lifecycle,
codec negotiation, track attachment, and the inbound-track surface.
Consumers (Continuum) own:

- Producing audio samples from TTS / synthesis
- Producing video frames from avatar render
- Consuming inbound audio for STT / persona awareness
- Consuming inbound video for face/scene understanding

## Scope cuts (this PR)

**IN:**
- Pre-connect track attachment (tracks added before the offer/answer
  exchange — frozen for the lifetime of the connection)
- Opinion-ated defaults for the common case: Opus audio, VP8 video
- Outbound: a sample-writer handle returned to the caller
- Inbound: a callback or per-peer stream of incoming `TrackRemote`
- Integration test exercising audio sample round-trip between two
  Airc instances on a shared signaling wire

**OUT (deferred to follow-ups):**
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

## Open questions for Joel

1. **Codec escape hatch.** Should `OutgoingAudioTrack` /
   `OutgoingVideoTrack` accept a `with_codec(mime_type)` override
   for callers that need H264 / AV1 / G.711? Or is opinion-ated-only
   acceptable for v1?

2. **Track count limits.** A single PC can carry multiple audio/video
   tracks (e.g. avatar voice + ambient audio). The proposed builder
   accepts a `Vec`. Is unbounded fine, or should there be a sanity
   cap?

3. **Inbound surface shape.** Global callback (proposed) vs per-peer
   receiver vs both. Recommendation above is global callback first.

4. **Failure mode when peer doesn't accept the track kinds.** SDP
   negotiation can refuse a codec/track. Surface as an `AircError`
   from `.open()` or panic? Recommend explicit `AircError::Transport`
   with the rejected codec name.

5. **Renegotiation timing.** Confirmed deferred to follow-up? The
   non-renegotiation constraint means avatar's tracks have to be
   declared at session start — they can't go from "voice-only" to
   "voice + video" mid-call without dropping and reconnecting.

## Implementation sketch (rough)

Once the API is approved, the implementation:

1. New module `airc-lib/src/webrtc_media.rs` with `WebRtcConnectionBuilder`,
   `OutgoingAudioTrack`, `OutgoingVideoTrack`, `OpenedWebRtcConnection`.
2. Refactor `Airc::open_webrtc_to(peer_id)` internals to delegate to
   `webrtc_connection(peer_id).open()` so the no-tracks path stays
   identical.
3. Update `OffererHandler` and `AnswererHandler` (currently in
   `webrtc.rs`) to forward `on_track` events to the global inbound
   handler.
4. Add `inner.webrtc_incoming_track_handler: Mutex<Option<...>>` to
   `AircInner`.
5. Integration test: two Airc instances, audio track only, Alice
   writes a sample, Bob's handler fires with a matching `TrackRemote`.

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
