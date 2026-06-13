//! Cross-machine round-trip tests for the command-bus primitive.
//!
//! Two `Airc` handles on **separate** machine homes exchange a
//! request + reply over a real cross-machine transport (LAN, then
//! relay). The substrate carries delivery + correlation matching;
//! each test confirms the requester's `await_reply` resolves with
//! the matching reply event.
//!
//! Same-machine request/reply is proven through the daemon in
//! `airc-daemon/tests/owner_core_proof.rs`
//! (`request_response_rpc_correlates_across_kind_filtered_sessions`)
//! — the in-process shared-wire path is gone with `LocalFsAdapter`.

use std::net::SocketAddr;
use std::time::Duration;

use airc_core::{Body, Headers, MentionTarget, PeerId};
use airc_lib::{
    Airc, PeerSpec, TransportHealthSample, TransportHealthState, TransportKind, TransportRole,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair};
use airc_protocol::{HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_REPLY_TO};
use airc_relay::{RelayServer, RelayServerConfig};
use futures::stream::StreamExt;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn request_and_reply_round_trip_over_lan_without_github() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let alice_home = TempDir::new().expect("alice home");
        let bob_home = TempDir::new().expect("bob home");

        let alice = Airc::open(alice_home.path()).await.expect("alice opens");
        let bob = Airc::open(bob_home.path()).await.expect("bob opens");

        let alice_spec = alice.peer_spec().parse().expect("alice peer spec");
        let bob_spec = bob.peer_spec().parse().expect("bob peer spec");
        alice.add_peer(bob_spec).await.expect("alice trusts bob");
        bob.add_peer(alice_spec).await.expect("bob trusts alice");

        alice.join("command-bus-lan-test").await.unwrap();
        bob.join("command-bus-lan-test").await.unwrap();

        let bob_addr = bob
            .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bob listens on LAN");
        alice
            .connect_lan(bob_addr, bob.peer_id())
            .await
            .expect("alice connects to bob over LAN");

        let bob_handle = bob.clone();
        let handler = tokio::spawn(async move {
            let mut stream = bob_handle.subscribe().await.unwrap();
            loop {
                match stream.next().await {
                    Some(Ok(event)) => {
                        let Some(correlation) = event.headers.get(HEADER_AIRC_CORRELATION_ID)
                        else {
                            continue;
                        };
                        let Some(reply_to) = event.headers.get(HEADER_AIRC_REPLY_TO) else {
                            continue;
                        };
                        if event.peer_id == bob_handle.peer_id() {
                            continue;
                        }
                        let correlation_id =
                            Uuid::parse_str(correlation).expect("valid correlation uuid");
                        let reply_to_peer = PeerId::from_uuid(
                            Uuid::parse_str(reply_to).expect("valid reply_to uuid"),
                        );
                        let mut headers = Headers::new();
                        headers.insert("forge.body_hint".into(), "test.lan.result".into());
                        bob_handle
                            .reply(
                                reply_to_peer,
                                correlation_id,
                                headers,
                                Body::text("lan-pong"),
                            )
                            .await
                            .expect("bob replies over LAN");
                        return;
                    }
                    Some(Err(_)) => continue,
                    None => return,
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut headers = Headers::new();
        headers.insert("airc.command_kind".into(), "test.lan.ping".into());
        let pending = alice
            .request(
                MentionTarget::All,
                headers,
                Body::text("lan-ping"),
                Duration::from_secs(3),
            )
            .await
            .expect("alice issues LAN request");
        let correlation_id = pending.correlation_id;

        let reply = alice.await_reply(pending).await.expect("alice gets reply");
        assert_eq!(
            reply.target,
            MentionTarget::Peer(alice.peer_id()),
            "LAN reply must be directed at the requester"
        );
        assert_eq!(
            reply.headers.get(HEADER_AIRC_CORRELATION_ID),
            Some(&correlation_id.to_string()),
            "LAN reply must preserve the correlation id"
        );
        assert_eq!(
            reply.body.as_ref().and_then(Body::as_text),
            Some("lan-pong")
        );

        handler.await.expect("handler completes");
    });
}

#[test]
fn request_and_reply_round_trip_over_relay_without_github_or_lan() {
    ensure_crypto_provider();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let alice_home = TempDir::new().expect("alice home");
        let bob_home = TempDir::new().expect("bob home");

        let alice = Airc::open(alice_home.path()).await.expect("alice opens");
        let bob = Airc::open(bob_home.path()).await.expect("bob opens");

        let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
        let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
        alice
            .add_peer(bob_spec.clone())
            .await
            .expect("alice trusts bob");
        bob.add_peer(alice_spec.clone())
            .await
            .expect("bob trusts alice");

        let relay_peer = PeerId::new();
        let relay_keypair = PeerKeypair::generate();
        let relay_spec = PeerSpec {
            peer_id: relay_peer,
            pubkey: relay_keypair.public_bytes(),
        };
        alice
            .add_peer(relay_spec.clone())
            .await
            .expect("alice pins relay");
        bob.add_peer(relay_spec).await.expect("bob pins relay");

        alice.join("command-bus-relay-test").await.unwrap();
        bob.join("command-bus-relay-test").await.unwrap();

        let server_registry = std::sync::Arc::new(PeerKeyRegistry::new());
        server_registry
            .enrol(alice_spec.peer_id, 0, alice_spec.pubkey)
            .expect("relay trusts alice");
        server_registry
            .enrol(bob_spec.peer_id, 0, bob_spec.pubkey)
            .expect("relay trusts bob");
        let relay = RelayServer::start(RelayServerConfig {
            peer_id: relay_peer,
            keypair: relay_keypair,
            registry: server_registry,
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        })
        .await
        .expect("relay starts");
        let relay_addr = relay.local_addr();

        alice
            .connect_relay(relay_addr, relay_peer)
            .await
            .expect("alice connects to relay");
        bob.connect_relay(relay_addr, relay_peer)
            .await
            .expect("bob connects to relay");
        wait_for_relay_peers(&relay, &[alice.peer_id(), bob.peer_id()]).await;

        let relay_only = [TransportHealthSample {
            kind: TransportKind::Relay,
            role: TransportRole::Relay,
            state: TransportHealthState::Healthy,
            rtt_ms: None,
            success_ppm: None,
        }];
        alice.replace_transport_health(relay_only).unwrap();
        bob.replace_transport_health(relay_only).unwrap();

        let bob_handle = bob.clone();
        let handler = tokio::spawn(async move {
            let mut stream = bob_handle.subscribe().await.unwrap();
            loop {
                match stream.next().await {
                    Some(Ok(event)) => {
                        let Some(correlation) = event.headers.get(HEADER_AIRC_CORRELATION_ID)
                        else {
                            continue;
                        };
                        let Some(reply_to) = event.headers.get(HEADER_AIRC_REPLY_TO) else {
                            continue;
                        };
                        if event.peer_id == bob_handle.peer_id() {
                            continue;
                        }
                        let correlation_id =
                            Uuid::parse_str(correlation).expect("valid correlation uuid");
                        let reply_to_peer = PeerId::from_uuid(
                            Uuid::parse_str(reply_to).expect("valid reply_to uuid"),
                        );
                        let mut headers = Headers::new();
                        headers.insert("forge.body_hint".into(), "test.relay.result".into());
                        bob_handle
                            .reply(
                                reply_to_peer,
                                correlation_id,
                                headers,
                                Body::text("relay-pong"),
                            )
                            .await
                            .expect("bob replies over relay");
                        return;
                    }
                    Some(Err(_)) => continue,
                    None => return,
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut headers = Headers::new();
        headers.insert("airc.command_kind".into(), "test.relay.ping".into());
        let pending = alice
            .request(
                MentionTarget::All,
                headers,
                Body::text("relay-ping"),
                Duration::from_secs(3),
            )
            .await
            .expect("alice issues relay request");
        let correlation_id = pending.correlation_id;

        let reply = alice.await_reply(pending).await.expect("alice gets reply");
        assert_eq!(
            reply.target,
            MentionTarget::Peer(alice.peer_id()),
            "relay reply must be directed at the requester"
        );
        assert_eq!(
            reply.headers.get(HEADER_AIRC_CORRELATION_ID),
            Some(&correlation_id.to_string()),
            "relay reply must preserve the correlation id"
        );
        assert_eq!(
            reply.body.as_ref().and_then(Body::as_text),
            Some("relay-pong")
        );

        handler.await.expect("handler completes");
        relay.shutdown();
    });
}

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn wait_for_relay_peers(relay: &RelayServer, expected: &[PeerId]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let connected = relay.connected_peers().await;
        if expected.iter().all(|peer| connected.contains(peer)) {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "timeout waiting for relay peers; expected {:?}, connected {:?}",
                expected, connected
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
