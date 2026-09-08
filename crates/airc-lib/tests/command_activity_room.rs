//! Activity requests retain their dispatch room despite other active rooms.
mod common;

use airc_core::{Body, Headers, MentionTarget};
use airc_lib::command_bus::reply_addressing;
use airc_lib::EventFilter;
use common::Machine;
use futures::StreamExt;
use std::time::Duration;

#[tokio::test]
async fn activity_request_receives_reply_when_both_default_rooms_differ() {
    let machine = Machine::boot().await;
    machine.pin_identity("command-activity-tests").await;
    let (alice, bob) = machine.pair_in("team-benchmark").await;
    let activity = alice.current_room().await.unwrap();
    let other = alice.join("other-activity").await.unwrap();
    bob.join("other-activity").await.unwrap();
    let mut stream = bob
        .subscribe_subscribed_filtered(EventFilter::default())
        .await
        .unwrap();
    let responder = tokio::spawn(async move {
        while let Some(Ok(event)) = stream.next().await {
            let Some((peer, correlation)) = reply_addressing(&event) else {
                continue;
            };
            if event.peer_id == bob.peer_id() {
                continue;
            }
            // Same correlation in a different room is not this command's reply.
            bob.reply_in(
                other.channel,
                Some(&other.name),
                peer,
                correlation,
                Headers::new(),
                Body::text("wrong-room"),
            )
            .await
            .unwrap();
            bob.reply_in(
                event.room_id,
                Some("team-benchmark"),
                peer,
                correlation,
                Headers::new(),
                Body::text("graded"),
            )
            .await
            .unwrap();
            return;
        }
    });
    let pending = alice
        .request_in(
            &activity,
            MentionTarget::All,
            Headers::new(),
            Body::text("grade candidate"),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    // A switch after dispatch must not redirect the reply subscription either.
    alice.join("third-activity").await.unwrap();
    let reply = alice.await_reply(pending).await.unwrap();
    assert_eq!(reply.room_id, activity.channel);
    assert_eq!(reply.body, Some(Body::text("graded")));
    assert_eq!(alice.current_room().await.unwrap().name, "third-activity");
    responder.await.unwrap();
}

#[tokio::test]
async fn explicit_request_does_not_auto_join_an_unknown_activity() {
    let machine = Machine::boot().await;
    machine.pin_identity("command-activity-tests").await;
    let alice = machine.solo("home").await;
    let unknown =
        airc_lib::Room::at_channel(alice.home(), "unknown", airc_core::RoomId::new()).unwrap();
    assert!(alice
        .request_in(
            &unknown,
            MentionTarget::All,
            Headers::new(),
            Body::text("must refuse"),
            Duration::from_secs(1)
        )
        .await
        .is_err());
    assert_eq!(alice.current_room().await.unwrap().name, "home");
}
