//! The wire envelope — what a transport adapter carries across the mesh.
//!
//! An `Envelope` is the canonical "one event" record on the wire. Three
//! frame kinds wrap it: `Message` (pull-driven, durable), `Event`
//! (push-driven interrupt), `Control` (session lifecycle — JOIN / NICK /
//! HEARTBEAT). The body and headers are opaque to the substrate;
//! consumers (forge/alloy, Continuum, OpenClaw extensions, Hermes, ...)
//! own meaning.
//!
//! Anatomy:
//!   - `event_id`       — adapter-generated UUIDv4, stable across replay.
//!   - `sender` + `sender_client` — three-field identity: the durable
//!     `PeerId` and the per-session `ClientId` so multi-tab sessions can
//!     be disambiguated. Identity card (nick / role / bio) lives on
//!     `airc_core::Identity`, looked up via `sender`.
//!   - `channel`        — alias for `RoomId`; the abstract scope an event
//!     belongs to. Consumers vary the metaphor (room / activity / thread
//!     / lobby / project) but airc has one type.
//!   - `target`         — broadcast / direct-peer / sibling-room
//!     addressing, per `airc_core::MentionTarget`.
//!   - `lamport` + `occurred_at_ms` — logical clock + wall-clock pair;
//!     ordering uses lamport (transcript ordering survives skew) but
//!     consumers can render wall-clock for UI.
//!   - `reply_to`       — structured threading. Authoritative field.
//!     Adapters that only know headers see the same value at
//!     `airc.reply_to`; mismatch fails verification.
//!   - `headers`        — `airc_core::Headers` (BTreeMap<String,String>).
//!     Namespaces: `airc.*` substrate, `forge.*` contracts, `continuum.*`,
//!     `openclaw.*`, `hermes.*`, `opencode.*`, `x-*`. Substrate routes /
//!     filters on these without parsing body.
//!   - `body`           — opaque `airc_core::Body` (Json | Binary).
//!     Receiver decodes per `forge.body_hint` header.
//!   - `media`          — `Vec<MediaRef>` for blob attachments;
//!     content-addressed via `airc_core::ContentHash`.
//!
//! Auto-lift policy (send-path, abstracted as a trait): if a `body`
//! exceeds the integrator's threshold, the send path writes it to
//! `airc-blobs` and replaces `body: Some(...)` with `media.push(MediaRef
//! {...})`. The substrate never inlines blob bytes into the message
//! path. This shape supports both inline and lifted forms
//! **transparently** — an outside caller cannot tell which path the
//! sender took, and shouldn't have to. The receiver either reads `body`
//! or fetches the blob by `content_hash`; same payload either way.
//!
//! Pluggable: the `policy::LiftPolicy` trait is the decision interface,
//! with `SizeThresholdPolicy` (default 16 KiB), `NeverLift`, and
//! `AlwaysLift` as ready-to-use impls. Integrators (bridge daemon,
//! browser SDK, server, CLI) plug their own. See the project-wide
//! `feedback_blobs_never_in_db` rule.
//!   - `signature`      — `signature::Signature`. Ed25519 in production,
//!     explicit `Unsigned` in dev (policy-gated).

use serde::{Deserialize, Serialize};

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
};

use crate::media::MediaRef;
use crate::signature::Signature;

/// Alias matching the cross-system "channel" terminology used by
/// OpenClaw / Hermes / OpenCode docs. Internally airc has always called
/// them rooms; consumers may use either. One type, two names.
pub type ChannelId = RoomId;

/// What kind of frame the wire is carrying.
///
/// Three discrete delivery semantics, not a spectrum:
///   - `Message`  — pull-driven. Durable. Joins the transcript. Late
///     joiners see it on replay.
///   - `Event`    — push-driven. Interrupt-style. Receivers consume
///     immediately; the substrate may or may not persist. Use for
///     presence transitions, typing indicators, work-allocation pings.
///   - `Control`  — session lifecycle. JOIN / NICK / HEARTBEAT /
///     PRESENCE-from-roster. Adapters interpret directly; consumers
///     usually do not see these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Message,
    Event,
    Control,
}

/// One wire-level frame. A transport adapter carries `Frame`s in either
/// direction; the substrate decides delivery semantics by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub kind: FrameKind,
    pub envelope: Envelope,
}

