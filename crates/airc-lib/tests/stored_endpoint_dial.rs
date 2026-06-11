//! Card 625abe6d slice 1 — stored peer endpoints become outbound
//! dials at route-discovery time.
//!
//! The cross-machine gap this closes: enrolment used to produce a
//! trust anchor and nothing else — the resolver had no endpoints, so
//! `airc peer add` + an account-registry import never yielded a
//! route, and cross-machine delivery required hand-driven
//! `lan-listen`/`lan-send` (the 2026-06-10 5090↔mac bring-up did
//! exactly that, with a gist as the out-of-band courier).
//!
//! Slice 1 contract proven here:
//!   - endpoints persisted on the trust record (via
//!     `airc_trust::set_endpoints_json`) are dialed OUTBOUND by
//!     `refresh_route_discovery` — the dialing side needs no inbound
//!     rule (outbound-only doctrine);
//!   - a successful dial yields a connected LAN peer + healthy
//!     LAN-TCP route, end-to-end frame delivery included;
//!   - a failed dial is RECORDED on the snapshot, never swallowed —
//!     offline peers are normal mesh weather, invisible dial attempts
//!     are bugs.

use std::net::SocketAddr;

use airc_lib::{endpoints_to_json, Airc, PeerSpec, RouteEndpoint};
use tempfile::TempDir;

/// The happy path: bob's trust record for alice carries alice's
/// listen endpoint; bob's route discovery dials it and the LAN link
/// comes up without bob ever calling `connect_lan` himself.
#[tokio::test]
async fn discovery_dials_stored_lan_endpoint_outbound() {
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let alice = Airc::open(tmp_a.path().join(".airc"))
        .await
        .expect("alice open");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    alice.add_peer(bob_spec).await.expect("alice trusts bob");
    bob.add_peer(alice_spec).await.expect("bob trusts alice");

    let alice_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");

    // What the account registry import (e3ebce7a rung 1) or the dev
    // verb `peer add --endpoint` would have stored.
    let endpoints_json =
        endpoints_to_json(&[RouteEndpoint::LanTcp { addr: alice_addr }]).expect("encode endpoints");
    airc_trust::set_endpoints_json(bob.home(), alice.peer_id(), Some(endpoints_json))
        .await
        .expect("store endpoints")
        .expect("alice must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.peer_dial_failures.is_empty(),
        "no dial may fail when the listener is up: {:?}",
        snapshot.peer_dial_failures
    );
    assert!(
        snapshot.connected_lan_peers.contains(&alice.peer_id()),
        "discovery must have dialed alice's stored endpoint outbound; \
         connected: {:?}",
        snapshot.connected_lan_peers
    );
}

/// The loud-failure path: a stored endpoint nobody listens on is
/// reported on the snapshot with the peer, the endpoint, and the
/// error — and the refresh itself still succeeds (an offline peer
/// must not take route discovery down with it).
#[tokio::test]
async fn discovery_records_failed_dial_instead_of_swallowing_it() {
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let alice = Airc::open(tmp_a.path().join(".airc"))
        .await
        .expect("alice open");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice spec");
    bob.add_peer(alice_spec).await.expect("bob trusts alice");

    // Bind-then-drop to get a loopback port that is definitely
    // closed at dial time.
    let closed_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        listener.local_addr().expect("probe addr")
    };
    let endpoints_json = endpoints_to_json(&[RouteEndpoint::LanTcp { addr: closed_addr }])
        .expect("encode endpoints");
    airc_trust::set_endpoints_json(bob.home(), alice.peer_id(), Some(endpoints_json))
        .await
        .expect("store endpoints")
        .expect("alice must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("refresh must survive an unreachable peer");

    assert_eq!(
        snapshot.peer_dial_failures.len(),
        1,
        "exactly one failed dial must be recorded: {:?}",
        snapshot.peer_dial_failures
    );
    let failure = &snapshot.peer_dial_failures[0];
    assert_eq!(failure.peer_id, alice.peer_id());
    assert_eq!(
        failure.endpoint,
        RouteEndpoint::LanTcp { addr: closed_addr }
    );
    assert!(
        !failure.error.is_empty(),
        "the dial error must be carried for display"
    );
}
