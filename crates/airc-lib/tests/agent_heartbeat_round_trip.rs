//! Integration: agent-heartbeat events round-trip between two Airc
//! instances over a shared local-fs wire.
//!
//! Proves GRID-SUBSTRATE-AUDIT flaw #6 substrate primitive. Alice
//! spawns a heartbeat task; Bob queries `active_agents` from his
//! side and sees Alice as live with the correct runtime/scope. Then
//! Alice emits an explicit `Leaving` beat; Bob's next query
//! excludes Alice. Bob never has to read inbox prose — typed
//! liveness with stable headers, queryable from any subscribed
//! peer.

use std::time::Duration;

use airc_lib::{Airc, HeartbeatKind, PeerSpec};
use tempfile::TempDir;

#[tokio::test]
async fn heartbeat_task_is_visible_via_active_agents_query() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec).await.expect("alice trusts bob");
    bob.add_peer(alice_spec.clone())
        .await
        .expect("bob trusts alice");

    alice
        .join_with_wire("heartbeat-test", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("heartbeat-test", wire_path)
        .await
        .expect("bob joins");

    // Alice starts emitting heartbeats. Short interval so the test
    // doesn't have to wait for a 60s tick; the first beat is
    // emitted synchronously inside start_agent_heartbeat so we don't
    // need to wait for any tick either.
    let heartbeat = alice
        .start_agent_heartbeat(
            "claude",
            Some("/work/airc".to_string()),
            Duration::from_secs(60),
        )
        .await
        .expect("alice starts heartbeat");

    // Give the wire-share subscriber time to ingest the synchronous
    // first beat.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let alive = bob
        .active_agents(Duration::from_secs(120), 64)
        .await
        .expect("bob queries active agents");
    assert!(
        alive
            .iter()
            .any(|liveness| liveness.peer == alice_spec.peer_id),
        "alice should appear as alive after her first heartbeat; got {alive:?}"
    );
    let alice_view = alive
        .iter()
        .find(|liveness| liveness.peer == alice_spec.peer_id)
        .expect("alice view present");
    assert_eq!(alice_view.runtime, "claude");
    assert_eq!(alice_view.scope.as_deref(), Some("/work/airc"));

    // Alice explicitly leaves.
    alice
        .emit_agent_heartbeat(
            HeartbeatKind::Leaving,
            "claude",
            Some("/work/airc".to_string()),
        )
        .await
        .expect("alice emits leaving");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let alive = bob
        .active_agents(Duration::from_secs(120), 64)
        .await
        .expect("bob queries after leaving");
    assert!(
        !alive
            .iter()
            .any(|liveness| liveness.peer == alice_spec.peer_id),
        "alice should no longer appear after Leaving; got {alive:?}"
    );

    heartbeat.stop().await;
}

#[tokio::test]
async fn stale_heartbeats_are_filtered_by_within_window() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");
    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec).await.expect("trust");
    bob.add_peer(alice_spec.clone()).await.expect("trust");

    alice
        .join_with_wire("heartbeat-staleness", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("heartbeat-staleness", wire_path)
        .await
        .expect("bob joins");

    // Alice emits a single beat.
    alice
        .emit_agent_heartbeat(HeartbeatKind::Alive, "claude", None)
        .await
        .expect("alice emits");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Query with a 1ms window — Alice's beat is older than that, so
    // she should be filtered out. Sleep a bit to make sure the
    // timestamp on the beat is comfortably outside the 1ms window.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let alive = bob
        .active_agents(Duration::from_millis(1), 64)
        .await
        .expect("bob queries with 1ms window");
    assert!(
        !alive.iter().any(|liveness| liveness.peer == alice_spec.peer_id),
        "alice's beat is older than the 1ms staleness window — should be filtered out; got {alive:?}"
    );

    // Same data, generous window — alice should appear.
    let alive = bob
        .active_agents(Duration::from_secs(60), 64)
        .await
        .expect("bob queries with 60s window");
    assert!(
        alive
            .iter()
            .any(|liveness| liveness.peer == alice_spec.peer_id),
        "alice's beat falls within 60s — should appear; got {alive:?}"
    );
}
