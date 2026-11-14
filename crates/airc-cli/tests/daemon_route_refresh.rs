//! Card 625abe6d slice 2 — the DAEMON, not the operator, keeps routes
//! alive.
//!
//! Slice 1 proved `refresh_route_discovery` dials stored peer
//! endpoints outbound — when something calls it. This test pins the
//! slice-2 contract: a freshly started daemon whose trust store
//! carries a stored peer endpoint establishes the LAN connection on
//! its own clock, WITHOUT anyone running `airc transport health` (or
//! any other CLI verb). That is the sleep/wake + restart requirement:
//! bring the daemon up, routes come back, zero operator action.
//!
//! Hazard notes (card d2ba719c): daemon-class tests can hang — every
//! wait here is bounded, and the spawned daemon is killed on drop.

use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use airc_lib::{endpoints_to_json, Airc, PeerSpec, RouteEndpoint};
mod common;

fn airc_bin() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// Kill the daemon process on every exit path (success, assert
/// failure, timeout panic) so a wedged daemon can't outlive the test.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn the real `airc daemon` for `home`. One retry for the
/// Windows Smart App Control race on freshly built binaries
/// (os error 4551) — environmental, not a product failure.
fn spawn_daemon(home: &std::path::Path, socket: &std::path::Path) -> DaemonGuard {
    let log_path = home.join("daemon-test.log");
    let mut attempt = 0;
    loop {
        attempt += 1;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("daemon log file");
        let stderr = log.try_clone().expect("clone log handle");
        let spawned = Command::new(airc_bin())
            .arg("--home")
            .arg(home)
            .arg("daemon")
            .arg("--socket")
            .arg(socket)
            // Hermetic gate (card d793c242): test daemons must never
            // touch the operator's real gh account rendezvous. The
            // temp-rooted home blocks it too — this is the intentional
            // layer.
            .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn();
        match spawned {
            Ok(child) => return DaemonGuard { child },
            Err(error) if attempt == 1 => {
                // Smart App Control may block a just-linked binary
                // once; retry after a beat.
                eprintln!("daemon spawn attempt 1 failed ({error}); retrying once");
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(error) => panic!("daemon must spawn: {error}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_dials_stored_endpoint_without_transport_health() {
    // Outer bound: this entire test must finish or fail loudly —
    // never hang (card d2ba719c).
    tokio::time::timeout(Duration::from_secs(120), scenario(false))
        .await
        .expect("daemon route-refresh scenario must finish inside 120s");
}

#[tokio::test(flavor = "multi_thread")]
async fn addressed_remote_scope_recovers_after_its_daemon_process_restarts() {
    tokio::time::timeout(Duration::from_secs(120), scenario(true))
        .await
        .expect("addressed process restart scenario deadline");
}

async fn scenario(restart_addressed: bool) {
    let tmp_alice = common::daemon_tempdir();
    let tmp_bob = common::daemon_tempdir();
    let alice_home = tmp_alice.path().join(".airc");
    let bob_home = tmp_bob.path().join(".airc");

    // Alice: an in-process listener — the peer bob's daemon must reach.
    let alice = Airc::open(&alice_home).await.expect("alice open");
    let alice_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");

    // Bob: enrol mutual trust + store alice's endpoint on bob's trust
    // record, exactly what the account-registry import produces. The
    // in-process handle is then DROPPED — from here on, the only
    // thing that can dial is bob's daemon process.
    let bob_peer_id = {
        let bob = Airc::open(&bob_home).await.expect("bob open");
        let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice spec");
        let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
        alice.add_peer(bob_spec).await.expect("alice trusts bob");
        bob.add_peer(alice_spec).await.expect("bob trusts alice");

        let endpoints_json = endpoints_to_json(&[RouteEndpoint::LanTcp { addr: alice_addr }])
            .expect("encode endpoints");
        let advertised_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis() as u64;
        airc_trust::set_endpoints_json(
            bob.home(),
            alice.peer_id(),
            Some(endpoints_json),
            advertised_at_ms,
            None,
        )
        .await
        .expect("store endpoints")
        .expect("alice must be enrolled on bob");
        bob.peer_id()
    };

    // Bob's daemon comes up. Nothing else runs against it — no
    // `transport health`, no `doctor`, no IPC client at all.
    let socket = bob_home.join("daemon.sock");
    let daemon = spawn_daemon(&bob_home, &socket);

    // The daemon's first refresh fires FIRST_REFRESH_DELAY (5s) after
    // start; give it generous-but-bounded room. Observe from alice's
    // side: her listener registers the inbound TLS connection keyed by
    // bob's peer id. `route_discovery_snapshot` reads state without
    // dialing, so alice can't be the one establishing the link.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = alice
            .route_discovery_snapshot()
            .await
            .expect("alice snapshot");
        if snapshot.connected_lan_peers.contains(&bob_peer_id) {
            break;
        }
        if Instant::now() >= deadline {
            let log = std::fs::read_to_string(bob_home.join("daemon-test.log"))
                .unwrap_or_else(|error| format!("<no daemon log: {error}>"));
            panic!(
                "bob's daemon never dialed alice's stored endpoint on its own \
                 clock; connected: {:?}; daemon log:\n{log}",
                snapshot.connected_lan_peers
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if restart_addressed {
        use airc_lib::addressed::InboxError;
        // Under custody (card 32f7dd97) attach never mints: the named scope is initialised first.
        Airc::initialize_as(&bob_home, "restart-bob")
            .await
            .expect("initialise the surviving Bob scope");
        let bob = Airc::attach_as(&bob_home, "restart-bob", socket.clone())
            .await
            .expect("attach surviving Bob scope");
        alice
            .add_peer(bob.peer_spec().parse().expect("Bob scope spec"))
            .await
            .expect("Alice enrolls Bob scope");
        bob.add_peer(alice.peer_spec().parse().expect("Alice spec"))
            .await
            .expect("Bob enrolls Alice");
        airc_trust::set_endpoints_json(alice.home(), bob.peer_id(), None, 1, Some(bob_peer_id))
            .await
            .expect("route Bob scope through its host")
            .expect("Bob enrolled");
        let deadline = || tokio::time::Instant::now() + Duration::from_secs(15);
        let mut alice_inbox = alice
            .addressed_inbox(deadline())
            .await
            .expect("Alice inbox");
        let mut bob_inbox = bob.addressed_inbox(deadline()).await.expect("Bob inbox");
        exchange(&alice, &bob, &mut bob_inbox, "before crash").await;
        exchange(&bob, &alice, &mut alice_inbox, "before crash reply").await;

        // Terminate and reap the real daemon process, including its TLS and
        // IPC sockets. Both SDK objects, Alice's inbox and crypto state survive.
        drop(daemon);
        assert_eq!(
            tokio::time::timeout_at(deadline(), bob_inbox.next())
                .await
                .expect("Bob must observe dead daemon")
                .err(),
            Some(InboxError::Closed)
        );
        drop(bob_inbox);
        tokio::time::timeout_at(deadline(), async {
            loop {
                if !alice
                    .route_discovery_snapshot()
                    .await
                    .expect("observe old TLS connection closure")
                    .connected_lan_peers
                    .contains(&bob_peer_id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("old daemon TLS session must close before restart");
        let _restarted = spawn_daemon(&bob_home, &socket);
        let client = airc_ipc::DaemonClient::new(socket);
        tokio::time::timeout_at(deadline(), async {
            loop {
                if let Ok(status) = client.status().await {
                    assert_eq!(status.peer_id, bob_peer_id.to_string());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("restarted daemon readiness");
        let mut bob_inbox = bob
            .addressed_inbox(deadline())
            .await
            .expect("Bob reacquires IPC owner");
        // Let the daemon's existing discovery owner re-establish TLS. Do not
        // call transport health, connect_lan, or a crypto-session reset hook.
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if alice
                    .route_discovery_snapshot()
                    .await
                    .expect("route observation")
                    .connected_lan_peers
                    .contains(&bob_peer_id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("daemon reconnects stored endpoint");
        exchange(&bob, &alice, &mut alice_inbox, "after crash").await;
        exchange(&alice, &bob, &mut bob_inbox, "after crash reply").await;
    }
}

async fn exchange(
    sender: &Airc,
    recipient: &Airc,
    inbox: &mut airc_lib::addressed::AddressedInbox,
    body: &str,
) {
    use airc_lib::addressed::{AddressedKind, AddressedSend};
    let correlation = uuid::Uuid::new_v4();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    sender
        .send_addressed(
            AddressedSend {
                to: recipient.peer_id(),
                kind: AddressedKind::Command,
                correlation,
                headers: Default::default(),
                body: airc_core::Body::text(body),
            },
            deadline,
        )
        .await
        .expect("addressed send");
    let received = tokio::time::timeout_at(deadline, inbox.next())
        .await
        .expect("addressed receive deadline")
        .expect("addressed receive");
    assert_eq!(received.correlation(), correlation);
    assert_eq!(received.from(), sender.peer_id());
    assert_eq!(received.to(), recipient.peer_id());
    assert_eq!(received.body().as_text(), Some(body));
}
