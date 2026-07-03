//! Live stream-chunk primitive over the AIRC bus — the substrate-level
//! carrier for any low-latency, produced-incrementally stream.
//!
//! A `Message` frame is the *durable, completed* utterance (`Airc::say`).
//! A stream chunk is the opposite: one fragment delivered the instant it
//! is produced, before the whole is known. The substrate already has the
//! right delivery class for this — [`FrameKind::Event`]: "push-driven,
//! interrupt-style, receivers consume immediately; the substrate may or
//! may not persist. Use for presence transitions, **typing indicators**,
//! work-allocation pings." A live generation stream IS a typing
//! indicator with content; it rides `Event`, and the final settled text
//! still rides a durable `Message`.
//!
//! Why this lives in the substrate and not in one consumer: the first
//! user is LLM token streaming (Continuum personas streaming a turn into
//! a room as it decodes), but the shape is modality-agnostic. The same
//! three facts — *which stream, what order, what kind* — carry audio
//! samples from a native-speaking model, avatar animation frames, or a
//! robot's actuator deltas. One primitive, every modality: a consumer
//! switches on [`StreamChunk::kind`] and decodes the payload. Build the
//! bus right once and speech / avatar / robot streaming is a
//! configuration, not a rewrite.
//!
//! Pairs a typed publish ([`Airc::publish_stream_chunk`]) with a typed,
//! header-filtered subscribe ([`Airc::subscribe_stream_chunks`]) — the
//! same shape as [`crate::diagnostic_event_sink`]. The substrate routes
//! and filters on the `airc.stream.*` headers without decoding the body;
//! text payloads ride [`Body::text`], binary payloads ride
//! [`Body::Binary`].

use std::sync::Arc;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, TranscriptEvent};
use airc_protocol::FrameKind;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::Airc;

/// Header keys for filtering stream chunks on the wire without decoding
/// the body. Stable string values so any subscriber (UI, TTS, another
/// peer) can match against them directly.
pub const HEADER_STREAM_ID: &str = "airc.stream.id";
pub const HEADER_STREAM_SEQ: &str = "airc.stream.seq";
pub const HEADER_STREAM_KIND: &str = "airc.stream.kind";
pub const HEADER_STREAM_FINAL: &str = "airc.stream.final";

/// Well-known chunk kinds. The field is an open string (consumers own
/// meaning) but these are the canonical values so producers and
/// subscribers agree without a shared enum. Text kinds are live today;
/// the rest are the rails the modality-agnostic design leaves for
/// audio / avatar / robot streams.
pub const STREAM_KIND_TEXT_TOKEN: &str = "text.token";
pub const STREAM_KIND_TEXT_REASONING: &str = "text.reasoning";
/// Reserved (not yet produced): PCM audio frames from a native-speaking
/// model, encoded video/animation frames, robot actuator deltas.
pub const STREAM_KIND_AUDIO_PCM: &str = "audio.pcm";
pub const STREAM_KIND_VIDEO_FRAME: &str = "video.frame";
pub const STREAM_KIND_MOTOR_CMD: &str = "motor.cmd";

/// The payload of one chunk. Text rides JSON-recoverable [`Body::text`];
/// binary modalities (audio/video/motor) ride [`Body::Binary`] to avoid
/// JSON overhead at high tick rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPayload {
    Text(String),
    Binary(Vec<u8>),
}

/// One fragment of a live stream: which stream it belongs to, its
/// position, what kind of fragment it is, whether it closes the stream,
/// and the payload itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Groups every chunk of one logical stream (one generation, one
    /// utterance). Subscribers demux on this.
    pub stream_id: String,
    /// Monotonic per-stream sequence, starting at 0. Lets a subscriber
    /// detect drops/reorders without trusting transport ordering.
    pub seq: u64,
    /// What this fragment is — one of the `STREAM_KIND_*` constants, or
    /// a consumer-defined value.
    pub kind: String,
    /// True on the terminal chunk so a subscriber knows the stream
    /// closed without waiting on a timeout. The final chunk may also
    /// carry payload (the last token) or be payload-empty (a pure
    /// end-of-stream marker).
    pub is_final: bool,
    /// The fragment bytes.
    pub payload: StreamPayload,
}

impl StreamChunk {
    /// Convenience constructor for a non-final text token chunk.
    pub fn text_token(stream_id: impl Into<String>, seq: u64, text: impl Into<String>) -> Self {
        Self {
            stream_id: stream_id.into(),
            seq,
            kind: STREAM_KIND_TEXT_TOKEN.to_string(),
            is_final: false,
            payload: StreamPayload::Text(text.into()),
        }
    }

    /// Convenience constructor for the terminal end-of-stream marker
    /// (no payload bytes).
    pub fn text_end(stream_id: impl Into<String>, seq: u64) -> Self {
        Self {
            stream_id: stream_id.into(),
            seq,
            kind: STREAM_KIND_TEXT_TOKEN.to_string(),
            is_final: true,
            payload: StreamPayload::Text(String::new()),
        }
    }
}

