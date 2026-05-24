//! Integration: typed lane-coordination events round-trip between
//! two Airc instances over a shared local-fs wire.
//!
//! Proves the substrate primitive for GRID-SUBSTRATE-AUDIT flaw #3.
//! Alice claims a lane; Bob queries the lane status from his side
//! and sees the claim. Then Alice completes the lane with a PR
//! number; Bob's next query reads the completion as the latest
//! event. Bob never has to parse free-text chat — the events are
//! signed substrate primitives with stable headers.

use std::time::Duration;

use airc_lib::{Airc, LaneAction, PeerSpec};
use tempfile::TempDir;

#[tokio::test]
async fn lane_claim_and_complete_round_trip_between_two_airc_instances() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice
        .add_peer(bob_spec.clone())
        .await
        .expect("alice trusts bob");
    bob.add_peer(alice_spec.clone())
        .await
        .expect("bob trusts alice");

    alice
        .join_with_wire("lane-coord-test", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("lane-coord-test", wire_path)
        .await
        .expect("bob joins");

    // Alice claims the lane.
    let lane = "audit:#964:flaw-3";
    alice
        .claim_lane(lane, Some("typed coordination skeleton".to_string()))
        .await
        .expect("alice publishes claim");

    // Allow the shared-wire subscriber a moment to ingest.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bob queries the lane status — should see Alice's claim as the
    // latest event with the expected owner.
    let status = bob.lane_status(lane, 64).await.expect("bob queries status");
    assert!(
        status.is_claimed(),
        "lane should read as claimed after Alice's claim"
    );
    assert!(!status.is_complete(), "lane should not yet be complete");
    let latest = status.latest.expect("latest claim present");
    assert_eq!(latest.lane_id, lane);
    assert_eq!(latest.action, LaneAction::Claim);
    assert_eq!(latest.owner, alice_spec.peer_id);
    assert_eq!(
        latest.rationale.as_deref(),
        Some("typed coordination skeleton")
    );

    // Alice completes the lane with a fictional PR number.
    alice
        .complete_lane(lane, 999)
        .await
        .expect("alice publishes completion");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let status = bob
        .lane_status(lane, 64)
        .await
        .expect("bob queries status after completion");
    assert!(!status.is_claimed(), "completion supersedes the claim");
    assert!(status.is_complete(), "lane should read complete");
    let latest = status.latest.expect("latest event present");
    assert_eq!(latest.action, LaneAction::Complete);
    assert_eq!(latest.pr_number, Some(999));
    assert_eq!(
        status.history.len(),
        2,
        "history should contain both the claim and the completion"
    );
}

#[tokio::test]
async fn unrelated_lane_does_not_pollute_status() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec).await.expect("trust");
    bob.add_peer(alice_spec).await.expect("trust");
    alice
        .join_with_wire("lane-coord-isolation", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("lane-coord-isolation", wire_path)
        .await
        .expect("bob joins");

    // Alice claims lane X.
    alice.claim_lane("lane-x", None).await.expect("claim x");
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Bob queries lane Y — should see nothing for Y, even though X is
    // visible on the same wire.
    let status_y = bob.lane_status("lane-y", 64).await.expect("query y");
    assert!(
        status_y.latest.is_none(),
        "unrelated lane must read empty even when other lanes are active"
    );
    assert!(status_y.history.is_empty());

    // And lane X reads correctly.
    let status_x = bob.lane_status("lane-x", 64).await.expect("query x");
    assert!(status_x.is_claimed());
}
