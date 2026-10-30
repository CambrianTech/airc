//! Addressed delivery across real machines: a command and its result reach
//! their two ends, and no store anywhere holds either.
//!
//! Three machine homes on loopback LAN. Every link carries everything (the
//! LAN adapter writes a frame to each connection), so carol, the third party,
//! receives the bytes of alice's request to bob and of bob's result to alice.
//! She must store and surface neither; alice and bob must not store them
//! either. Regression for the continuum incident of 2026-10-30: peer inference
//! requests travelled as plaintext room messages and landed in every member's
//! transcript.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use airc_core::{Body, Headers, TranscriptEvent};
use airc_lib::addressed::{AddressedKind, AddressedSend};
use airc_lib::Airc;
use airc_protocol::addressed::addressed_channel;
use airc_store::{EventStore, SqliteEventStore};
use tempfile::TempDir;
use tokio::time::{timeout, Instant};
use uuid::Uuid;

const REQUEST_SECRET: &str = "resident prompt: the request only bob may read";
const RESULT_SECRET: &str = "the result only alice may read";

async fn stored(home: &Path) -> Vec<TranscriptEvent> {
    let store = SqliteEventStore::open_path(&home.join("events.sqlite"))
        .await
        .expect("open store");
    store.page_recent(None, 100_000).await.expect("read store")
}

fn texts(events: &[TranscriptEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| serde_json::to_string(event).expect("event serializes"))
        .collect()
}

#[test]
fn an_addressed_request_and_its_result_reach_only_their_two_ends() {
    // what this catches: request or result readable by, stored by, or surfaced
    // to a third party on the links; or stored by either end.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let homes: Vec<TempDir> = (0..3).map(|_| TempDir::new().expect("home")).collect();
        let alice = Airc::open(homes[0].path()).await.expect("alice opens");
        let bob = Airc::open(homes[1].path()).await.expect("bob opens");
        let carol = Airc::open(homes[2].path()).await.expect("carol opens");
        let all = [&alice, &bob, &carol];
        for a in all {
            for b in all {
                if a.peer_id() != b.peer_id() {
                    a.add_peer(b.peer_spec().parse().expect("spec"))
                        .await
                        .expect("trust");
                }
            }
            a.join("addressed-delivery").await.expect("join");
        }
        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let bob_addr = bob.listen_lan(loopback).await.expect("bob listens");
        let carol_addr = carol.listen_lan(loopback).await.expect("carol listens");
        alice
            .connect_lan(bob_addr, bob.peer_id())
            .await
            .expect("alice-bob");
        alice
            .connect_lan(carol_addr, carol.peer_id())
            .await
            .expect("alice-carol");
        bob.connect_lan(carol_addr, carol.peer_id())
            .await
            .expect("bob-carol");

        let mut alice_inbox = alice.addressed_inbox();
        let mut bob_inbox = bob.addressed_inbox();
        let mut carol_inbox = carol.addressed_inbox();
        let correlation = Uuid::new_v4();
        let deadline = Instant::now() + Duration::from_secs(10);

        alice
            .send_addressed(
                AddressedSend {
                    to: bob.peer_id(),
                    kind: AddressedKind::Command,
                    correlation,
                    headers: Headers::new(),
                    body: Body::text(REQUEST_SECRET),
                },
                deadline,
            )
            .await
            .expect("alice sends the request");
        let request = timeout(Duration::from_secs(10), bob_inbox.next())
            .await
            .expect("bob receives in time")
            .expect("bob's inbox yields");
        assert_eq!(request.from(), alice.peer_id());
        assert_eq!(request.to(), bob.peer_id());
        assert_eq!(request.kind(), AddressedKind::Command);
        assert_eq!(request.correlation(), correlation);
        assert_eq!(request.body().as_text(), Some(REQUEST_SECRET));

        bob.send_addressed(
            AddressedSend {
                to: request.from(),
                kind: AddressedKind::CommandResult,
                correlation: request.correlation(),
                headers: Headers::new(),
                body: Body::text(RESULT_SECRET),
            },
            deadline,
        )
        .await
        .expect("bob sends the result");
        let result = timeout(Duration::from_secs(10), alice_inbox.next())
            .await
            .expect("alice receives in time")
            .expect("alice's inbox yields");
        assert_eq!(result.from(), bob.peer_id());
        assert_eq!(result.kind(), AddressedKind::CommandResult);
        assert_eq!(result.correlation(), correlation);
        assert_eq!(result.body().as_text(), Some(RESULT_SECRET));

        // Carol's links carry both senders' traffic: their room messages land
        // in her store, so the absence below is not a disconnected link.
        alice.say("alice-public").await.expect("alice says");
        bob.say("bob-public").await.expect("bob says");
        let seen_by = Instant::now() + Duration::from_secs(10);
        loop {
            let carol_texts = texts(&stored(homes[2].path()).await);
            let has = |needle: &str| carol_texts.iter().any(|t| t.contains(needle));
            if has("alice-public") && has("bob-public") {
                break;
            }
            assert!(
                Instant::now() < seen_by,
                "carol never stored the room messages"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert!(
            timeout(Duration::from_millis(200), carol_inbox.next())
                .await
                .is_err(),
            "the third party's inbox surfaced an addressed payload"
        );
        for home in &homes {
            let events = stored(home.path()).await;
            assert!(
                events
                    .iter()
                    .all(|event| event.room_id != addressed_channel()),
                "an addressed frame was stored in {}",
                home.path().display()
            );
            for text in texts(&events) {
                assert!(!text.contains(REQUEST_SECRET) && !text.contains(RESULT_SECRET));
            }
        }
    });
}

