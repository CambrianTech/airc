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

use std::net::{Ipv4Addr, SocketAddr};

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

/// The dual-advertise contract: `listen_lan_advertising` binds ONE
/// wildcard listener and publishes BOTH the LAN and the Tailscale
/// address under the same port, LAN sorted first. This is the daemon's
/// connection ladder (local → LAN → Tailscale → grid): a same-subnet
/// peer dials the LAN address directly and Tailscale is dialed only if
/// the peer has left the LAN. Earlier the daemon advertised Tailscale
/// exclusively, forcing every same-LAN peer through a wasted 100.x hop.
#[tokio::test]
async fn advertise_publishes_both_lan_and_tailscale_lan_first() {
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join(".airc")).await.expect("open");

    let lan_ip = Ipv4Addr::new(192, 168, 1, 50);
    let tailscale_ip = Ipv4Addr::new(100, 79, 156, 3);
    let advertised = airc
        .listen_lan_advertising(Some(lan_ip), Some(tailscale_ip))
        .await
        .expect("advertise both");

    let endpoints = airc.route_endpoints().expect("read endpoints");
    assert_eq!(
        endpoints, advertised,
        "the method's return value must mirror the advertised table"
    );
    assert_eq!(endpoints.len(), 2, "exactly LAN + Tailscale: {endpoints:?}");

    // LAN sorts before Tailscale (RouteEndpointKind order) so the dialer
    // tries it first and breaks on success — Tailscale only off-LAN.
    let (lan_port, ts_port) = match (&endpoints[0], &endpoints[1]) {
        (RouteEndpoint::LanTcp { addr: lan }, RouteEndpoint::TailscaleTcp { addr: ts }) => {
            assert_eq!(lan.ip(), std::net::IpAddr::V4(lan_ip));
            assert_eq!(ts.ip(), std::net::IpAddr::V4(tailscale_ip));
            (lan.port(), ts.port())
        }
        other => panic!("expected [LanTcp, TailscaleTcp] in order, got {other:?}"),
    };
    assert_eq!(
        lan_port, ts_port,
        "one wildcard listener → both endpoints share its port"
    );
    assert_ne!(lan_port, 0, "the OS-assigned port must be concrete");
    // The wildcard bind address itself is NEVER advertised — peers only
    // ever receive specific, dialable IPs.
    assert!(
        !endpoints.iter().any(|endpoint| matches!(
            endpoint,
            RouteEndpoint::LanTcp { addr } if addr.ip().is_unspecified()
        )),
        "0.0.0.0 must never be advertised: {endpoints:?}"
    );
}

/// End-to-end ladder pin: a peer that imports BOTH advertised endpoints
/// connects via the LAN rung and never touches the (unreachable, off-box)
/// Tailscale rung. The wildcard listener accepts the loopback dial, so we
/// advertise 127.0.0.1 as the "LAN" address and a real-range 100.x that
/// nothing answers — discovery must connect with ZERO failures, proving
/// LAN-first-break (Tailscale only if we leave the LAN).
#[tokio::test]
async fn peer_dials_lan_rung_and_skips_tailscale() {
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

    // Loopback stands in for the LAN IP (reachable via the wildcard
    // listener); the 100.x Tailscale address is off-box and unreachable.
    let advertised = alice
        .listen_lan_advertising(
            Some(Ipv4Addr::LOCALHOST),
            Some(Ipv4Addr::new(100, 79, 156, 3)),
        )
        .await
        .expect("alice advertises both");

    let endpoints_json = endpoints_to_json(&advertised).expect("encode endpoints");
    airc_trust::set_endpoints_json(bob.home(), alice.peer_id(), Some(endpoints_json))
        .await
        .expect("store endpoints")
        .expect("alice must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.connected_lan_peers.contains(&alice.peer_id()),
        "bob must connect via the LAN rung: connected {:?}, failures {:?}",
        snapshot.connected_lan_peers,
        snapshot.peer_dial_failures
    );
    assert!(
        snapshot.peer_dial_failures.is_empty(),
        "LAN-first must break before the unreachable Tailscale rung is \
         ever dialed — no failure should be recorded: {:?}",
        snapshot.peer_dial_failures
    );
}

