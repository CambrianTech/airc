//! Who an envelope could be addressed to, as the durable tier indexes it.
//!
//! The router never parses a payload to ROUTE; this is the one place the
//! durable tier reads it, to index addressing beside the stored row (the
//! envelope bytes stay untouched). A payload that is not an airc `Body` has no
//! text and so no leading mention, exactly as the SDK projects it.

use airc_core::{Addressing, Body};

use crate::{Envelope, Target};

/// The addressing of an envelope's parts: a typed peer target, and a `Body`
/// payload whose text opens with an `@name`.
pub fn addressing_of(target: &Target, payload: &[u8]) -> Addressing {
    let peer = match target {
        Target::Peer(peer) => Some(*peer),
        Target::All | Target::Endpoint(_) | Target::Reply(_) | Target::Capability(_) => None,
    };
    Addressing::of(peer, Body::from_payload(payload).ok().as_ref())
}

impl Envelope {
    pub fn addressing(&self) -> Addressing {
        addressing_of(&self.target, &self.payload)
    }

    /// Whether this envelope matches a bounded history scan's predicate.
    pub fn matches_history(&self, rule: airc_core::HistoryMatch<'_>) -> bool {
        rule.matches_with(&self.headers, || self.addressing())
    }
}
