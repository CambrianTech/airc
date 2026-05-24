//! Integration: control event over UDP without GitHub or LAN-TCP.
//!
//! Proves the `TransportKind::Udp` execution arm sends a frame
//! through `UdpAdapter` end-to-end between two Airc instances on
//! separate homes. UDP/WebRTC are policy-restricted to
//! `RouteClass::{ControlInteractive, MediaSignaling, PresenceEphemeral}`
//! (see `route::policy::allows`) — UDP correctly refuses to carry
//! `DataInteractive` (the lossy-tolerant property is the whole point).
//! So this test uses `FrameKind::Event` (which maps to
//! `ControlInteractive`) via `Airc::send_frame_to_for_test`, the
//! doc-hidden test alias matching the `teardown_wire_for_test` pattern
//! from #923.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use airc_lib::{
    Airc, Body, Headers, MentionTarget, PeerSpec, TransportHealthSample, TransportHealthState,
    TransportKind, TransportRole,
};
use airc_protocol::FrameKind;
use futures::stream::StreamExt;
use tempfile::TempDir;
use tokio::net::UdpSocket;

static CRYPTO_INIT: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

fn ensure_crypto_provider() {
    let mut guard = CRYPTO_INIT.lock().unwrap();
    if !*guard {
        let _ = rustls::crypto::ring::default_provider().install_default();
        *guard = true;
    }
}

async fn ephemeral_loopback_addr() -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("ephemeral bind");
    let addr = sock.local_addr().expect("local_addr");
    drop(sock);
    addr
}

#[test]
fn control_event_over_udp_without_lan_or_relay() {
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

        alice.join("udp-route-test").await.unwrap();
        bob.join("udp-route-test").await.unwrap();

        let alice_addr = ephemeral_loopback_addr().await;
        let bob_addr = ephemeral_loopback_addr().await;

        alice
            .bind_udp(alice_addr, HashMap::new())
            .await
            .expect("alice binds udp");
        bob.bind_udp(bob_addr, HashMap::new())
            .await
            .expect("bob binds udp");

        alice
            .add_udp_peer(bob_spec.peer_id, bob_addr)
            .await
            .expect("alice registers bob udp endpoint");
        bob.add_udp_peer(alice_spec.peer_id, alice_addr)
            .await
            .expect("bob registers alice udp endpoint");

        // Force route resolver to pick UDP — no other healthy route.
        let udp_only = [TransportHealthSample {
            kind: TransportKind::Udp,
            role: TransportRole::Direct,
            state: TransportHealthState::Healthy,
            rtt_ms: None,
            success_ppm: None,
        }];
        alice.replace_transport_health(udp_only).unwrap();
        bob.replace_transport_health(udp_only).unwrap();

        // Bob's live subscriber must be armed before Alice sends —
        // matches the readiness discipline from #948.
        let bob_handle = bob.clone();
        let bob_peer_id = bob.peer_id();
        let alice_peer_id = alice.peer_id();
        let receiver = tokio::spawn(async move {
            let mut stream = bob_handle.subscribe().await.unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                    Ok(Some(Ok(event))) => {
                        if event.peer_id == bob_peer_id {
                            continue;
                        }
                        if event.peer_id != alice_peer_id {
                            continue;
                        }
                        if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                            if text == "udp-control-ping" {
                                return Some(event);
                            }
                        }
                    }
                    Ok(Some(Err(_))) => continue,
                    Ok(None) => return None,
                    Err(_) => continue,
                }
            }
            None
        });

        // Small delay to let bob's subscriber actually attach.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut headers = Headers::new();
        headers.insert("airc.command_kind".into(), "test.udp.control".into());
        alice
            .send_frame_to_for_test(
                FrameKind::Event,
                MentionTarget::Peer(bob_spec.peer_id),
                Body::text("udp-control-ping"),
                headers,
            )
            .await
            .expect("alice sends event over udp route");

        let event = receiver
            .await
            .expect("receiver task joined")
            .expect("bob received udp-routed event within deadline");
        assert_eq!(
            event.body.as_ref().and_then(Body::as_text),
            Some("udp-control-ping")
        );
    });
}
