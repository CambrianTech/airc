//! #1247 slice 2 — a relay endpoint stored on a peer's trust record
//! becomes an outbound relay CONNECTION at route-discovery time, and the
//! relay route is marked Healthy.
//!
//! This is the cross-subnet half of the #1243 fix: BigMama (10.0.1.x) and
//! the Macs (192.168.1.x) cannot dial each other directly (firewall drops
//! SYN), so room broadcast must traverse a relay both can reach. Slice 1
//! made the relay endpoint carry its peer id (dialable + pinnable from the
//! gist); this slice makes `refresh_route_discovery` actually connect it.
//!
//! Contract proven here:
//!   - a `RouteEndpoint::relay(relay_peer, relay_addr)` persisted on a
//!     trust record (what the account-registry gist import would store)
//!     is connected OUTBOUND by `refresh_route_discovery`, mTLS-pinned to
//!     the relay's enrolled identity — no manual `connect_relay` call;
//!   - a successful connect yields a Healthy `Relay` transport in the
//!     route-health snapshot.

use std::net::SocketAddr;
use std::sync::Arc;

use airc_core::PeerId;
use airc_lib::{endpoints_to_json, Airc, PeerSpec, RouteEndpoint};
use airc_protocol::{PeerKeyRegistry, PeerKeypair};
use airc_relay::{RelayServer, RelayServerConfig};
use tempfile::TempDir;

#[tokio::test]
async fn discovery_connects_stored_relay_endpoint_and_marks_it_healthy() {
    use airc_lib::{TransportHealthState, TransportKind};

    // The relay's own identity (the node clients pin + dial).
    let relay_peer = PeerId::from_u128(0x_3e_1a);
    let relay_keypair = PeerKeypair::generate();

    let tmp_b = TempDir::new().expect("bob tempdir");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");

    // The relay server allowlists its clients (bob); bob in turn pins the
    // relay's identity (enrolled below via `add_peer`).
    let server_registry = Arc::new(PeerKeyRegistry::new());
    server_registry
        .enrol(bob_spec.peer_id, 1, bob_spec.pubkey)
        .expect("relay server enrols bob");

    let server = RelayServer::start(RelayServerConfig {
        peer_id: relay_peer,
        keypair: relay_keypair.clone(),
        registry: server_registry,
        bind: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .expect("start relay");
    let relay_addr: SocketAddr = server.local_addr();

    // Enrol the relay as a trusted peer on bob (pins its pubkey for the
    // mTLS handshake) — exactly what importing the relay's gist beacon
    // would do.
    bob.add_peer(PeerSpec {
        peer_id: relay_peer,
        pubkey: relay_keypair.public_bytes(),
    })
    .await
    .expect("bob trusts the relay");

    // What the account-registry gist import (slice 4) would persist: the
    // relay's endpoint, carrying its peer id so it's connectable + pinnable.
    let endpoints_json =
        endpoints_to_json(&[RouteEndpoint::relay(relay_peer, relay_addr)]).expect("encode");
    airc_trust::set_endpoints_json(bob.home(), relay_peer, Some(endpoints_json))
        .await
        .expect("store relay endpoint")
        .expect("relay must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.peer_dial_failures.is_empty(),
        "relay connect must not fail when the relay is up: {:?}",
        snapshot.peer_dial_failures
    );
    let relay_health = snapshot
        .health
        .iter()
        .find(|h| h.kind == TransportKind::Relay);
    assert!(
        matches!(
            relay_health.map(|h| h.state),
            Some(TransportHealthState::Healthy)
        ),
        "discovery must connect the stored relay endpoint and mark Relay Healthy; \
         health table: {:?}",
        snapshot.health
    );

    server.shutdown();
}
