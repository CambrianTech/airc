//! Typed envelope addressing. Payload bytes never participate in routing.

use airc_core::Addressing;

use crate::{Envelope, Target};

/// The addressed peer supplied by the producer.
pub fn addressing_of(target: &Target) -> Addressing {
    let peer = match target {
        Target::Peer(peer) => Some(*peer),
        Target::All | Target::Endpoint(_) | Target::Reply(_) | Target::Capability(_) => None,
    };
    Addressing::of(peer)
}

impl Envelope {
    pub fn addressing(&self) -> Addressing {
        addressing_of(&self.target)
    }

    /// Whether this envelope matches a bounded history scan's predicate.
    pub fn matches_history(&self, rule: &airc_core::HistoryMatch<'_>) -> bool {
        rule.matches_with(
            self.class,
            self.kind.transcript_kind(),
            self.from,
            &self.headers,
            || self.addressing(),
        )
    }
}
