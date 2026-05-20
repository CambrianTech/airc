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

pub mod canonical;
pub mod envelope;
pub mod headers_keys;
pub mod keypair;
pub mod media;
pub mod policy;
pub mod signature;
pub mod subscription;
pub mod transcript_conv;
pub mod trust_rotation;

// Re-exports — the stable public API surface.

pub use canonical::{canonical_signed_bytes, CanonicalError};
pub use envelope::{ChannelId, Envelope, Frame, FrameKind};
pub use headers_keys::{
    HEADER_AIRC_DEADLINE, HEADER_AIRC_PRIORITY, HEADER_AIRC_REPLY_TO, HEADER_AIRC_TRACE_ID,
    HEADER_FORGE_BODY_HINT,
};
pub use keypair::PeerKeypair;
pub use media::MediaRef;
pub use policy::{AlwaysLift, LiftPolicy, NeverLift, SizeThresholdPolicy};
pub use signature::{
    verify, KeyError, PeerKeyRegistry, Signature, VerificationError, VerificationPolicy,
};
pub use subscription::Subscription;
pub use trust_rotation::{
    canonical_rotation_bytes, sign_rotation, verify_rotation, RotationVerificationError,
    TrustRotation,
};
