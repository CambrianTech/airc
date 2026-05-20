//! End-to-end integration test: two `RelayAdapter`s connect to one
//! `RelayServer`, a frame from Alice routes through the relay to Bob.
//!
//! This is the proof PR-E baseline ships: the kernel-level transport
//! contract works at the cross-machine substitute boundary (the relay
//! stands in for "we can't reach each other directly").

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::StreamExt;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
};
use airc_protocol::{
    ChannelId, Envelope, Frame, FrameKind, PeerKeyRegistry, PeerKeypair, Signature, Subscription,
};
use airc_relay::{RelayServer, RelayServerConfig};
use airc_transport::relay::{RelayAdapter, RelayClientConfig};
use airc_transport::transport::Transport;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

struct TestIdentity {
    peer_id: PeerId,
    keypair: PeerKeypair,
}

impl TestIdentity {
    fn new(peer_id: u128) -> Self {
        Self {
            peer_id: PeerId::from_u128(peer_id),
            keypair: PeerKeypair::generate(),
        }
    }
}

fn enrolled_registry(identities: &[&TestIdentity]) -> Arc<RwLock<PeerKeyRegistry>> {
    let registry = Arc::new(RwLock::new(PeerKeyRegistry::new()));
    {
        let mut w = registry.write().unwrap();
        for id in identities {
            w.enrol(id.peer_id, 1, id.keypair.public_bytes()).unwrap();
        }
    }
    registry
}

async fn wait_for_peers(server: &RelayServer, expected: &[PeerId]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let connected = server.connected_peers().await;
        if expected.iter().all(|p| connected.contains(p)) {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "timeout waiting for peers; expected {:?}, currently connected {:?}",
                expected, connected,
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn message_frame(sender: PeerId, lamport: u64) -> Frame {
    Frame {
        kind: FrameKind::Message,
        envelope: Envelope {
            event_id: EventId::from_u128(lamport as u128),
            sender,
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from(RoomId::from_u128(0xc0c0)),
            target: MentionTarget::All,
            lamport,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text("hello bob through the relay")),
            media: Vec::new(),
            signature: Signature::Unsigned,
        },
    }
}

#[tokio::test]
async fn frame_round_trips_from_alice_through_relay_to_bob() {
    ensure_crypto_provider();

    let relay = TestIdentity::new(0xff);
    let alice = TestIdentity::new(0xa1);
    let bob = TestIdentity::new(0xb2);

    // Server allowlists alice + bob (clients).
    let server_registry = enrolled_registry(&[&alice, &bob]);
    // Each client pins the relay's identity.
    let alice_registry = enrolled_registry(&[&relay]);
    let bob_registry = enrolled_registry(&[&relay]);

    let server = RelayServer::start(RelayServerConfig {
        peer_id: relay.peer_id,
        keypair: relay.keypair,
        registry: server_registry,
        bind: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .expect("start relay");
    let relay_addr = server.local_addr();

    let alice_adapter = RelayAdapter::new(RelayClientConfig {
        self_peer_id: alice.peer_id,
        self_keypair: alice.keypair,
        relay_peer_id: relay.peer_id,
        relay_addr,
        registry: alice_registry,
    });
    alice_adapter.connect().await.expect("alice connect");

    let bob_adapter = RelayAdapter::new(RelayClientConfig {
        self_peer_id: bob.peer_id,
        self_keypair: bob.keypair,
        relay_peer_id: relay.peer_id,
        relay_addr,
        registry: bob_registry,
    });
    bob_adapter.connect().await.expect("bob connect");

    // Server must have registered both connections before Alice sends.
    // Polling beats a fixed sleep — passes faster on a healthy box,
    // fails loud on a stuck handshake.
    wait_for_peers(&server, &[alice.peer_id, bob.peer_id]).await;

    let mut bob_stream = bob_adapter
        .subscribe(Subscription {
            kinds: BTreeSet::from([FrameKind::Message]),
            ..Default::default()
        })
        .await
        .expect("bob subscribe");

    let outbound = message_frame(alice.peer_id, 1);
    alice_adapter
        .send(outbound.clone())
        .await
        .expect("alice send");

    let received = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
        .await
        .expect("relay did not deliver frame within 2s")
        .expect("bob subscription closed before delivery")
        .expect("bob received a transport error");

    assert_eq!(received.envelope.event_id, outbound.envelope.event_id);
    assert_eq!(received.envelope.sender, alice.peer_id);
    assert_eq!(received.kind, FrameKind::Message);
    assert_eq!(
        received.envelope.body, outbound.envelope.body,
        "relay must forward body bytes unchanged"
    );

    server.shutdown();
}

#[tokio::test]
async fn unenrolled_client_is_never_registered_at_server() {
    // Security guarantee: the server's `PinnedClientVerifier` rejects
    // certs whose Ed25519 pubkey isn't enrolled. The client's
    // `connect()` may return Ok (TLS 1.3 handshake on the client side
    // can complete before the server-side rejection propagates — same
    // observation lan_tcp's `unenrolled_peer_cannot_handshake` test
    // documents). The truth is on the server: an imposter MUST NOT
    // appear in `connected_peers()`.
    ensure_crypto_provider();

    let relay = TestIdentity::new(0xff);
    let alice = TestIdentity::new(0xa1);
    let imposter = TestIdentity::new(0xee); // not on the allowlist

    let server_registry = enrolled_registry(&[&alice]); // imposter NOT enrolled
    let imposter_registry = enrolled_registry(&[&relay]);

    let server = RelayServer::start(RelayServerConfig {
        peer_id: relay.peer_id,
        keypair: relay.keypair,
        registry: server_registry,
        bind: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .expect("start relay");

    let imposter_adapter = RelayAdapter::new(RelayClientConfig {
        self_peer_id: imposter.peer_id,
        self_keypair: imposter.keypair,
        relay_peer_id: relay.peer_id,
        relay_addr: server.local_addr(),
        registry: imposter_registry,
    });
    // Outcome of connect() is intentionally not asserted — see comment
    // above. The verifier reject lands as either a client-side Err
    // (handshake) or as a half-open connection the server drops.
    let _ = imposter_adapter.connect().await;

    // Give the server time to process+reject the handshake.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let connected = server.connected_peers().await;
    assert!(
        !connected.contains(&imposter.peer_id),
        "imposter must NEVER appear in server.connected_peers(); got {connected:?}",
    );

    server.shutdown();
}
