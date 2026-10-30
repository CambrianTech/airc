//! Addressed exchanges are independent of conversational room selection.
mod common;

use airc_core::{Body, Headers, MentionTarget};
use airc_lib::AddressedKind;
use common::Machine;
use std::time::Duration;

#[tokio::test]
async fn addressed_request_survives_default_room_changes() {
    let machine = Machine::boot().await;
    machine.pin_identity("command-activity-tests").await;
    let (alice, bob) = machine.pair_in("team-benchmark").await;
    alice.join("alice-activity").await.unwrap();
    bob.join("bob-activity").await.unwrap();
    let recipient = bob.peer_id();
    let mut inbox = bob
        .addressed_inbox(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let responder = tokio::spawn(async move {
        let event = tokio::time::timeout(Duration::from_secs(5), inbox.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.kind(), AddressedKind::Command);
        assert_eq!(event.to(), bob.peer_id());
        bob.reply(&event, Headers::new(), Body::text("graded"))
            .await
            .unwrap();
    });
    let pending = alice
        .request(
            MentionTarget::Peer(recipient),
            Headers::new(),
            Body::text("grade candidate"),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    alice.join("third-activity").await.unwrap();
    let reply = alice.await_reply(pending).await.unwrap();
    assert_eq!(reply.from(), recipient);
    assert_eq!(reply.to(), alice.peer_id());
    assert_eq!(reply.body(), &Body::text("graded"));
    assert_eq!(alice.current_room().await.unwrap().name, "third-activity");
    responder.await.unwrap();
}

#[tokio::test]
async fn command_rejects_broadcast_and_room_without_switching_rooms() {
    let machine = Machine::boot().await;
    machine.pin_identity("command-activity-tests").await;
    let alice = machine.solo("home").await;
    for target in [
        MentionTarget::All,
        MentionTarget::Room(airc_core::RoomId::new()),
    ] {
        assert!(alice
            .request(
                target,
                Headers::new(),
                Body::text("must refuse"),
                Duration::from_secs(1)
            )
            .await
            .is_err());
    }
    assert_eq!(alice.current_room().await.unwrap().name, "home");
}
