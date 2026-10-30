use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
};
use airc_protocol::{ChannelId, Envelope, Frame, FrameKind, Signature, Subscription};
use futures::StreamExt;

use crate::transport::Transport;
use crate::udp::{UdpAdapter, UdpConfig, UdpError};

fn frame(class: airc_core::EventClass, lamport: u64, target: MentionTarget, body: &str) -> Frame {
    Frame {
        kind: FrameKind::for_class(class),
        envelope: Envelope {
            class,
            event_id: EventId::from_u128(lamport as u128),
            sender: PeerId::from_u128(0xa1),
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target,
            lamport,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            media: Vec::new(),
            signature: Signature::Unsigned,
        },
    }
}

async fn bound_adapter(peers: HashMap<PeerId, SocketAddr>) -> (UdpAdapter, SocketAddr) {
    let adapter = UdpAdapter::new(UdpConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        peer_endpoints: peers,
    });
    let addr = adapter.bind().await.unwrap();
    (adapter, addr)
}

#[tokio::test]
async fn event_round_trips_between_two_udp_adapters() {
    let alice_id = PeerId::from_u128(0xa1);
    let bob_id = PeerId::from_u128(0xb2);

    let (bob, bob_addr) = bound_adapter(HashMap::new()).await;
    let mut alice_peers = HashMap::new();
    alice_peers.insert(bob_id, bob_addr);
    let (alice, _alice_addr) = bound_adapter(alice_peers).await;

    let mut bob_stream = bob.subscribe(Subscription::default()).await.unwrap();
    let outbound = frame(
        airc_core::EventClass::Activity(airc_core::ActivityKind::WorkReceipt),
        1,
        MentionTarget::Peer(bob_id),
        "udp hello",
    );

    alice.send(outbound.clone()).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(received, outbound);
    assert_eq!(received.envelope.sender, alice_id);
}

#[tokio::test]
async fn channel_and_typed_subscription_filter_udp_events() {
    let wanted = RoomId::from_u128(0x11);
    let other = RoomId::from_u128(0x22);
    let bob_id = PeerId::from_u128(0xb2);

    let (bob, bob_addr) = bound_adapter(HashMap::new()).await;
    let mut alice_peers = HashMap::new();
    alice_peers.insert(bob_id, bob_addr);
    let (alice, _alice_addr) = bound_adapter(alice_peers).await;

    let mut stream = bob
        .subscribe(Subscription {
            channel: Some(wanted),
            selection: airc_core::EventSelection::Exact(airc_core::EventClass::Activity(
                airc_core::ActivityKind::WorkReceipt,
            )),
            ..Default::default()
        })
        .await
        .unwrap();

    let mut wrong = frame(
        airc_core::EventClass::Activity(airc_core::ActivityKind::WorkReceipt),
        1,
        MentionTarget::Peer(bob_id),
        "wrong",
    );
    wrong.envelope.channel = other;
    let mut right = frame(
        airc_core::EventClass::Activity(airc_core::ActivityKind::WorkReceipt),
        2,
        MentionTarget::Peer(bob_id),
        "right",
    );
    right.envelope.channel = wanted;

    let mut wrong_class = frame(
        airc_core::EventClass::Activity(airc_core::ActivityKind::Intent),
        3,
        MentionTarget::Peer(bob_id),
        "right",
    );
    wrong_class.envelope.channel = wanted;
    alice.send(wrong).await.unwrap();
    alice.send(wrong_class).await.unwrap();
    alice.send(right.clone()).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(received, right);
}

#[tokio::test]
async fn durable_frames_fail_closed_on_udp() {
    let bob_id = PeerId::from_u128(0xb2);
    let mut peers = HashMap::new();
    peers.insert(bob_id, SocketAddr::from(([127, 0, 0, 1], 5555)));
    let (alice, _alice_addr) = bound_adapter(peers).await;

    let error = alice
        .send(frame(
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            1,
            MentionTarget::Peer(bob_id),
            "must not silently send",
        ))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        UdpError::UnsupportedDurableKind(FrameKind::Message)
    ));
}

#[tokio::test]
async fn unknown_direct_peer_fails_before_socket_send() {
    let (alice, _alice_addr) = bound_adapter(HashMap::new()).await;
    let missing = PeerId::from_u128(0x99);

    let error = alice
        .send(frame(
            airc_core::EventClass::Activity(airc_core::ActivityKind::WorkReceipt),
            1,
            MentionTarget::Peer(missing),
            "missing peer",
        ))
        .await
        .unwrap_err();

    assert!(matches!(error, UdpError::UnknownPeerEndpoint(peer) if peer == missing));
}
