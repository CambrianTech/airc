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

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use airc_core::PeerId;
use airc_lib::{endpoints_to_json, Airc, PeerSpec, RouteEndpoint};
use airc_protocol::{PeerKeyRegistry, PeerKeypair};
use airc_relay::{RelayServer, RelayServerConfig};
use tempfile::TempDir;

/// Self-healing join: endpoint writes carry a freshness stamp; these
/// tests simulate "the import just persisted a current advertisement",
/// so the stamp is simply now.
fn test_stamp_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

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
    airc_trust::set_endpoints_json(
        bob.home(),
        relay_peer,
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
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

/// #1247 slice 4 — self-election mechanism: a NODE promotes itself to a
/// relay (`become_relay`), advertises its own peer-id-bearing relay
/// endpoint, and a client that imports that endpoint (as it would from the
/// gist directory) discovers + connects to it. Proves the advertise →
/// discover → connect loop with a node-as-relay (no standalone server),
/// which is what the daemon's election trigger will drive.
#[tokio::test]
async fn a_node_becomes_a_relay_and_a_client_discovers_it() {
    use airc_lib::{TransportHealthState, TransportKind};

    let tmp_r = TempDir::new().expect("relay-node tempdir");
    let relay_node = Airc::open(tmp_r.path().join(".airc"))
        .await
        .expect("relay node open");
    let tmp_c = TempDir::new().expect("client tempdir");
    let client = Airc::open(tmp_c.path().join(".airc"))
        .await
        .expect("client open");

    // Mutual trust: the relay node allowlists the client (its relay server
    // serves enrolled peers), and the client pins the relay node.
    let relay_spec: PeerSpec = relay_node.peer_spec().parse().expect("relay spec");
    let client_spec: PeerSpec = client.peer_spec().parse().expect("client spec");
    relay_node
        .add_peer(client_spec)
        .await
        .expect("relay node trusts client");
    client
        .add_peer(relay_spec)
        .await
        .expect("client trusts relay node");

    // The relay node promotes itself, advertising under loopback (the
    // routable address for this same-machine test; in production the daemon
    // passes the node's detected LAN/Tailscale IPs).
    let relay_addr = relay_node
        .become_relay(
            "127.0.0.1:0".parse().unwrap(),
            Some(Ipv4Addr::LOCALHOST),
            None,
        )
        .await
        .expect("relay node becomes a relay");

    // It advertised its OWN connectable relay endpoint.
    let advertised = relay_node.route_endpoints().expect("relay endpoints");
    assert!(
        advertised
            .iter()
            .any(|e| e.connectable_relay() == Some((relay_node.peer_id(), relay_addr))),
        "a self-elected relay must advertise its own connectable endpoint; got: {advertised:?}"
    );

    // The client imports that endpoint (the gist-directory path) and
    // discovery connects to the node-hosted relay.
    let endpoints_json =
        endpoints_to_json(&[RouteEndpoint::relay(relay_node.peer_id(), relay_addr)])
            .expect("encode");
    airc_trust::set_endpoints_json(
        client.home(),
        relay_node.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
    .await
    .expect("store relay endpoint")
    .expect("relay node enrolled on client");

    let snapshot = client
        .refresh_route_discovery()
        .await
        .expect("client discovery refresh");
    assert!(
        snapshot
            .health
            .iter()
            .any(|h| h.kind == TransportKind::Relay && h.state == TransportHealthState::Healthy),
        "client must discover + connect the node-hosted relay (Relay Healthy); \
         health: {:?}",
        snapshot.health
    );
}

/// A relay server running on its own runtime, so the test can end it the way a
/// crashed or restarted relay ends: every task and socket gone at once.
struct DisposableRelay {
    addr: SocketAddr,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DisposableRelay {
    fn start(peer_id: PeerId, keypair: PeerKeypair, registry: Arc<PeerKeyRegistry>) -> Self {
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("relay runtime");
            let server = runtime
                .block_on(RelayServer::start(RelayServerConfig {
                    peer_id,
                    keypair,
                    registry,
                    bind: "127.0.0.1:0".parse().unwrap(),
                }))
                .expect("start relay");
            addr_tx
                .send(server.local_addr())
                .expect("report relay addr");
            let _ = stop_rx.recv();
            drop(server);
            drop(runtime); // every connection task and socket ends here
        });
        let addr = addr_rx.recv().expect("relay addr");
        Self {
            addr,
            stop: Some(stop_tx),
            thread: Some(thread),
        }
    }

    fn kill(mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            thread.join().expect("relay thread");
        }
    }
}

