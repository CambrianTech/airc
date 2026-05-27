//! Subscription filter — a compiled predicate over the envelope (§3.1).
//!
//! A subscription compiles to a predicate so one subscription can span many
//! rooms (continuum's wildcard/pattern subscriptions). Slice 1 carries the
//! channel + kind + header dimensions; the predicate is evaluated cheaply on
//! the hot path after the O(1) channel-index lookup.

use airc_core::{HeaderFilter, RoomId};

use crate::envelope::{DeliveryClass, Envelope, Kind};

/// What a subscriber wants. An empty filter (`Filter::channel(c)`) matches
/// every envelope on the channel.
#[derive(Debug, Clone)]
pub struct Filter {
    /// The channel to subscribe to. Slice 1 indexes on a single channel; a
    /// later slice compiles cross-room patterns to a predicate over many.
    pub channel: RoomId,
    /// If set, only these kinds match.
    pub kinds: Option<Vec<Kind>>,
    /// If set, only these delivery classes match.
    pub delivery: Option<Vec<DeliveryClass>>,
    /// Header predicate (`HeaderFilter::Any` = match all).
    pub headers: HeaderFilter,
}

impl Filter {
    /// A filter that matches everything on `channel`.
    pub fn channel(channel: RoomId) -> Self {
        Self {
            channel,
            kinds: None,
            delivery: None,
            headers: HeaderFilter::Any,
        }
    }

    /// Restrict to these kinds.
    pub fn with_kinds(mut self, kinds: Vec<Kind>) -> Self {
        self.kinds = Some(kinds);
        self
    }

    /// Restrict to these delivery classes.
    pub fn with_delivery(mut self, delivery: Vec<DeliveryClass>) -> Self {
        self.delivery = Some(delivery);
        self
    }

    /// Restrict by header predicate.
    pub fn with_headers(mut self, headers: HeaderFilter) -> Self {
        self.headers = headers;
        self
    }

    /// Evaluate the predicate against an envelope already known to be on this
    /// filter's channel.
    pub fn matches(&self, env: &Envelope) -> bool {
        if env.channel != self.channel {
            return false;
        }
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&env.kind) {
                return false;
            }
        }
        if let Some(delivery) = &self.delivery {
            if !delivery.contains(&env.delivery) {
                return false;
            }
        }
        self.headers.matches(&env.headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{ClientId, PeerId};
    use bytes::Bytes;

    fn env(kind: Kind, delivery: DeliveryClass) -> Envelope {
        Envelope::new(
            RoomId::from_u128(1),
            (PeerId::from_u128(1), ClientId::from_u128(1)),
            kind,
            delivery,
            Bytes::new(),
        )
    }

    #[test]
    fn channel_only_matches_all() {
        let f = Filter::channel(RoomId::from_u128(1));
        assert!(f.matches(&env(Kind::Message, DeliveryClass::Durable)));
    }

    #[test]
    fn wrong_channel_never_matches() {
        let f = Filter::channel(RoomId::from_u128(2));
        assert!(!f.matches(&env(Kind::Message, DeliveryClass::Durable)));
    }

    #[test]
    fn kind_and_delivery_restrict() {
        let f = Filter::channel(RoomId::from_u128(1))
            .with_kinds(vec![Kind::Message])
            .with_delivery(vec![DeliveryClass::Durable]);
        assert!(f.matches(&env(Kind::Message, DeliveryClass::Durable)));
        assert!(!f.matches(&env(Kind::Signal, DeliveryClass::Durable)));
        assert!(!f.matches(&env(Kind::Message, DeliveryClass::EphemeralLatest)));
    }
}