/// The canonical "one event" record on the wire.
///
/// Every field is part of the canonical encoding for signature purposes
/// EXCEPT `signature` itself (the signer signs the rest; including the
/// sig in the signed bytes would be a chicken-and-egg). Receivers
/// canonical-encode the envelope minus `signature`, then verify the sig
/// against that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub event_id: EventId,
    pub sender: PeerId,
    pub sender_client: ClientId,
    pub channel: ChannelId,
    pub target: MentionTarget,
    pub lamport: u64,
    pub occurred_at_ms: u64,

    /// Authoritative threading reference. Substrate-typed; adapters MAY
    /// project this into `headers["airc.reply_to"]` for header-only
    /// gateways but the field is the truth. Mismatch between field and
    /// header fails `signature::verify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<EventId>,

    /// Routable envelope metadata. Cheap to inspect; namespaces own
    /// their own key prefixes (see `headers_keys` module).
    #[serde(default)]
    pub headers: Headers,

    /// Opaque consumer payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,

    /// Attached blob references. Empty vector for messages with no
    /// attachments — kept as `Vec` rather than `Option<Vec>` because
    /// "no attachments" and "empty attachments" are the same thing on
    /// the wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaRef>,

    pub signature: Signature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::Signature;
    use airc_core::transcript::MentionTarget;

    fn envelope_fixture() -> Envelope {
        Envelope {
            event_id: EventId::from_u128(0x01),
            sender: PeerId::from_u128(0xa1),
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target: MentionTarget::All,
            lamport: 7,
            occurred_at_ms: 1_700_000_000_000,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text("hello world")),
            media: Vec::new(),
            signature: Signature::Unsigned,
        }
    }

    #[test]
    fn envelope_roundtrips_through_serde_json() {
        let envelope = envelope_fixture();
        let encoded = serde_json::to_value(&envelope).unwrap();
        let decoded: Envelope = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn empty_optional_fields_are_skipped_on_serialization() {
        // Wire shape stays compact when fields are unset. Important for
        // canonical-bytes hashing (we want one canonical encoding per
        // semantic envelope, not per "did the sender bother to emit
        // null fields"). The skip_serializing_if directives pin this.
        let envelope = envelope_fixture();
        let encoded = serde_json::to_value(&envelope).unwrap();
        let obj = encoded.as_object().unwrap();
        assert!(!obj.contains_key("reply_to"));
        assert!(!obj.contains_key("media"));
        // body is Some so it should be present
        assert!(obj.contains_key("body"));
    }

    #[test]
    fn frame_kind_serializes_snake_case() {
        // Wire form is snake_case so cross-language consumers stay
        // happy (TS / Python / Go all read this without a custom
        // discriminator deserializer).
        assert_eq!(serde_json::to_value(FrameKind::Message).unwrap(), "message");
        assert_eq!(serde_json::to_value(FrameKind::Event).unwrap(), "event");
        assert_eq!(serde_json::to_value(FrameKind::Control).unwrap(), "control");
    }

    #[test]
    fn frame_wraps_envelope_for_dispatch() {
        let frame = Frame {
            kind: FrameKind::Message,
            envelope: envelope_fixture(),
        };
        let encoded = serde_json::to_value(&frame).unwrap();
        let decoded: Frame = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(decoded.kind, FrameKind::Message);
    }

    #[test]
    fn channel_id_is_an_alias_for_room_id() {
        // Cross-language docs say "channel"; airc-core says "room".
        // Equivalence at the type level so neither name surprises a
        // reader.
        let room: RoomId = RoomId::from_u128(0xc0ffee);
        let channel: ChannelId = room;
        assert_eq!(channel, room);
    }

    #[test]
    fn envelope_carries_attachments_when_present() {
        let mut envelope = envelope_fixture();
        envelope.media.push(MediaRef {
            file_id: airc_core::FileId::from_u128(0xf1),
            content_hash: airc_core::ContentHash("sha256:1234".to_string()),
            mime: Some("image/png".to_string()),
            size_bytes: Some(2048),
            caption: None,
        });
        let encoded = serde_json::to_value(&envelope).unwrap();
        let media = encoded["media"].as_array().unwrap();
        assert_eq!(media.len(), 1);
        let decoded: Envelope = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.media.len(), 1);
        assert_eq!(decoded.media[0].mime.as_deref(), Some("image/png"));
    }
}