/// #1120 sentinel BLOCKING-1 regression — the split-topology keystone.
///
/// On every REAL machine `home != wire_root`. The registry import
/// writes endpoints to the wire-root store but ALSO creates an
/// endpoint-less home-store row for the same peer (via
/// `import_invite_beacon` → `add_peer`). A first-record-wins dedupe
/// let that endpoint-less shadow consume the peer's slot, so the
/// endpoint-carrying record never dialed — zero dials, zero recorded
/// failures, silently, in production, while single-store hermetic
/// tests stayed green. Endpoints are now MERGED per peer across both
/// stores; this test runs the REAL import path on a split topology
/// and demands the dial happens.
#[tokio::test]
async fn split_store_import_still_dials_no_silent_shadow() {
    use airc_lib::{
        beacon_now, AccountPeerBeacon, AccountRegistryDocument, ChannelName, MeshIdentity,
    };

    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let alice = Airc::open(tmp_a.path().join(".airc"))
        .await
        .expect("alice open");
    // Bob gets the real-machine shape: scope home and machine-account
    // wire root are DIFFERENT stores.
    let bob_scope = tmp_b.path().join("scope/.airc");
    let bob_wire = tmp_b.path().join("machine/.airc");
    std::fs::create_dir_all(&bob_scope).expect("bob scope dir");
    std::fs::create_dir_all(&bob_wire).expect("bob wire dir");
    let bob = Airc::open_with_wire_root_for_test(&bob_scope, &bob_wire)
        .await
        .expect("bob open split");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    alice.add_peer(bob_spec).await.expect("alice trusts bob");

    let alice_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");

    let channel = ChannelName::new("general").expect("channel");
    let document = AccountRegistryDocument::new(
        MeshIdentity::new("test-account"),
        2_000,
        vec![channel.clone()],
        vec![AccountPeerBeacon {
            presence: beacon_now(
                alice.peer_id(),
                tmp_a.path().join(".airc"),
                vec![channel],
                123,
                1_000,
            ),
            peer_spec: alice_spec,
            endpoints: vec![RouteEndpoint::LanTcp { addr: alice_addr }],
        }],
    );
    bob.import_account_registry_document(document)
        .await
        .expect("bob imports registry");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.connected_lan_peers.contains(&alice.peer_id()),
        "the registry-import → dial path must connect on a split \
         home/wire_root topology; a silent zero-dial here is the \
         #1120 blocking-1 shadow regressing. connected: {:?}, \
         failures: {:?}",
        snapshot.connected_lan_peers,
        snapshot.peer_dial_failures
    );
}

