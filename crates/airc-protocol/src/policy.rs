//! Body-lift policy — abstract decision interface for "should this body
//! be lifted to airc-blobs storage instead of carried inline?"
//!
//! Send-path concern. The substrate exposes the trait; integrators wire
//! it up with their blob store, runtime, and consumer-specific rules.
//! Pure decision interface — actual blob I/O lives in the integrator.
//! The substrate enforces only the *shape*: blobs go to disk (or
//! ORM-managed external storage), never inline in the wire envelope or
//! DB rows. See the project-wide `feedback_blobs_never_in_db` rule.
//!
//! Why a trait, not a knob: different consumers have different lift
//! policies. A browser SDK may lift aggressively (small max-inline) to
//! keep WebSocket frames tiny; a LAN-TCP bridge may inline larger
//! payloads. The substrate stays neutral — every integrator plugs the
//! policy that fits.

use airc_core::Body;

/// Decision interface: does this body warrant lifting to a blob?
///
/// Implementors return `true` when the send path should serialize the
/// body to `airc-blobs` storage and replace `Envelope.body` with a
/// `MediaRef` pointer. Returning `false` keeps the body inline.
pub trait LiftPolicy {
    /// Pure decision. No I/O. Implementors may inspect the body shape
    /// (`Body::Json` vs `Body::Binary`) or its serialized size.
    fn should_lift_body(&self, body: &Body) -> bool;
}

/// Lift bodies whose JSON encoding exceeds `max_inline_bytes`.
///
/// Cheap pre-check: the same JSON encoding is what most transports
/// carry, so size is an accurate proxy. For transports using a
/// different encoding (e.g. canonical CBOR for signing) the JSON size
/// is still a reasonable estimate.
pub struct SizeThresholdPolicy {
    pub max_inline_bytes: usize,
}

impl SizeThresholdPolicy {
    /// Default threshold — 16 KiB. Picked to keep messages well under
    /// typical transport frame limits while not lifting trivial
    /// payloads.
    pub const DEFAULT_MAX_INLINE: usize = 16 * 1024;

    pub fn new(max_inline_bytes: usize) -> Self {
        Self { max_inline_bytes }
    }
}

impl Default for SizeThresholdPolicy {
    fn default() -> Self {
        Self {
            max_inline_bytes: Self::DEFAULT_MAX_INLINE,
        }
    }
}

impl LiftPolicy for SizeThresholdPolicy {
    fn should_lift_body(&self, body: &Body) -> bool {
        // serde_json::to_vec on a Body type can fail only if a
        // user-supplied Json value contains a non-encodable shape
        // (e.g. a Map with non-string keys, which serde_json refuses).
        // Treat "can't measure" as "don't lift" — the failure will
        // surface downstream when the transport tries to serialize.
        serde_json::to_vec(body)
            .map(|encoded| encoded.len() > self.max_inline_bytes)
            .unwrap_or(false)
    }
}

/// Never lift — keep every body inline. Useful for tests and for
/// transports that handle their own chunking.
pub struct NeverLift;

impl LiftPolicy for NeverLift {
    fn should_lift_body(&self, _body: &Body) -> bool {
        false
    }
}

/// Always lift any body. Useful for transports that REQUIRE every
/// payload to live in blob storage (e.g. for replication discipline
/// or audit trails).
pub struct AlwaysLift;

impl LiftPolicy for AlwaysLift {
    fn should_lift_body(&self, _body: &Body) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_threshold_lifts_bodies_over_limit() {
        let policy = SizeThresholdPolicy::new(64);
        // Short body — under threshold.
        let small = Body::text("hi");
        assert!(!policy.should_lift_body(&small));

        // Long body — over threshold. Padding the text past 64 bytes
        // of JSON-encoded form is enough.
        let long = Body::text("x".repeat(200));
        assert!(policy.should_lift_body(&long));
    }

    #[test]
    fn size_threshold_default_is_16_kib() {
        let policy = SizeThresholdPolicy::default();
        assert_eq!(policy.max_inline_bytes, 16 * 1024);
        // A 32 KiB string body MUST lift under the default.
        let big = Body::text("x".repeat(32 * 1024));
        assert!(policy.should_lift_body(&big));
    }

    #[test]
    fn never_lift_keeps_everything_inline() {
        let policy = NeverLift;
        let big = Body::text("x".repeat(64 * 1024));
        assert!(!policy.should_lift_body(&big));
    }

    #[test]
    fn always_lift_lifts_even_tiny_bodies() {
        let policy = AlwaysLift;
        let tiny = Body::text("a");
        assert!(policy.should_lift_body(&tiny));
    }

    #[test]
    fn binary_body_lifts_when_oversized() {
        // The trait works equally for Body::Binary — substrate doesn't
        // care which variant the consumer carries.
        let policy = SizeThresholdPolicy::new(64);
        let large_bytes = vec![0u8; 200];
        let binary = Body::Binary(large_bytes);
        assert!(policy.should_lift_body(&binary));
    }

    #[test]
    fn custom_policy_via_trait_object() {
        // The trait is dyn-compatible so integrators can swap policies
        // at runtime (e.g. config-driven). This test pins that
        // contract.
        fn lift_decision(policy: &dyn LiftPolicy, body: &Body) -> bool {
            policy.should_lift_body(body)
        }
        let strict = SizeThresholdPolicy::new(8);
        let loose = NeverLift;
        let body = Body::text("hello world");
        assert!(lift_decision(&strict, &body));
        assert!(!lift_decision(&loose, &body));
    }
}
