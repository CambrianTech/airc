//! Subscription — a peer's request for "send me the matching frames."
//!
//! Transport adapters use `Subscription` to express what their attached
//! consumer wants. The substrate fan-out path tests each outbound frame
//! against active subscriptions and routes accordingly. Subscriptions
//! are intentionally cheap — no closures, only data — so they survive
//! cross-process forwarding (e.g. a bridge daemon proxying a
//! subscription from one transport to another).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use airc_core::{HeaderFilter, TranscriptCursor};

use crate::envelope::{ChannelId, Frame, FrameKind};

/// What a subscriber wants delivered.
///
/// All criteria are AND-composed: a frame matches iff (channel matches
/// or is None) AND (kind is in `kinds` or `kinds` is empty) AND
/// (`headers_filter` matches). `from_cursor` is the replay anchor —
/// frames before this cursor are skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// Restrict to one channel. `None` = "any channel."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelId>,

    /// Replay anchor. `None` = "from now" (no replay). `Some(cursor)` =
    /// "deliver everything strictly after this cursor, then live."
    /// Used for catch-up after reconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_cursor: Option<TranscriptCursor>,

    /// Frame kinds the subscriber accepts. Empty set = "any kind."
    /// `BTreeSet` for deterministic serde ordering.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub kinds: BTreeSet<FrameKind>,

    /// Header predicate — uses the same `HeaderFilter` from airc-core
    /// (`Any` / `Exact` / `Prefix` / `All` / `AnyOf`). Default `Any`
    /// matches everything.
    #[serde(default = "default_any_filter")]
    pub headers_filter: HeaderFilter,
}

fn default_any_filter() -> HeaderFilter {
    HeaderFilter::Any
}

impl Default for Subscription {
    fn default() -> Self {
        Self {
            channel: None,
            from_cursor: None,
            kinds: BTreeSet::new(),
            headers_filter: HeaderFilter::Any,
        }
    }
}

impl Subscription {
    /// Predicate: does this subscription want this frame?
    ///
    /// Cheap to evaluate — no body parsing, no allocations beyond what
    /// `HeaderFilter::matches` does. Adapters call this in a hot loop
    /// during fan-out.
    pub fn matches(&self, frame: &Frame) -> bool {
        if let Some(channel) = self.channel {
            if frame.envelope.channel != channel {
                return false;
            }
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&frame.kind) {
            return false;
        }
        if !self.headers_filter.matches(&frame.envelope.headers) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Frame, FrameKind};
    use crate::signature::Signature;
    use airc_core::{
        headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
    };

    fn frame_with(channel: ChannelId, kind: FrameKind, headers: Vec<(&str, &str)>) -> Frame {
        let mut h = Headers::new();
        for (k, v) in headers {
            h.insert(k.to_string(), v.to_string());
        }
        Frame {
            kind,
            envelope: Envelope {
                event_id: EventId::from_u128(0x01),
                sender: PeerId::from_u128(0xa1),
                sender_client: ClientId::from_u128(0xc1),
                channel,
                target: MentionTarget::All,
                lamport: 1,
                occurred_at_ms: 1_700_000_000_000,
                reply_to: None,
                headers: h,
                body: Some(Body::text("hi")),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    #[test]
    fn default_subscription_matches_any_frame() {
        // The "give me everything" subscription is the construction
        // for an unbounded firehose consumer (debugger / audit log).
        let sub = Subscription::default();
        let frame = frame_with(RoomId::from_u128(0xa), FrameKind::Message, vec![]);
        assert!(sub.matches(&frame));
    }

    #[test]
    fn channel_restriction_drops_other_channels() {
        let sub = Subscription {
            channel: Some(RoomId::from_u128(0xa)),
            ..Default::default()
        };
        assert!(sub.matches(&frame_with(
            RoomId::from_u128(0xa),
            FrameKind::Message,
            vec![]
        )));
        assert!(!sub.matches(&frame_with(
            RoomId::from_u128(0xb),
            FrameKind::Message,
            vec![]
        )));
    }

    #[test]
    fn empty_kinds_set_means_all_kinds() {
        // Important distinction from "no kinds specified means no
        // frames" — the empty set is the wildcard.
        let sub = Subscription::default();
        for kind in [FrameKind::Message, FrameKind::Event, FrameKind::Control] {
            assert!(sub.matches(&frame_with(RoomId::from_u128(0xa), kind, vec![])));
        }
    }

    #[test]
    fn kind_restriction_drops_other_kinds() {
        let mut kinds = BTreeSet::new();
        kinds.insert(FrameKind::Event);
        let sub = Subscription {
            kinds,
            ..Default::default()
        };
        assert!(sub.matches(&frame_with(
            RoomId::from_u128(0xa),
            FrameKind::Event,
            vec![]
        )));
        assert!(!sub.matches(&frame_with(
            RoomId::from_u128(0xa),
            FrameKind::Message,
            vec![]
        )));
    }

    #[test]
    fn header_filter_composes_with_channel_and_kind() {
        // All three criteria AND together. Adapter fan-out path: cheap
        // multi-axis match without touching body.
        let mut kinds = BTreeSet::new();
        kinds.insert(FrameKind::Message);
        let sub = Subscription {
            channel: Some(RoomId::from_u128(0xa)),
            from_cursor: None,
            kinds,
            headers_filter: HeaderFilter::Prefix {
                key: "forge.body_hint".to_string(),
                value_prefix: "forge.persona.".to_string(),
            },
        };
        assert!(sub.matches(&frame_with(
            RoomId::from_u128(0xa),
            FrameKind::Message,
            vec![("forge.body_hint", "forge.persona.turn")]
        )));
        // Wrong kind → drop
        assert!(!sub.matches(&frame_with(
            RoomId::from_u128(0xa),
            FrameKind::Event,
            vec![("forge.body_hint", "forge.persona.turn")]
        )));
        // Wrong header prefix → drop
        assert!(!sub.matches(&frame_with(
            RoomId::from_u128(0xa),
            FrameKind::Message,
            vec![("forge.body_hint", "forge.code.review")]
        )));
    }

    #[test]
    fn subscription_serde_roundtrips() {
        let mut kinds = BTreeSet::new();
        kinds.insert(FrameKind::Message);
        kinds.insert(FrameKind::Event);
        let sub = Subscription {
            channel: Some(RoomId::from_u128(0xc0ffee)),
            from_cursor: Some(TranscriptCursor {
                lamport: 42,
                event_id: EventId::from_u128(0xc),
            }),
            kinds,
            headers_filter: HeaderFilter::Exact {
                key: "forge.body_hint".to_string(),
                value: "forge.persona.turn".to_string(),
            },
        };
        let encoded = serde_json::to_value(&sub).unwrap();
        let decoded: Subscription = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, sub);
    }
}