/// BIGMAMA review BLOCKING-2 on PR #1201 — the OFF-LAN cost test.
///
/// The publisher daemon publishes ONE beacon per account-registry, so
/// every account peer (same-LAN AND off-LAN) imports BOTH advertised
/// endpoints. The dialer in `discovery.rs` walks them in
/// `RouteEndpointKind` order (LanTcp first) for every peer, with NO
/// subnet/reachability gate. Off-LAN peers MUST therefore dial the
/// publisher's unreachable LAN rung FIRST, eat the full
/// `PEER_DIAL_TIMEOUT = 3s`, and only then fall through to the
/// reachable Tailscale rung.
///
/// This test pins that cost so the off-LAN penalty is visible and
/// intentional, not accidental:
///   - dead LAN rung (bind-then-drop on a loopback port that nothing
///     answers) sorted first;
///   - live "Tailscale" rung (stood in by a real listening loopback —
///     `RouteEndpointKind::TailscaleTcp`, sorted second);
///   - dialer connects via the second rung;
///   - exactly one failure is recorded (the LAN one), carrying the
///     "dial timed out" marker so an operator can see WHY the LAN
///     rung was skipped, not just that it was;
///   - the recorded refresh time is at least `PEER_DIAL_TIMEOUT` —
///     the cost is REAL, not imagined.
///
/// When the dialer eventually gets a same-subnet reachability gate
/// (BIGMAMA's "real fix" #3) this test will need to be updated to
/// pin a faster off-LAN path; until then, the substrate's truthful
/// behavior is "off-LAN peers pay 3s before Tailscale connects."
#[tokio::test]
async fn off_lan_peer_pays_lan_dial_timeout_then_connects_via_tailscale() {
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

    // Alice's REAL listener — what we'll publish as the Tailscale rung
    // (the only one bob can reach).
    let alice_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");

    // The LAN rung is a bind-then-drop loopback port that nothing
    // answers. On Linux/macOS the kernel returns ECONNREFUSED
    // instantly; on Windows the client retries SYN for ~2s before
    // returning, so the closed-port shape is platform-sensitive.
    // The four load-bearing asserts below (Tailscale-connects,
    // exactly-one-LAN-failure, failure-is-LAN-rung, error-nonempty)
    // are the actual proof of the dialer's rung-order contract; the
    // wall-clock bound is sanity only — bounded by `PEER_DIAL_TIMEOUT`
    // so it works on both kernels without papering over a stall.
    // BIGMAMA review BLOCKING-fix on PR #1201: prior `< 2s` ceiling
    // tripped on Windows-in-matrix CI (~2.03-2.05s deterministic);
    // `< PEER_DIAL_TIMEOUT` is the correct universal bound — a dead
    // rung that exceeds the per-dial deadline is a real failure
    // we'd want surfaced.
    let dead_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        listener.local_addr().expect("probe addr")
    };

    let endpoints_json = endpoints_to_json(&[
        RouteEndpoint::LanTcp { addr: dead_addr },
        RouteEndpoint::TailscaleTcp { addr: alice_addr },
    ])
    .expect("encode endpoints");
    airc_trust::set_endpoints_json(bob.home(), alice.peer_id(), Some(endpoints_json))
        .await
        .expect("store endpoints")
        .expect("alice must be enrolled on bob");

    let started = std::time::Instant::now();
    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");
    let elapsed = started.elapsed();

    assert!(
        snapshot.connected_lan_peers.contains(&alice.peer_id()),
        "bob must connect via the Tailscale rung after the LAN rung \
         fails: connected {:?}, failures {:?}",
        snapshot.connected_lan_peers,
        snapshot.peer_dial_failures
    );
    assert_eq!(
        snapshot.peer_dial_failures.len(),
        1,
        "exactly the dead LAN rung may fail; the Tailscale rung must \
         have connected before any retry: {:?}",
        snapshot.peer_dial_failures
    );
    let failure = &snapshot.peer_dial_failures[0];
    assert_eq!(
        failure.endpoint,
        RouteEndpoint::LanTcp { addr: dead_addr },
        "the recorded failure MUST be the LAN rung — that's the off-LAN \
         penalty made visible: {failure:?}"
    );
    assert!(
        !failure.error.is_empty(),
        "the LAN-rung error must be recorded for display — operator must \
         see WHY this peer's first dial slot was wasted"
    );
    // Sanity bound: dial loop must complete within PEER_DIAL_TIMEOUT
    // even with one rung dead. Universal across kernels — Linux/macOS
    // refuse instantly; Windows SYN-retries for ~2s. A stall past
    // this bound would mean a rung isn't honoring the per-dial
    // deadline, which IS a real bug we'd want surfaced.
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "off-LAN dial loop must complete within PEER_DIAL_TIMEOUT \
         (3s) even with the LAN rung dead — anything past this means \
         a rung isn't honoring the per-dial deadline. took {elapsed:?}"
    );
}

/// Cost-order + one-success-per-peer + bounded-dial pins: with a dead
/// endpoint stored FIRST and a live one second, discovery records
/// exactly one failure (the dead one, in order) and still connects via
/// the second — and the dead endpoint cannot stall the refresh beyond
/// the per-dial deadline (#1120 blocking-2).
#[tokio::test]
async fn dial_walks_endpoints_in_stored_order_and_stops_on_success() {
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
    let dead_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        listener.local_addr().expect("probe addr")
    };

    let endpoints_json = endpoints_to_json(&[
        RouteEndpoint::LanTcp { addr: dead_addr },
        RouteEndpoint::LanTcp { addr: alice_addr },
    ])
    .expect("encode endpoints");
    airc_trust::set_endpoints_json(bob.home(), alice.peer_id(), Some(endpoints_json))
        .await
        .expect("store endpoints")
        .expect("alice must be enrolled on bob");

    let started = std::time::Instant::now();
    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.connected_lan_peers.contains(&alice.peer_id()),
        "second (live) endpoint must connect after the first fails: {:?}",
        snapshot.peer_dial_failures
    );
    assert_eq!(
        snapshot.peer_dial_failures.len(),
        1,
        "exactly the dead first endpoint may fail: {:?}",
        snapshot.peer_dial_failures
    );
    assert_eq!(
        snapshot.peer_dial_failures[0].endpoint,
        RouteEndpoint::LanTcp { addr: dead_addr },
        "the recorded failure must be the FIRST stored endpoint (order pin)"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "a dead endpoint must not stall the refresh past the per-dial \
         deadline; took {:?}",
        started.elapsed()
    );
}
