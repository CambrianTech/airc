//! Integration: bridged messages round-trip with attribution
//! preserved, over the daemon.
//!
//! Proves work card fdc4b753: a bridge process (Alice) publishes a
//! message claiming "user X on Slack posted this." Bob — another scope
//! on the same machine, attached to the one owner-core daemon —
//! receives the typed `BridgedMessage`, cryptographically signed by
//! Alice's PeerId, with `ExternalIdentity` describing the external
//! user. Bob can filter by source/handle before decoding bodies.

mod common;

use std::time::Duration;

use airc_lib::{BridgedMessageFilter, ExternalIdentity, ExternalIdentitySource};
use common::Machine;
use futures::stream::StreamExt;

#[tokio::test]
async fn bridged_message_round_trip_with_typed_external_identity() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("bridge-test").await;
    let alice_peer = alice.peer_id();

    // Bob subscribes filtered by Slack source. Box::pin because the
    // filter_map closures aren't Unpin.
    let filter = BridgedMessageFilter::new().with_source(ExternalIdentitySource::Slack);
    let mut stream = Box::pin(
        bob.subscribe_bridged_messages(filter)
            .await
            .expect("subscribe"),
    );

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Alice acts as a Slack bridge — publishes a message claiming a
    // Slack user posted it. The frame is signed by Alice (the bridge),
    // and the body carries the ExternalIdentity.
    let identity = ExternalIdentity {
        source: ExternalIdentitySource::Slack,
        handle: "U012ABCD".to_string(),
        display_name: Some("Test User".to_string()),
    };
    alice
        .publish_bridged_message(
            identity.clone(),
            "hello from slack",
            Some("C012ABCD".to_string()),
        )
        .await
        .expect("alice publishes bridged message");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut got = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some((event, message))) => {
                if message.text == "hello from slack" {
                    // The daemon is a broker: it preserves the bridge's
                    // (Alice's) participant identity as the substrate
                    // author — NOT a synthetic per-Slack-user PeerId.
                    // The external user lives in the body's identity.
                    assert_eq!(
                        event.peer_id, alice_peer,
                        "frame must be authored by the bridge participant"
                    );
                    got = Some(message);
                    break;
                }
            }
            Ok(None) => panic!("subscription closed before our event"),
            Err(_) => continue,
        }
    }

    let message = got.expect("bridged message should arrive at subscriber");
    assert_eq!(message.external_identity, identity);
    assert_eq!(message.external_channel.as_deref(), Some("C012ABCD"));
    assert_eq!(message.text, "hello from slack");
}

#[tokio::test]
async fn source_filter_excludes_other_platforms() {
    let machine = Machine::boot().await;
    let alice = machine.solo("bridge-filter-test").await;

    // Alice publishes one Slack message + one Discord message.
    alice
        .publish_bridged_message(
            ExternalIdentity {
                source: ExternalIdentitySource::Slack,
                handle: "slack-user".to_string(),
                display_name: None,
            },
            "from slack",
            None,
        )
        .await
        .expect("publish slack");

    alice
        .publish_bridged_message(
            ExternalIdentity {
                source: ExternalIdentitySource::Discord,
                handle: "discord-user".to_string(),
                display_name: None,
            },
            "from discord",
            None,
        )
        .await
        .expect("publish discord");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Query Slack-only — should only see the Slack message.
    let slack_only = alice
        .recent_bridged_messages(
            BridgedMessageFilter::new().with_source(ExternalIdentitySource::Slack),
            64,
        )
        .await
        .expect("query slack");
    assert_eq!(slack_only.len(), 1);
    assert_eq!(slack_only[0].text, "from slack");
    assert_eq!(
        slack_only[0].external_identity.source,
        ExternalIdentitySource::Slack
    );

    // Query Discord-only — should only see the Discord message.
    let discord_only = alice
        .recent_bridged_messages(
            BridgedMessageFilter::new().with_source(ExternalIdentitySource::Discord),
            64,
        )
        .await
        .expect("query discord");
    assert_eq!(discord_only.len(), 1);
    assert_eq!(discord_only[0].text, "from discord");

    // Empty filter sees both.
    let all = alice
        .recent_bridged_messages(BridgedMessageFilter::default(), 64)
        .await
        .expect("query all");
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn handle_filter_excludes_other_users_on_same_platform() {
    let machine = Machine::boot().await;
    let alice = machine.solo("bridge-handle-filter").await;

    for handle in ["U001", "U002", "U003"] {
        alice
            .publish_bridged_message(
                ExternalIdentity {
                    source: ExternalIdentitySource::Slack,
                    handle: handle.to_string(),
                    display_name: None,
                },
                format!("from {handle}"),
                None,
            )
            .await
            .expect("publish");
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let target = alice
        .recent_bridged_messages(
            BridgedMessageFilter::new()
                .with_source(ExternalIdentitySource::Slack)
                .with_handle("U002"),
            64,
        )
        .await
        .expect("query U002");

    assert_eq!(target.len(), 1);
    assert_eq!(target[0].external_identity.handle, "U002");
    assert_eq!(target[0].text, "from U002");
}
