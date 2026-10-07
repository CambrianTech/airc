//! One socket per subscriber, not one per room.
//!
//! What this catches (2026-10-04): a continuum core with ~15 subscribers
//! held ~960 attach sockets to its airc daemon because `daemon_subscribe`
//! opened one attach per ROOM per subscriber. A subscriber of N rooms now
//! attaches once with a channel set when the daemon serves sets; every
//! room's events still arrive, each tagged with its own room, and the set
//! survives a daemon restart with every room's durable gap replayed.

mod common;

use std::time::Duration;

use airc_core::Body;
use common::Machine;
use futures::stream::StreamExt;

async fn daemon_connections(socket: &std::path::Path) -> usize {
    airc_ipc::DaemonClient::new(socket.to_path_buf())
        .status()
        .await
        .expect("daemon status")
        .connections
        .expect("a daemon that counts its connections")
}

/// Drain `stream` until `want` texts have arrived or the deadline passes;
/// returns (room, text) in arrival order.
async fn collect(
    stream: &mut airc_lib::FilteredEventStream,
    want: usize,
) -> Vec<(uuid::Uuid, String)> {
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while seen.len() < want && std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                    seen.push((event.room_id.0, text.to_string()));
                }
            }
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscriber_of_two_rooms_holds_one_attach_socket_and_hears_both() {
    let mut machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("set-room-a").await;
    let room_a = alice.current_room().await.expect("alice's room a").channel;
    let room_b = alice
        .join("set-room-b")
        .await
        .expect("alice joins b")
        .channel;
    bob.join("set-room-b").await.expect("bob joins b");
    assert_ne!(room_a, room_b);
    // `join` is idempotent on membership and sets the current room, so it is
    // how alice aims `say` at one room or the other.
    let say_in = |room: &'static str, text: &'static str| {
        let alice = &alice;
        async move {
            alice.join(room).await.expect("alice switches room");
            alice.say(text).await.expect("alice says");
        }
    };

    let socket = machine.daemon.socket.clone();
    let before = daemon_connections(&socket).await;
    let mut bob_stream = bob
        .subscribe_subscribed_filtered(airc_lib::EventFilter::default())
        .await
        .expect("bob subscribes to both rooms");
    let after = daemon_connections(&socket).await;
    assert_eq!(
        after - before,
        1,
        "two rooms must cost ONE attach socket (per-room attaches would cost two)"
    );

    say_in("set-room-b", "in-b").await;
    say_in("set-room-a", "in-a").await;
    let mut heard = collect(&mut bob_stream, 2).await;
    heard.sort();
    let mut expected = vec![
        (room_a.0, "in-a".to_string()),
        (room_b.0, "in-b".to_string()),
    ];
    expected.sort();
    assert_eq!(
        heard, expected,
        "both rooms arrive on the one stream, each tagged with its room"
    );

    // The set survives a daemon restart: the reattach carries each room's
    // own cursor, so lines published while the daemon was down are replayed
    // for BOTH rooms and nothing older is re-fed.
    machine.restart_daemon().await;
    say_in("set-room-a", "after-restart-a").await;
    say_in("set-room-b", "after-restart-b").await;
    let mut heard = collect(&mut bob_stream, 2).await;
    heard.sort();
    let mut expected = vec![
        (room_a.0, "after-restart-a".to_string()),
        (room_b.0, "after-restart-b".to_string()),
    ];
    expected.sort();
    assert_eq!(
        heard, expected,
        "after a daemon restart the set resumes every room"
    );
}