// what this catches (airc audit 2026-10-02): a relay session that dropped left its
// dead adapter installed, `connect_relay` returned Ok for every later call, and
// discovery counted the peer as dialed, so relay-only peers stayed dark until the
// daemon restarted. The drop must wake the owner, and the next connect must
// dial a fresh session.
#[tokio::test]
async fn a_dropped_relay_session_wakes_the_owner_and_is_redialed() {
    let relay_peer = PeerId::from_u128(0x_3e_1b);
    let relay_keypair = PeerKeypair::generate();
    let tmp = TempDir::new().expect("bob tempdir");
    let bob = Airc::open(tmp.path().join(".airc"))
        .await
        .expect("bob open");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    let allow_bob = || {
        let registry = Arc::new(PeerKeyRegistry::new());
        registry
            .enrol(bob_spec.peer_id, 1, bob_spec.pubkey)
            .expect("relay enrols bob");
        registry
    };
    bob.add_peer(PeerSpec {
        peer_id: relay_peer,
        pubkey: relay_keypair.public_bytes(),
    })
    .await
    .expect("bob trusts the relay");

    let (dropped_tx, mut dropped_rx) = tokio::sync::mpsc::unbounded_channel();
    bob.set_disconnect_observer(Arc::new(move |peer| {
        let _ = dropped_tx.send(peer);
    }));

    let first = DisposableRelay::start(relay_peer, relay_keypair.clone(), allow_bob());
    bob.connect_relay(first.addr, relay_peer)
        .await
        .expect("first relay connects");
    first.kill();

    let dropped = tokio::time::timeout(std::time::Duration::from_secs(10), dropped_rx.recv())
        .await
        .expect("the relay drop must wake the owner, not wait for a refresh tick");
    assert_eq!(dropped, Some(relay_peer));

    // The relay comes back (here on a new port, as after a restart). The dead
    // session must not answer for it.
    let second = DisposableRelay::start(relay_peer, relay_keypair.clone(), allow_bob());
    bob.connect_relay(second.addr, relay_peer)
        .await
        .expect("the restarted relay connects");
    // Only a session that was really redialed and installed can drop and wake the
    // owner a second time; the old code returned Ok here without dialing.
    second.kill();
    let dropped_again = tokio::time::timeout(std::time::Duration::from_secs(10), dropped_rx.recv())
        .await
        .expect("a session that was never redialed cannot drop a second time");
    assert_eq!(dropped_again, Some(relay_peer));
}

// what this catches (review of #1498): when two connects for DIFFERENT relays raced,
// the losing call closed its own session and still reported success for its relay,
// although the other relay held the route. Exactly one may succeed.
#[tokio::test]
async fn concurrent_connects_to_different_relays_never_both_succeed() {
    let tmp = TempDir::new().expect("bob tempdir");
    let bob = Airc::open(tmp.path().join(".airc"))
        .await
        .expect("bob open");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    let relay = |n: u128| {
        let peer = PeerId::from_u128(n);
        let keypair = PeerKeypair::generate();
        let registry = Arc::new(PeerKeyRegistry::new());
        registry
            .enrol(bob_spec.peer_id, 1, bob_spec.pubkey)
            .expect("relay enrols bob");
        (
            peer,
            keypair.clone(),
            DisposableRelay::start(peer, keypair, registry),
        )
    };
    let (peer_a, key_a, relay_a) = relay(0x_3e_1c);
    let (peer_b, key_b, relay_b) = relay(0x_3e_1d);
    for (peer, key) in [(peer_a, &key_a), (peer_b, &key_b)] {
        bob.add_peer(PeerSpec {
            peer_id: peer,
            pubkey: key.public_bytes(),
        })
        .await
        .expect("bob trusts the relay");
    }

    let (a, b) = tokio::join!(
        bob.connect_relay(relay_a.addr, peer_a),
        bob.connect_relay(relay_b.addr, peer_b)
    );
    assert_eq!(
        a.is_ok() as u8 + b.is_ok() as u8,
        1,
        "one relay session per handle: exactly one connect may succeed (a: {a:?}, b: {b:?})"
    );
    relay_a.kill();
    relay_b.kill();
}
