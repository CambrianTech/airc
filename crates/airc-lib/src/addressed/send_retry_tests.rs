use super::*;
use crate::addressed::{AddressedFrameSink, AddressedKind};
use airc_core::{Body, Headers, PeerId};
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
