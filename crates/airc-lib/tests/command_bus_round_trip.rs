//! End-to-end round-trip test for the command-bus primitive.
//!
//! Two in-process `Airc` handles in the same machine-account home
//! exchange a request + reply. The substrate carries delivery +
//! correlation matching; the test confirms the requester's
//! `await_reply` resolves with the matching reply event.
//!
//! This is the proof point for Phase 4 of the GRID-SUBSTRATE-AUDIT:
//! consumers (Continuum/OpenClaw/Hermes/agent tool integrations)
//! can build typed request/reply on top of `Airc::request` +
//! `Airc::reply` + `Airc::await_reply`.

use std::net::SocketAddr;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use airc_core::{Body, Headers, MentionTarget, PeerId};
use airc_lib::{
    command_bus::PendingCommand, Airc, AircError, PeerSpec, TransportHealthSample,
    TransportHealthState, TransportKind, TransportRole,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair};
use airc_protocol::{HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_REPLY_TO};
use airc_relay::{RelayServer, RelayServerConfig};
use futures::stream::StreamExt;
use tempfile::TempDir;
use uuid::Uuid;

// HOME mutation needs to be serialised across tests in this file
// (matches the pattern in embedding_smoke.rs) so parallel
// `temp_env::with_var` calls don't race the global env.
static HOME_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[test]
fn request_and_reply_round_trip_via_shared_wire() {
    let machine = TempDir::new().expect("tempdir");
    let _home_env_guard = HOME_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    temp_env::with_var("HOME", Some(machine.path()), || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            round_trip_inner(machine.path()).await;
        });
    });
}

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

async fn round_trip_inner(machine: &std::path::Path) {
    // Two scopes under the same machine HOME — substrate routes
    // them onto the same wire via the local-fs adapter.
    let alice_home = machine.join("alice/.airc");
    let bob_home = machine.join("bob/.airc");
    std::fs::create_dir_all(alice_home.parent().unwrap()).unwrap();
    std::fs::create_dir_all(bob_home.parent().unwrap()).unwrap();

    let alice = Airc::open(&alice_home).await.expect("alice opens");
    let bob = Airc::open(&bob_home).await.expect("bob opens");

    // Both subscribe to the same channel so events fan out
    // between them via the shared wire.
    let _alice_room = alice.join("command-bus-test").await.unwrap();
    let _bob_room = bob.join("command-bus-test").await.unwrap();

    // Bob spawns a handler task: wait for a request, reply.
    let bob_handle = bob.clone();
    let handler = tokio::spawn(async move {
        let mut stream = bob_handle.subscribe().await.unwrap();
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    let Some(correlation) = event.headers.get(HEADER_AIRC_CORRELATION_ID) else {
                        continue;
                    };
                    let Some(reply_to) = event.headers.get(HEADER_AIRC_REPLY_TO) else {
                        continue;
                    };
                    // Don't reply to our own events.
                    if event.peer_id == bob_handle.peer_id() {
                        continue;
                    }
                    let correlation_id =
                        Uuid::parse_str(correlation).expect("valid correlation uuid");
                    let reply_to_peer =
                        PeerId::from_uuid(Uuid::parse_str(reply_to).expect("valid reply_to uuid"));
                    let mut headers = Headers::new();
                    headers.insert("forge.body_hint".into(), "test.result".into());
                    bob_handle
                        .reply(reply_to_peer, correlation_id, headers, Body::text("pong"))
                        .await
                        .expect("bob replies");
                    return;
                }
                Some(Err(_)) => continue,
                None => return,
            }
        }
    });

    // Give bob's subscriber a beat to attach so the request isn't
    // emitted before bob's stream is live. The substrate replays
    // recent broadcast buffer on attach, so this should be safe,
    // but in test we keep it deterministic.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut headers = Headers::new();
    headers.insert("airc.command_kind".into(), "test.ping".into());
    let pending: PendingCommand = alice
        .request(
            MentionTarget::All,
            headers,
            Body::text("ping"),
            Duration::from_secs(3),
        )
        .await
        .expect("alice issues request");

    let correlation_id = pending.correlation_id;
    let reply = alice.await_reply(pending).await.expect("alice gets reply");

    assert_eq!(
        reply.target,
        MentionTarget::Peer(alice.peer_id()),
        "reply must be directed at the requester"
    );
    // The reply must carry the same correlation id.
    assert_eq!(
        reply.headers.get(HEADER_AIRC_CORRELATION_ID),
        Some(&correlation_id.to_string()),
        "reply must carry the correlation_id"
    );
    // The reply body should be bob's "pong".
    assert_eq!(reply.body.as_ref().and_then(Body::as_text), Some("pong"));

    handler.await.expect("handler completes");
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

#[test]
fn await_reply_returns_command_deadline_when_no_reply() {
    let machine = TempDir::new().expect("tempdir");
    let _home_env_guard = HOME_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    temp_env::with_var("HOME", Some(machine.path()), || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            deadline_inner(machine.path()).await;
        });
    });
}

async fn deadline_inner(machine: &std::path::Path) {
    let home = machine.join("solo/.airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("airc opens");
    airc.join("deadline-test").await.unwrap();

    let mut headers = Headers::new();
    headers.insert("airc.command_kind".into(), "test.no_handler".into());
    let pending = airc
        .request(
            MentionTarget::All,
            headers,
            Body::text("ping"),
            Duration::from_millis(200),
        )
        .await
        .expect("request emits");
    let correlation_id = pending.correlation_id;

    let err = airc
        .await_reply(pending)
        .await
        .expect_err("no reply means deadline");
    match err {
        AircError::CommandDeadline {
            correlation_id: returned,
        } => {
            assert_eq!(returned, correlation_id);
        }
        other => panic!("expected CommandDeadline, got {other:?}"),
    }
}
