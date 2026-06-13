//! Adapter-only tests. End-to-end (server + adapter) integration tests
//! live in `airc-relay`'s `tests/` directory because they need both
//! sides and `airc-relay` already depends on this crate — wiring a
//! dev-dependency back here would be circular.

use std::sync::Arc;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
};
use airc_protocol::{
    ChannelId, Envelope, Frame, FrameKind, PeerKeyRegistry, PeerKeypair, Signature,
};

use super::{RelayAdapter, RelayClientConfig, RelayClientError};
use crate::transport::Transport;

fn dummy_frame(sender: PeerId) -> Frame {
    Frame {
        kind: FrameKind::Message,
        envelope: Envelope {
            event_id: EventId::from_u128(1),
            sender,
            sender_client: ClientId::from_u128(2),
            channel: ChannelId::from(RoomId::from_u128(3)),
            target: MentionTarget::All,
            lamport: 1,
            occurred_at_ms: 0,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text("noop")),
            media: Vec::new(),
            signature: Signature::Unsigned,
        },
    }
}

fn adapter_without_connect() -> RelayAdapter {
    RelayAdapter::new(RelayClientConfig {
        self_peer_id: PeerId::from_u128(0xa1),
        self_keypair: PeerKeypair::generate(),
        relay_peer_id: PeerId::from_u128(0xff),
        relay_addr: "127.0.0.1:0".parse().unwrap(),
        registry: Arc::new(PeerKeyRegistry::new()),
    })
}

#[tokio::test]
async fn send_before_connect_returns_not_connected() {
    let adapter = adapter_without_connect();
    let result = adapter.send(dummy_frame(PeerId::from_u128(0xa1))).await;
    assert!(matches!(result, Err(RelayClientError::NotConnected)));
}

#[tokio::test]
async fn send_oversized_frame_fails_loudly_not_silently() {
    let adapter = adapter_without_connect();
    let mut big_body = String::with_capacity(17 * 1024 * 1024);
    big_body.extend(std::iter::repeat_n('A', 17 * 1024 * 1024));
    let mut frame = dummy_frame(PeerId::from_u128(0xa1));
    frame.envelope.body = Some(Body::text(&big_body));
    let result = adapter.send(frame).await;
    assert!(
        matches!(result, Err(RelayClientError::FrameTooLarge { .. })),
        "oversized frame must error rather than silently drop, got {result:?}",
    );
}
