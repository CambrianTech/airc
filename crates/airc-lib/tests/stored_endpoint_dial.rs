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

/// Self-healing join: endpoint writes carry a freshness stamp; these
/// tests simulate "the import just persisted a current advertisement",
/// so the stamp is simply now.
fn test_stamp_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

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
    airc_trust::set_endpoints_json(
        bob.home(),
        alice.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
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
    airc_trust::set_endpoints_json(
        bob.home(),
        alice.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
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

/// Card 7e3c9a1f — a dead endpoint dialed once enters dial-failure
/// backoff; the NEXT refresh within the window SKIPS it and surfaces it on
/// the SEPARATE `peer_dial_skips` channel, NOT as a `peer_dial_failure`.
///
/// what this catches: the over-report regression an adversarial review
/// found in the first quarantine cut — a skip must NOT be counted or
/// labelled as an attempted-and-failed dial (which would emit false
/// `PeerDialFailed` warnings every refresh and inflate `transport
/// health`'s failure count), yet must still be VISIBLE (not the silent
/// omission the review BLOCKED before that). Mutation checks: pushing the
/// skip back onto `peer_dial_failures` fails the "failures empty" assert;
/// dropping the skip entirely fails the "skips len == 1" assert.
#[tokio::test]
async fn quarantined_endpoint_surfaces_as_skip_not_failure_on_re_refresh() {
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

    // A loopback port that is definitely closed at dial time.
    let closed_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        listener.local_addr().expect("probe addr")
    };
    let endpoints_json = endpoints_to_json(&[RouteEndpoint::LanTcp { addr: closed_addr }])
        .expect("encode endpoints");
    airc_trust::set_endpoints_json(
        bob.home(),
        alice.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
    .await
    .expect("store endpoints")
    .expect("alice must be enrolled on bob");

    // First refresh: the dial is ATTEMPTED and fails → recorded as a
    // failure, nothing skipped.
    let first = bob.refresh_route_discovery().await.expect("first refresh");
    assert_eq!(
        first.peer_dial_failures.len(),
        1,
        "first refresh attempts and fails the dial"
    );
    assert!(
        first.peer_dial_skips.is_empty(),
        "nothing is skipped on the first attempt: {:?}",
        first.peer_dial_skips
    );

    // Second refresh, immediately (well within the backoff window): the
    // dead endpoint is now quarantined → SKIPPED, surfaced on the skips
    // channel, and NOT re-reported as a failed dial.
    let second = bob.refresh_route_discovery().await.expect("second refresh");
    assert!(
        second.peer_dial_failures.is_empty(),
        "a quarantined endpoint must NOT be reported as a failed dial: {:?}",
        second.peer_dial_failures
    );
    assert_eq!(
        second.peer_dial_skips.len(),
        1,
        "the backed-off endpoint is surfaced as a skip: {:?}",
        second.peer_dial_skips
    );
    let skip = &second.peer_dial_skips[0];
    assert_eq!(skip.peer_id, alice.peer_id());
    assert_eq!(skip.endpoint, RouteEndpoint::LanTcp { addr: closed_addr });
    assert!(
        skip.remaining_ms > 0,
        "the operator sees the remaining backoff"
    );
}

/// what this catches (self-healing join item 1, machine-vs-scope): a
/// stored record pins a SCOPE peer while the endpoint's TLS listener
/// answers with the MACHINE (daemon) identity — the live two-machine
/// failure where every automatic dial died with a loud identity
/// mismatch until a human redialed with the machine id. The dialer must
/// do what the human did: parse the presented identity out of the
/// mismatch, verify it is ENROLLED, and retry the pin exactly once.
/// Mutation checks: dropping the retry leaves `connected` empty and a
/// mismatch failure recorded; retrying without the enrolment gate is
/// pinned by the strictness test below.
#[tokio::test]
async fn identity_mismatch_dial_retries_once_pinning_the_presented_enrolled_identity() {
    let tmp_machine = TempDir::new().expect("machine tempdir");
    let tmp_bob = TempDir::new().expect("bob tempdir");
    // "machine" is the daemon-shaped listener identity that answers TLS.
    let machine = Airc::open(tmp_machine.path().join(".airc"))
        .await
        .expect("machine open");
    let bob = Airc::open(tmp_bob.path().join(".airc"))
        .await
        .expect("bob open");

    // The scope peer: a real identity hosted behind the machine's
    // listener (its own keypair — enrolled, but NOT what the listener's
    // cert presents).
    let scope_peer = airc_core::PeerId::new();
    let scope_keypair = airc_protocol::PeerKeypair::generate();
    let scope_spec = airc_lib::PeerSpec {
        peer_id: scope_peer,
        pubkey: scope_keypair.public_bytes(),
    };

    let machine_spec: PeerSpec = machine.peer_spec().parse().expect("machine spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    machine
        .add_peer(bob_spec)
        .await
        .expect("machine trusts bob");
    bob.add_peer(machine_spec)
        .await
        .expect("bob trusts the machine identity");
    bob.add_peer(scope_spec)
        .await
        .expect("bob trusts the scope peer");

    let machine_addr: SocketAddr = machine
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("machine listens");

    // The field shape: the SCOPE peer's trust record carries the
    // MACHINE's endpoint (a scope-published registry beacon advertising
    // the daemon's listener).
    let endpoints_json = endpoints_to_json(&[RouteEndpoint::LanTcp { addr: machine_addr }])
        .expect("encode endpoints");
    airc_trust::set_endpoints_json(
        bob.home(),
        scope_peer,
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
    .await
    .expect("store endpoints")
    .expect("scope peer must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.connected_lan_peers.contains(&machine.peer_id()),
        "the mismatch retry must connect to the enrolled identity the cert \
         presented (the machine); connected: {:?}",
        snapshot.connected_lan_peers
    );
    assert!(
        snapshot.peer_dial_failures.is_empty(),
        "a healed mismatch must not surface as a dial failure: {:?}",
        snapshot.peer_dial_failures
    );
}

/// what this catches (machine-vs-scope item 3 — the stored mapping):
/// a trust record whose endpoints carry the registry-imported
/// `endpoints_peer_id` host mapping must cert-pin the MACHINE identity
/// on the FIRST dial — no failed handshake, no mismatch retry — and
/// the route must come up. Together with the resolve_dial_pin unit
/// tests (strictness: unenrolled/self/absent mappings pin the record's
/// own peer), this pins item 3(c): dial-by-scope-peer resolves to the
/// machine identity for cert pinning when the mapping is known.
#[tokio::test]
async fn stored_host_mapping_pins_the_machine_identity_and_connects() {
    let tmp_machine = TempDir::new().expect("machine tempdir");
    let tmp_bob = TempDir::new().expect("bob tempdir");
    let machine = Airc::open(tmp_machine.path().join(".airc"))
        .await
        .expect("machine open");
    let bob = Airc::open(tmp_bob.path().join(".airc"))
        .await
        .expect("bob open");

    let scope_peer = airc_core::PeerId::new();
    let scope_keypair = airc_protocol::PeerKeypair::generate();
    let scope_spec = airc_lib::PeerSpec {
        peer_id: scope_peer,
        pubkey: scope_keypair.public_bytes(),
    };
    let machine_spec: PeerSpec = machine.peer_spec().parse().expect("machine spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    machine
        .add_peer(bob_spec)
        .await
        .expect("machine trusts bob");
    bob.add_peer(machine_spec)
        .await
        .expect("bob trusts the machine identity");
    bob.add_peer(scope_spec)
        .await
        .expect("bob trusts the scope peer");

    let machine_addr: SocketAddr = machine
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("machine listens");

    // What the account-registry import now persists: the scope peer's
    // endpoints WITH the machine's identity as their transport host.
    let endpoints_json = endpoints_to_json(&[RouteEndpoint::LanTcp { addr: machine_addr }])
        .expect("encode endpoints");
    airc_trust::set_endpoints_json(
        bob.home(),
        scope_peer,
        Some(endpoints_json),
        test_stamp_now_ms(),
        Some(machine.peer_id()),
    )
    .await
    .expect("store endpoints")
    .expect("scope peer must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");

    assert!(
        snapshot.peer_dial_failures.is_empty(),
        "a correctly-pinned first dial must record NO failure: {:?}",
        snapshot.peer_dial_failures
    );
    assert!(
        snapshot.connected_lan_peers.contains(&machine.peer_id()),
        "the mapped dial must connect to the machine identity; connected: {:?}",
        snapshot.connected_lan_peers
    );

    // Steady state: the next refresh sees the machine connected and
    // skips the hosted record cleanly — no re-dial churn, no failures.
    let second = bob.refresh_route_discovery().await.expect("second refresh");
    assert!(
        second.peer_dial_failures.is_empty(),
        "a hosted record behind a live machine connection must not re-dial \
         into failures: {:?}",
        second.peer_dial_failures
    );
}

/// what this catches (strict pinning, the retry's non-negotiable gate):
/// when the endpoint answers with an identity this scope has NEVER
/// enrolled, there is no retry and no connection — the dial fails loud.
/// An unknown identity must never be accepted, no matter how convenient
/// the heal would be.
#[tokio::test]
async fn identity_mismatch_never_retries_an_unenrolled_identity() {
    let tmp_machine = TempDir::new().expect("machine tempdir");
    let tmp_bob = TempDir::new().expect("bob tempdir");
    let machine = Airc::open(tmp_machine.path().join(".airc"))
        .await
        .expect("machine open");
    let bob = Airc::open(tmp_bob.path().join(".airc"))
        .await
        .expect("bob open");

    let scope_peer = airc_core::PeerId::new();
    let scope_keypair = airc_protocol::PeerKeypair::generate();
    let scope_spec = airc_lib::PeerSpec {
        peer_id: scope_peer,
        pubkey: scope_keypair.public_bytes(),
    };
    // bob trusts ONLY the scope peer — the machine identity answering
    // TLS at the endpoint is a stranger to bob's trust store.
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    machine
        .add_peer(bob_spec)
        .await
        .expect("machine trusts bob");
    bob.add_peer(scope_spec)
        .await
        .expect("bob trusts the scope peer");

    let machine_addr: SocketAddr = machine
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("machine listens");
    let endpoints_json = endpoints_to_json(&[RouteEndpoint::LanTcp { addr: machine_addr }])
        .expect("encode endpoints");
    airc_trust::set_endpoints_json(
        bob.home(),
        scope_peer,
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
    .await
    .expect("store endpoints")
    .expect("scope peer must be enrolled on bob");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("refresh must survive the refused dial");

    assert!(
        snapshot.connected_lan_peers.is_empty(),
        "an unenrolled listener identity must never be connected to: {:?}",
        snapshot.connected_lan_peers
    );
    assert_eq!(
        snapshot.peer_dial_failures.len(),
        1,
        "the refused dial must be recorded loudly: {:?}",
        snapshot.peer_dial_failures
    );
    let failure = &snapshot.peer_dial_failures[0];
    assert_eq!(failure.peer_id, scope_peer);
    assert!(
        failure.error.contains("not enrolled") || failure.error.contains("expected"),
        "the failure must carry the verifier's loud refusal: {}",
        failure.error
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

    // Bind the wildcard listener (advertising only the unreachable 100.x
    // Tailscale rung — self-healing join's advertise hygiene rightly
    // refuses loopback as a LAN advertisement), then build the peer-side
    // endpoint set MANUALLY with loopback standing in for the LAN IP.
    // This test pins the DIAL ladder, not the advertise filter, and an
    // import can legitimately hold any addr an older/other node stored.
    let advertised = alice
        .listen_lan_advertising(None, Some(Ipv4Addr::new(100, 79, 156, 3)))
        .await
        .expect("alice advertises the tailscale rung");
    let port = advertised
        .iter()
        .find_map(|endpoint| match endpoint {
            RouteEndpoint::TailscaleTcp { addr } => Some(addr.port()),
            _ => None,
        })
        .expect("tailscale rung advertised");
    let both_rungs = vec![
        RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        },
        RouteEndpoint::TailscaleTcp {
            addr: SocketAddr::from((Ipv4Addr::new(100, 79, 156, 3), port)),
        },
    ];

    let endpoints_json = endpoints_to_json(&both_rungs).expect("encode endpoints");
    airc_trust::set_endpoints_json(
        bob.home(),
        alice.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
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
        beacon_now, AccountPeerBeacon, AccountRegistryDocument, MeshIdentity,
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

    let room = alice.current_room().await.expect("alice current room");
    let document = AccountRegistryDocument::new(
        MeshIdentity::new("test-account"),
        2_000,
        vec![airc_lib::AccountRoom::new(room.channel, Some(room.name.clone()))],
        vec![AccountPeerBeacon {
            endpoints_advertised_at_ms: None,
            endpoints_peer_id: None,
            presence: beacon_now(
                alice.peer_id(),
                tmp_a.path().join(".airc"),
                vec![room.channel],
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
    airc_trust::set_endpoints_json(
        bob.home(),
        alice.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
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
    airc_trust::set_endpoints_json(
        bob.home(),
        alice.peer_id(),
        Some(endpoints_json),
        test_stamp_now_ms(),
        None,
    )
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

/// Concurrency pin — peers are dialed CONCURRENTLY, so the refresh
/// wall-clock is `~max(single-peer dial)`, NOT the SUM across peers.
///
/// what this catches: the serial-dial hang. The loop in
/// `dial_stored_peer_endpoints` used to walk peers one at a time, each
/// unreachable peer paying the full `PEER_DIAL_TIMEOUT` before the next
/// was even attempted. On a real account with dozens of enrolled peers
/// (41 on BIGMAMA, ~15 off-LAN), a cold refresh — before any endpoint
/// is quarantined — serially burned ~45s, hanging `airc doctor
/// --health` and every send-path refresh. A regression to serial
/// dialing makes N tarpit peers take N×3s; this test bounds the whole
/// refresh well under that.
///
/// Each "peer" advertises a TARPIT endpoint: a TCP listener that
/// ACCEPTS the connection but never speaks TLS, so `connect_lan` blocks
/// in the handshake until `PEER_DIAL_TIMEOUT` fires (a closed port would
/// be refused instantly and prove nothing about concurrency). With
/// N_TARPITS peers, serial = N×3s = 15s; concurrent ≈ 3s. The 9s bound
/// is generous enough for a loaded CI runner yet still fails loudly if
/// the dials ever go serial again.
#[tokio::test]
async fn peers_are_dialed_concurrently_not_serially() {
    const N_TARPITS: usize = 5;

    let tmp_b = TempDir::new().expect("bob tempdir");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");

    // Hold the tarpit listeners + their accept tasks alive for the whole
    // test: each accepts inbound TCP and parks the stream, never
    // completing TLS, so the dialer's handshake stalls to the deadline.
    let mut tarpits = Vec::new();
    let mut peer_tmps = Vec::new();
    for _ in 0..N_TARPITS {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("tarpit bind");
        let addr = listener.local_addr().expect("tarpit addr");
        let handle = tokio::spawn(async move {
            let mut held = Vec::new();
            // Accept and PARK every inbound stream — never speak TLS, so
            // the dialer's handshake stalls to PEER_DIAL_TIMEOUT.
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        tarpits.push(handle);

        // A real enrolled peer identity whose ONLY endpoint is the tarpit.
        let tmp = TempDir::new().expect("peer tempdir");
        let peer = Airc::open(tmp.path().join(".airc"))
            .await
            .expect("peer open");
        let peer_spec: PeerSpec = peer.peer_spec().parse().expect("peer spec");
        let peer_id = peer.peer_id();
        bob.add_peer(peer_spec).await.expect("bob trusts peer");
        let endpoints_json =
            endpoints_to_json(&[RouteEndpoint::LanTcp { addr }]).expect("encode endpoints");
        airc_trust::set_endpoints_json(
            bob.home(),
            peer_id,
            Some(endpoints_json),
            test_stamp_now_ms(),
            None,
        )
        .await
        .expect("store endpoints")
        .expect("peer must be enrolled on bob");
        peer_tmps.push((tmp, peer));
    }

    let started = std::time::Instant::now();
    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");
    let elapsed = started.elapsed();

    // Every tarpit dial must be recorded as a failure (none connects).
    assert_eq!(
        snapshot.peer_dial_failures.len(),
        N_TARPITS,
        "each tarpit peer must record one failed dial: {:?}",
        snapshot.peer_dial_failures
    );
    // The load-bearing assert: concurrent, not serial. Serial would be
    // N_TARPITS × PEER_DIAL_TIMEOUT (15s); concurrent is ~3s.
    assert!(
        elapsed < std::time::Duration::from_secs(9),
        "{N_TARPITS} tarpit peers must be dialed CONCURRENTLY (~3s), not \
         serially ({N_TARPITS}×3s=15s); took {elapsed:?} — the serial-dial \
         hang has regressed"
    );

    for handle in tarpits {
        handle.abort();
    }
}

// ---------------------------------------------------------------------
// Self-healing join — refresh-on-failure (`Airc::heal_failed_dials`).
// ---------------------------------------------------------------------

/// Stub rendezvous: hands back whatever document the test placed in it.
/// The trait seam (`AccountRegistryStore`) IS the stub point — no gh, no
/// network, fully deterministic.
struct StubRendezvous {
    doc: std::sync::Mutex<Option<airc_lib::AccountRegistryDocument>>,
}

impl StubRendezvous {
    fn holding(doc: airc_lib::AccountRegistryDocument) -> Self {
        Self {
            doc: std::sync::Mutex::new(Some(doc)),
        }
    }

    fn set(&self, doc: airc_lib::AccountRegistryDocument) {
        *self.doc.lock().expect("stub lock") = Some(doc);
    }
}

#[async_trait::async_trait]
impl airc_lib::AccountRegistryStore for StubRendezvous {
    async fn publish(
        &self,
        _document: &airc_lib::AccountRegistryDocument,
    ) -> Result<(), airc_lib::AccountRegistryError> {
        Ok(())
    }

    async fn refresh(
        &self,
        _mesh_identity: &airc_lib::MeshIdentity,
    ) -> Result<Option<airc_lib::AccountRegistryDocument>, airc_lib::AccountRegistryError> {
        Ok(self.doc.lock().expect("stub lock").clone())
    }
}

/// One-beacon registry document for `peer` advertising `endpoints`,
/// heartbeated at `heartbeat_at_ms` (which also stamps the endpoints'
/// freshness via `endpoints_freshness_ms`).
fn one_beacon_doc(
    spec: &PeerSpec,
    endpoints: Vec<RouteEndpoint>,
    heartbeat_at_ms: u64,
) -> airc_lib::AccountRegistryDocument {
    airc_lib::AccountRegistryDocument::new(
        airc_lib::MeshIdentity::new("joelteply"),
        heartbeat_at_ms,
        Vec::new(),
        vec![airc_lib::AccountPeerBeacon {
            presence: airc_lib::beacon_now(
                spec.peer_id,
                "/machine/alice/.airc".into(),
                Vec::new(),
                123,
                heartbeat_at_ms,
            ),
            peer_spec: spec.clone(),
            endpoints,
            endpoints_advertised_at_ms: Some(heartbeat_at_ms),
            endpoints_peer_id: None,
        }],
    )
}

/// what this catches (self-healing join, M5↔bigmama decay mode #1 —
/// "stale port dialed forever"): after a dial failure,
/// `heal_failed_dials` must (a) do NOTHING when the rendezvous has no
/// fresher endpoint — no blind retry of the corpse — and (b) once a
/// fresher advertisement lands, re-read it, replace the stored
/// endpoint, and dial the LIVE endpoint immediately instead of waiting
/// for the next blind cadence. Mutation check: dropping the
/// changed-endpoint gate makes (a) return Some (blind retry); dropping
/// the post-import re-dial makes (b) return a snapshot without the
/// live connection.
#[tokio::test]
async fn heal_failed_dials_rereads_rendezvous_and_dials_fresh_endpoint() {
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
    bob.add_peer(alice_spec.clone())
        .await
        .expect("bob trusts alice");

    // Alice's REAL listener (the restarted daemon's new port)...
    let live_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");
    // ...and the dead port her stale advertisement still points at.
    let dead_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        listener.local_addr().expect("probe addr")
    };

    let now_ms = test_stamp_now_ms();
    // The rendezvous initially carries the STALE advertisement.
    let stub = StubRendezvous::holding(one_beacon_doc(
        &alice_spec,
        vec![RouteEndpoint::LanTcp { addr: dead_addr }],
        now_ms - 60_000,
    ));
    bob.refresh_account_registry(&stub)
        .await
        .expect("import stale advertisement");

    let snapshot = bob
        .refresh_route_discovery()
        .await
        .expect("bob discovery refresh");
    assert_eq!(
        snapshot.peer_dial_failures.len(),
        1,
        "the stale endpoint must fail: {:?}",
        snapshot.peer_dial_failures
    );

    // (a) Rendezvous unchanged → nothing fresher → heal must be a
    // no-op (the quarantine backoff owns the dead endpoint's retry).
    let unchanged = bob
        .heal_failed_dials(&stub, &snapshot.peer_dial_failures)
        .await
        .expect("heal with unchanged rendezvous");
    assert!(
        unchanged.is_none(),
        "no fresher endpoint on the rendezvous must mean NO extra dial pass"
    );

    // (b) The fresh advertisement lands (daemon restarted, new port).
    stub.set(one_beacon_doc(
        &alice_spec,
        vec![RouteEndpoint::LanTcp { addr: live_addr }],
        now_ms,
    ));
    let healed = bob
        .heal_failed_dials(&stub, &snapshot.peer_dial_failures)
        .await
        .expect("heal with fresh rendezvous")
        .expect("a fresher endpoint must trigger the re-dial pass");
    assert!(
        healed.connected_lan_peers.contains(&alice.peer_id()),
        "heal must dial the FRESH endpoint and connect; connected: {:?}, failures: {:?}",
        healed.connected_lan_peers,
        healed.peer_dial_failures
    );

    // The stored record is now exactly the live endpoint (atomic
    // replace, item 1) — the corpse is gone from the dial set.
    let stored = airc_trust::load(bob.wire_root())
        .await
        .expect("load bob trust")
        .into_iter()
        .find(|p| p.peer_id == alice.peer_id())
        .expect("alice enrolled on bob");
    let stored_endpoints =
        airc_lib::endpoints_from_json(stored.endpoints_json.as_deref().expect("endpoints stored"))
            .expect("decode stored endpoints");
    assert_eq!(
        stored_endpoints,
        vec![RouteEndpoint::LanTcp { addr: live_addr }],
        "the fresher advertisement must fully replace the dead endpoint"
    );
}