impl Airc {
    /// Publish a single [`StreamChunk`] as an AIRC [`FrameKind::Event`]
    /// frame into the current room, tagged with stable `airc.stream.*`
    /// headers so subscribers can demux and order without decoding the
    /// body. Returns the substrate event id.
    ///
    /// This is the live, ephemeral path: chunks are typing-indicator
    /// class traffic, NOT durable transcript content. The completed,
    /// settled utterance is published separately as a `Message` (via
    /// [`Airc::say`]). Producers stream chunks as bytes arrive, then
    /// `say` the final text once.
    pub async fn publish_stream_chunk(
        &self,
        chunk: &StreamChunk,
    ) -> Result<airc_core::EventId, AircError> {
        let mut headers = Headers::new();
        headers.insert(HEADER_STREAM_ID.into(), chunk.stream_id.clone());
        headers.insert(HEADER_STREAM_SEQ.into(), chunk.seq.to_string());
        headers.insert(HEADER_STREAM_KIND.into(), chunk.kind.clone());
        if chunk.is_final {
            headers.insert(HEADER_STREAM_FINAL.into(), "true".to_string());
        }
        let body = match &chunk.payload {
            StreamPayload::Text(text) => Body::text(text.clone()),
            StreamPayload::Binary(bytes) => Body::Binary(bytes.clone()),
        };
        self.send_frame_to(FrameKind::Event, MentionTarget::All, body, headers)
            .await
    }

    /// Live stream of typed [`StreamChunk`]s observed on the substrate.
    /// Filters the raw subscription to events carrying an
    /// `airc.stream.id` header and decodes them. Yields the raw
    /// [`TranscriptEvent`] (for sender/peer identity) alongside the
    /// decoded chunk.
    ///
    /// Subscribers that care about one stream filter the result on
    /// [`StreamChunk::stream_id`]; subscribers acting as a sink for a
    /// modality filter on [`StreamChunk::kind`].
    pub async fn subscribe_stream_chunks(
        &self,
    ) -> Result<impl Stream<Item = (Arc<TranscriptEvent>, StreamChunk)>, AircError> {
        let inner = self.subscribe().await?;
        Ok(inner.filter_map(|item| async move {
            let event = item.ok()?;
            let chunk = parse_stream_chunk(&event)?;
            Some((event, chunk))
        }))
    }
}

/// Decode a [`StreamChunk`] from a transcript event, or `None` if the
/// event is not a stream chunk (no `airc.stream.id` header) or is
/// malformed (a missing/unparseable required header fails the decode —
/// the chunk is skipped, never silently coerced to a default).
fn parse_stream_chunk(event: &TranscriptEvent) -> Option<StreamChunk> {
    let stream_id = event.headers.get(HEADER_STREAM_ID)?.to_string();
    let seq = event.headers.get(HEADER_STREAM_SEQ)?.parse::<u64>().ok()?;
    let kind = event.headers.get(HEADER_STREAM_KIND)?.to_string();
    let is_final = event
        .headers
        .get(HEADER_STREAM_FINAL)
        .map(|v| v == "true")
        .unwrap_or(false);
    let payload = match event.body.as_ref()? {
        Body::Json(_) => StreamPayload::Text(event.body.as_ref()?.as_text()?.to_string()),
        Body::Binary(bytes) => StreamPayload::Binary(bytes.clone()),
    };
    Some(StreamChunk {
        stream_id,
        seq,
        kind,
        is_final,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the header/body encoding of a text chunk must
    // round-trip back to the identical StreamChunk through a real
    // TranscriptEvent — the wire contract between producer and any
    // subscriber. Regresses silent header-key/parse drift.
    fn make_event(headers: Headers, body: Option<Body>) -> TranscriptEvent {
        TranscriptEvent {
            event_id: airc_core::EventId::new(),
            room_id: airc_core::RoomId::from_u128(0xc0ffee),
            peer_id: airc_core::PeerId::from_u128(0xa1),
            client_id: airc_core::ClientId::from_u128(0xc1),
            kind: airc_core::transcript::TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers,
            body,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    fn encode(chunk: &StreamChunk) -> (Headers, Option<Body>) {
        let mut headers = Headers::new();
        headers.insert(HEADER_STREAM_ID.into(), chunk.stream_id.clone());
        headers.insert(HEADER_STREAM_SEQ.into(), chunk.seq.to_string());
        headers.insert(HEADER_STREAM_KIND.into(), chunk.kind.clone());
        if chunk.is_final {
            headers.insert(HEADER_STREAM_FINAL.into(), "true".to_string());
        }
        let body = match &chunk.payload {
            StreamPayload::Text(t) => Body::text(t.clone()),
            StreamPayload::Binary(b) => Body::Binary(b.clone()),
        };
        (headers, Some(body))
    }

    #[test]
    fn text_token_round_trips_through_event() {
        let chunk = StreamChunk::text_token("gen-abc", 7, "hello");
        let (headers, body) = encode(&chunk);
        let event = make_event(headers, body);
        let decoded = parse_stream_chunk(&event).expect("decodes");
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn final_marker_round_trips() {
        let chunk = StreamChunk::text_end("gen-abc", 99);
        let (headers, body) = encode(&chunk);
        let event = make_event(headers, body);
        let decoded = parse_stream_chunk(&event).expect("decodes");
        assert!(decoded.is_final);
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn binary_payload_round_trips() {
        let chunk = StreamChunk {
            stream_id: "audio-1".into(),
            seq: 3,
            kind: STREAM_KIND_AUDIO_PCM.into(),
            is_final: false,
            payload: StreamPayload::Binary(vec![0, 1, 2, 250, 255]),
        };
        let (headers, body) = encode(&chunk);
        let event = make_event(headers, body);
        let decoded = parse_stream_chunk(&event).expect("decodes");
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn non_stream_event_is_ignored() {
        // no airc.stream.id header → not a stream chunk
        let event = make_event(Headers::new(), Some(Body::text("just chat")));
        assert!(parse_stream_chunk(&event).is_none());
    }
}
