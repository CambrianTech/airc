//! The error type for the wire codec.

use thiserror::Error;

/// A failure decoding a wire envelope, or an inconsistency in the buffer.
///
/// [`crate::encode`] is infallible (it builds a well-formed FlatBuffer from
/// a valid [`airc_bus::Envelope`]), so every variant here is a *decode*
/// concern: a truncated/garbled buffer, a missing required field, or an
/// unknown enum tag from a newer/foreign producer.
#[derive(Debug, Error)]
pub enum WireError {
    /// The buffer is not a valid FlatBuffer root, or a field access walked
    /// off the buffer / hit a malformed offset. Wraps the underlying
    /// `planus` read error.
    #[error("malformed wire buffer: {0}")]
    Malformed(#[from] planus::Error),

    /// A field that a well-formed envelope must carry was absent. Carries
    /// the field name so the caller can see which one.
    ///
    /// Required fields are the four routing ids (`event_id`, `channel`,
    /// `peer_id`, `client_id`) and the `payload`. The owner-stamped scalars
    /// (`epoch`, `counter`, `occurred_at_ms`) and the enums have FlatBuffers
    /// schema defaults, so they are never "missing" — they decode to their
    /// default. Genuinely-optional Rust fields (`correlation_id`,
    /// `coalesce_key`, the per-variant `target_*`) decode to `None` when
    /// absent, never an error.
    #[error("wire envelope missing required field: {0}")]
    MissingField(&'static str),
}