async fn trusted_pair(homes: &[TempDir]) -> (Airc, Airc) {
    let alice = Airc::open(homes[0].path()).await.expect("alice opens");
    let bob = Airc::open(homes[1].path()).await.expect("bob opens");
    alice
        .add_peer(bob.peer_spec().parse().expect("spec"))
        .await
        .expect("trust");
    bob.add_peer(alice.peer_spec().parse().expect("spec"))
        .await
        .expect("trust");
    (alice, bob)
}

fn command(to: airc_core::PeerId, text: &str) -> AddressedSend {
    AddressedSend {
        to,
        kind: AddressedKind::Command,
        correlation: Uuid::new_v4(),
        headers: Headers::new(),
        body: Body::text(text),
    }
}

#[test]
fn a_lost_first_init_ends_in_its_deadline_and_the_next_send_completes() {
    // what this catches: the very first init is lost (no link yet) and no
    // other traffic follows. The send ends in an explicit refusal by its
    // deadline, never a hang or another route; the abandoned handshake does
    // not block the next send, which completes once the link exists.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let homes: Vec<TempDir> = (0..2).map(|_| TempDir::new().expect("home")).collect();
        let (alice, bob) = trusted_pair(&homes).await;
        for _ in 0..2 {
            let sent = alice
                .send_addressed(
                    command(bob.peer_id(), REQUEST_SECRET),
                    Instant::now() + Duration::from_millis(300),
                )
                .await;
            assert!(sent.is_err(), "a send with no session must be refused");
        }
        for text in texts(&stored(homes[0].path()).await) {
            assert!(!text.contains(REQUEST_SECRET));
        }

        let addr = bob
            .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bob listens");
        alice
            .connect_lan(addr, bob.peer_id())
            .await
            .expect("connect");
        let mut inbox = bob.addressed_inbox();
        alice
            .send_addressed(
                command(bob.peer_id(), "once linked"),
                Instant::now() + Duration::from_secs(10),
            )
            .await
            .expect("the next send completes");
        let opened = timeout(Duration::from_secs(10), inbox.next())
            .await
            .expect("bob receives in time")
            .expect("bob's inbox yields");
        assert_eq!(opened.body().as_text(), Some("once linked"));
    });
}

#[test]
fn a_restarted_addressee_rekeys_and_the_next_send_opens() {
    // what this catches: review F2 end to end. Bob loses every session (a
    // restart); alice still seals under the old one. Bob's failure to open it
    // must re-key (no timer, no restart of alice), so alice's retry opens.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let homes: Vec<TempDir> = (0..2).map(|_| TempDir::new().expect("home")).collect();
        let (alice, bob) = trusted_pair(&homes).await;
        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let addr = bob.listen_lan(loopback).await.expect("bob listens");
        alice
            .connect_lan(addr, bob.peer_id())
            .await
            .expect("connect");
        let mut inbox = bob.addressed_inbox();
        let deadline = Instant::now() + Duration::from_secs(10);
        alice
            .send_addressed(command(bob.peer_id(), "before restart"), deadline)
            .await
            .expect("first send");
        timeout(Duration::from_secs(10), inbox.next())
            .await
            .expect("bob receives before restart")
            .expect("bob's inbox yields");

        // Bob loses every session, as a restart does; alice still holds hers.
        bob.forget_addressed_sessions_for_test();
        let bob_id = bob.peer_id();
        let correlation = Uuid::new_v4();

        let opened = timeout(Duration::from_secs(10), async {
            loop {
                let mut retry = command(bob_id, "after restart");
                retry.correlation = correlation;
                alice
                    .send_addressed(retry, Instant::now() + Duration::from_secs(2))
                    .await
                    .expect("send after restart");
                if let Ok(Ok(item)) = timeout(Duration::from_millis(500), inbox.next()).await {
                    return item;
                }
            }
        })
        .await
        .expect("a send after bob's restart opens");
        assert_eq!(opened.from(), alice.peer_id());
        assert_eq!(
            opened.correlation(),
            correlation,
            "a retry keeps its correlation"
        );
        assert_eq!(opened.body().as_text(), Some("after restart"));
    });
}
