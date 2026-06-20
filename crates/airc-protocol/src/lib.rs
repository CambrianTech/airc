//! airc-protocol — wire-level envelope, frame, signature, subscription.
//!
//! The substrate's protocol layer. Sits between `airc-core` (typed
//! primitives) and `airc-transport` (which carries bytes). What lives
//! here:
//!
//! - [`envelope`]       — `Envelope` + `Frame` + `FrameKind`
//! - [`media`]          — `MediaRef` (pointer into airc-blobs)
//! - [`headers_keys`]   — substrate-owned `airc.*` header constants
//! - [`signature`]      — `Signature`, `VerificationPolicy`, `verify()`
//!   with real Ed25519 verification
//! - [`keypair`]        — `PeerKeypair` (Ed25519 signing side)
//! - [`canonical`]      — deterministic CBOR encoding for signing
//! - [`subscription`]   — `Subscription` predicate for fan-out
//! - [`policy`]         — pluggable `LiftPolicy` for body-lift decisions
//!
//! Module split follows the CBAR substrate discipline (one concern per
//! file, small files). Public surface is re-exported at the crate root
//! so callers say `use airc_protocol::Envelope;` rather than reach
//! into module paths.

pub mod assertion;
pub mod canonical;
pub mod delivery_ack;
pub mod envelope;
pub mod handshake;
pub mod headers_keys;
pub mod keypair;
pub mod media;
pub mod policy;
pub mod rtc_signal;
pub mod session;
pub mod signature;
pub mod subscription;
pub mod transcript_conv;
pub mod trust_rotation;

// Re-exports — the stable public API surface.

pub use assertion::{AssertionError, IdentityAssertion, ASSERTION_DOMAIN};
pub use canonical::{canonical_signed_bytes, CanonicalError};
pub use delivery_ack::{
    decode_delivery_ack, wants_delivery_ack, DeliveryAck, DeliveryOutcome, UndeliverableReason,
    DELIVERY_ACK_REQUEST, DELIVERY_ACK_RESPONSE, HEADER_AIRC_DELIVERY_ACK,
};
pub use envelope::{ChannelId, Envelope, Frame, FrameKind};
pub use handshake::{
    initiate, respond, HandshakeError, HandshakeInit, HandshakeResp, PendingHandshake,
};
pub use headers_keys::{
    HEADER_AIRC_BODY_ENC_AAD, HEADER_AIRC_BODY_ENC_KEY_ID, HEADER_AIRC_BODY_ENC_SCHEME,
    HEADER_AIRC_CLIENT, HEADER_AIRC_COMMAND_KIND, HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_DEADLINE,
    HEADER_AIRC_MEDIA, HEADER_AIRC_PRIORITY, HEADER_AIRC_REPLY_TO, HEADER_AIRC_TRACE_ID,
    HEADER_FORGE_BODY_HINT,
};
pub use keypair::PeerKeypair;
pub use media::MediaRef;
pub use policy::{AlwaysLift, LiftPolicy, NeverLift, SizeThresholdPolicy};
pub use rtc_signal::{WebRtcSignal, WebRtcSignalKind, WEBRTC_SIGNAL_BODY_HINT};
pub use session::{SealedFrame, SessionError, SessionRole, StreamSession};
pub use signature::{
    verify, KeyError, PeerKeyRegistry, Signature, VerificationError, VerificationPolicy,
};
pub use subscription::Subscription;
pub use trust_rotation::{
    canonical_rotation_bytes, sign_rotation, verify_rotation, RotationVerificationError,
    TrustRotation,
};
