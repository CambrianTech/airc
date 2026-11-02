use super::*;
use crate::addressed::{AddressedFrameSink, AddressedKind, SessionUnavailableReason};
use airc_core::{Body, Headers, PeerId};
use airc_protocol::addressed::AddressedWire;
use std::time::Duration;
use tokio::sync::mpsc;

struct Capture(mpsc::Sender<Frame>);

#[async_trait::async_trait]
impl AddressedFrameSink for Capture {
    async fn deliver(&self, frame: Frame) -> Result<(), AddressedError> {
        self.0
            .send(frame)
            .await
            .map_err(|error| AddressedError::Transport(error.to_string()))
    }
}

#[tokio::test]
async fn losing_first_init_recovers_original_requests_without_other_traffic() {
    // Real TLS transport and endpoint crypto; only the first init is discarded.
    // First prove one original request recovers with no other application send.
    // Then exercise the same recovery with concurrent waiters.
    for request_count in [1, 8] {
        tokio::time::timeout(Duration::from_secs(15), async {
            let alice_home = tempfile::TempDir::new().unwrap();
            let bob_home = tempfile::TempDir::new().unwrap();
            let alice = Airc::open(alice_home.path()).await.unwrap();
            let bob = Airc::open(bob_home.path()).await.unwrap();
            alice
                .add_peer(bob.peer_spec().parse().unwrap())
                .await
                .unwrap();
            bob.add_peer(alice.peer_spec().parse().unwrap())
                .await
                .unwrap();
            alice.join("lost-init").await.unwrap();
            bob.join("lost-init").await.unwrap();
            let (tx, mut frames) = mpsc::channel(32);
            bob.set_addressed_frame_sink(Arc::new(Capture(tx))).unwrap();
            let address = bob
                .listen_lan("127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
            alice.connect_lan(address, bob.peer_id()).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut inbox = bob.addressed_inbox(deadline).await.unwrap();
            let expected: Vec<_> = (0..request_count).map(|_| uuid::Uuid::new_v4()).collect();
            let sends = async {
                let pending = expected.iter().map(|correlation| {
                    alice.send_addressed(
                        AddressedSend {
                            to: bob.peer_id(),
                            kind: AddressedKind::Command,
                            correlation: *correlation,
                            headers: Headers::new(),
                            body: Body::text("same original request"),
                        },
                        deadline,
                    )
                });
                for result in futures::future::join_all(pending).await {
                    result.unwrap();
                }
            };
            let delivery = async {
                let mut first_init = None;
                let mut init_count = 0;
                let mut sealed_count = 0;
                while sealed_count != expected.len() {
                    let frame = frames.recv().await.unwrap();
                    match AddressedWire::from_body(frame.envelope.body.as_ref().unwrap()).unwrap() {
                        AddressedWire::Init(_) => {
                            init_count += 1;
                            if first_init.is_none() {
                                first_init = frame.envelope.body.clone();
                                continue; // Simulate loss after a successful transport write.
                            }
                            assert_eq!(
                                frame.envelope.body, first_init,
                                "retry must reuse the init"
                            );
                        }
                        AddressedWire::Sealed(_) => sealed_count += 1,
                        AddressedWire::Resp(_) => panic!("unexpected responder traffic"),
                    }
                    bob.receive_addressed(frame).await;
                }
                // Slow CI scheduling can legitimately postpone a response past
                // another retry tick. Exclusive ownership is checked separately.
                assert!(init_count >= 2);
            };
            tokio::join!(sends, delivery);
            let mut received = std::collections::HashSet::new();
            for _ in &expected {
                let message = inbox.next().await.unwrap();
                assert_eq!(message.from(), alice.peer_id());
                assert_eq!(message.body().as_text(), Some("same original request"));
                assert!(received.insert(message.correlation()));
            }
            assert!(expected.iter().all(|id| received.contains(id)));
        })
        .await
        .expect("original request deadline must bound recovery");
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_owner_hands_off_same_retry_schedule_and_dead_peers_are_pruned() {
    let waiters = HandshakeWaiters::default();
    let peer = PeerId::new();
    let first = waiters.lease(peer).await;
    let follower = waiters.lease(peer).await;
    assert!(Arc::ptr_eq(&first, &follower));
    let mut owner = first.lock().await;
    let next = Instant::now() + Duration::from_millis(500);
    *owner = next;
    assert!(follower.try_lock().is_err());
    drop(owner); // Dropping a cancelled request releases its guard.
    drop(first);
    assert_eq!(*follower.lock().await, next);
    drop(follower);
    let _next_peer = waiters.lease(PeerId::new()).await;
    let peers = waiters.peers.lock().await;
    assert_eq!(peers.len(), 1);
    assert!(!peers.contains_key(&peer));
}

// what this catches: the IntelMac 2026-11-02 shape at the wire. The route is healthy but the peer
// forms no session: its frame sink takes every init and answers none. A send with a ten-minute
// deadline gives up at the route's delivery budget as pre-dispatch NoHandshakeReply, and the
// peer's wire sink saw inits only, never a sealed request, so the caller may take the work
// elsewhere without running it twice (Codex on #1649: prove it at the sink, not the store).
#[tokio::test]
async fn a_peer_that_answers_no_handshake_is_given_up_at_the_budget_and_nothing_sealed_leaves() {
    let alice_home = tempfile::TempDir::new().unwrap();
    let bob_home = tempfile::TempDir::new().unwrap();
    let alice = Airc::open(alice_home.path()).await.unwrap();
    let bob = Airc::open(bob_home.path()).await.unwrap();
    alice
        .add_peer(bob.peer_spec().parse().unwrap())
        .await
        .unwrap();
    bob.add_peer(alice.peer_spec().parse().unwrap())
        .await
        .unwrap();
    alice.join("silent-seat").await.unwrap();
    bob.join("silent-seat").await.unwrap();
    let (tx, mut frames) = mpsc::channel(256);
    bob.set_addressed_frame_sink(Arc::new(Capture(tx))).unwrap();
    let address = bob
        .listen_lan("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    alice.connect_lan(address, bob.peer_id()).await.unwrap();
    let budget = crate::route_forwarder::RoutedForwarderConfig::default().delivery_budget();
    let started = Instant::now();
    let sent = alice
        .send_addressed(
            AddressedSend {
                to: bob.peer_id(),
                kind: AddressedKind::Command,
                correlation: uuid::Uuid::new_v4(),
                headers: Headers::new(),
                body: Body::text("a request the seat must never see"),
            },
            Instant::now() + Duration::from_secs(600),
        )
        .await;
    let waited = started.elapsed();
    assert!(
        matches!(
            sent,
            Err(AddressedError::SessionUnavailable {
                reason: SessionUnavailableReason::NoHandshakeReply,
                ..
            })
        ),
        "a routable peer that forms no session is pre-dispatch NoHandshakeReply: {sent:?}"
    );
    assert!(
        waited < budget + Duration::from_secs(5),
        "given up at the route budget ({budget:?}), not the caller's 600 s: {waited:?}"
    );
    let (mut inits, mut sealed) = (0, 0);
    while let Ok(frame) = frames.try_recv() {
        match AddressedWire::from_body(frame.envelope.body.as_ref().unwrap()).unwrap() {
            AddressedWire::Init(_) => inits += 1,
            AddressedWire::Sealed(_) => sealed += 1,
            AddressedWire::Resp(_) => {}
        }
    }
    assert!(inits >= 1, "the handshake was attempted on the live route");
    assert_eq!(
        sealed, 0,
        "nothing sealed left: the request was never dispatched"
    );
}

// what this catches: the bound of the pre-dispatch phases. The route's delivery budget bounds
// them when it is the earlier, typed NoHandshakeReply; the caller's deadline when it is,
// typed CallerDeadline. Never the caller's deadline alone (the 630 s wait).
#[test]
fn the_pre_dispatch_bound_is_the_earlier_of_the_route_budget_and_the_caller() {
    let now = Instant::now();
    let budget = crate::route_forwarder::RoutedForwarderConfig::default().delivery_budget();
    let (bound, reason) = pre_dispatch_bound(now, now + Duration::from_secs(600));
    assert_eq!(
        (bound, reason),
        (now + budget, SessionUnavailableReason::NoHandshakeReply)
    );
    let short = now + Duration::from_millis(300);
    assert_eq!(
        pre_dispatch_bound(now, short),
        (short, SessionUnavailableReason::CallerDeadline)
    );
}

// what this catches: relabeling a failed init as an absent route (Codex on #1649). Only the route
// resolver's typed refusal is NoRoute and retried; a codec, signing, clock or IPC failure returns
// at once, typed; the bound cutting a write short ends on the session wait's own timer.
#[test]
fn only_a_typed_absent_route_is_retried_as_no_route() {
    assert_eq!(InitOutcome::of(Ok(())).unwrap(), InitOutcome::Sent);
    assert_eq!(
        InitOutcome::of(Err(AddressedError::NoLiveRoute("none".into()))).unwrap(),
        InitOutcome::NoRoute
    );
    assert_eq!(
        InitOutcome::of(Err(AddressedError::DeadlineExceeded)).unwrap(),
        InitOutcome::Sent
    );
    assert!(matches!(
        InitOutcome::of(Err(AddressedError::Codec("bad".into()))),
        Err(AddressedError::Codec(_))
    ));
    assert!(matches!(
        InitOutcome::of(Err(AddressedError::Transport("ipc".into()))),
        Err(AddressedError::Transport(_))
    ));
    use SessionUnavailableReason::*;
    assert_eq!(
        InitOutcome::NoRoute.give_up_reason(NoHandshakeReply),
        NoRoute
    );
    assert_eq!(
        InitOutcome::Sent.give_up_reason(NoHandshakeReply),
        NoHandshakeReply
    );
    assert_eq!(
        InitOutcome::NoRoute.give_up_reason(CallerDeadline),
        CallerDeadline
    );
}

fn probe_send(to: PeerId) -> AddressedSend {
    AddressedSend {
        to,
        kind: AddressedKind::Command,
        correlation: uuid::Uuid::new_v4(),
        headers: Headers::new(),
        body: Body::text("must not leave"),
    }
}

// what this catches: an already-expired send reported as a post-seal DeadlineExceeded (delivery
// unknown) when nothing was attempted. It is refused pre-dispatch as CallerDeadline, and the
// peer's wire sink sees nothing at all (Codex on #1649).
#[tokio::test]
async fn an_already_expired_send_is_refused_pre_dispatch_and_nothing_leaves() {
    let alice_home = tempfile::TempDir::new().unwrap();
    let bob_home = tempfile::TempDir::new().unwrap();
    let alice = Airc::open(alice_home.path()).await.unwrap();
    let bob = Airc::open(bob_home.path()).await.unwrap();
    alice
        .add_peer(bob.peer_spec().parse().unwrap())
        .await
        .unwrap();
    bob.add_peer(alice.peer_spec().parse().unwrap())
        .await
        .unwrap();
    alice.join("expired-send").await.unwrap();
    bob.join("expired-send").await.unwrap();
    let (tx, mut frames) = mpsc::channel(8);
    bob.set_addressed_frame_sink(Arc::new(Capture(tx))).unwrap();
    let address = bob
        .listen_lan("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    alice.connect_lan(address, bob.peer_id()).await.unwrap();
    let sent = alice
        .send_addressed(probe_send(bob.peer_id()), Instant::now())
        .await;
    assert!(
        matches!(
            sent,
            Err(AddressedError::SessionUnavailable {
                reason: SessionUnavailableReason::CallerDeadline,
                ..
            })
        ),
        "an expired send is pre-dispatch CallerDeadline: {sent:?}"
    );
    assert!(
        frames.try_recv().is_err(),
        "nothing reached the peer's wire"
    );
}

// what this catches: a daemon-owner attach that cannot complete keeping the caller's whole
// deadline (the 630 s failure one layer down) or reading as delivery-unknown. With the owner slot
// held, a send under a ten-minute deadline gives up at the route budget as pre-dispatch
// OwnerUnavailable. Time is paused after setup: the held lock leaves the runtime idle, so the
// budget elapses at once (Codex on #1649: hold the existing owner mutex, daemon-client mode).
#[tokio::test]
async fn a_blocked_owner_attach_is_refused_pre_dispatch_as_owner_unavailable() {
    let home = tempfile::TempDir::new().unwrap();
    let airc =
        Airc::open(home.path())
            .await
            .unwrap()
            .with_daemon_client(airc_ipc::DaemonClient::new(
                home.path().join("no-daemon.sock"),
            ));
    let held = airc.inner.addressed.daemon_owner.lock().await;
    tokio::time::pause();
    let sent = airc
        .send_addressed(
            probe_send(PeerId::new()),
            Instant::now() + Duration::from_secs(600),
        )
        .await;
    drop(held);
    assert!(
        matches!(
            sent,
            Err(AddressedError::SessionUnavailable {
                reason: SessionUnavailableReason::OwnerUnavailable,
                ..
            })
        ),
        "a blocked owner attach is pre-dispatch OwnerUnavailable at the route budget: {sent:?}"
    );
}
