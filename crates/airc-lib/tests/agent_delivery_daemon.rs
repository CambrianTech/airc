//! Same partition contract over the deployed daemon ScanInbox transport.
mod common;
#[tokio::test]
async fn daemon_live_lane_does_not_wait_for_retained_catchup() {
    let machine = common::Machine::boot().await;
    machine.pin_identity("delivery-daemon").await;
    let airc = machine.attach("delivery-runtime").await;
    airc.join("delivery-room").await.unwrap();
    for n in 0..96 {
        airc.say(&format!("historical-{n}")).await.unwrap();
    }
    drop(
        airc.prepare_agent_delivery("consumer", 32, 8)
            .await
            .unwrap(),
    );
    let fresh = airc.say("fresh-request").await.unwrap();
    let page = airc
        .prepare_agent_delivery("consumer", 32, 8)
        .await
        .unwrap();
    assert!(page.live_events().iter().any(|e| e.event_id == fresh));
    assert!(!page.backlog_events().is_empty());
    assert!(page.has_pending(page.live_events().len(), 1));
    let live = page.live_events().len();
    page.acknowledge(live, 1).await.unwrap();
    let next = airc
        .prepare_agent_delivery("consumer", 32, 8)
        .await
        .unwrap();
    assert!(!next.live_events().iter().any(|e| e.event_id == fresh));
    assert!(!next.backlog_events().iter().any(|e| e.event_id == fresh));
    assert!(!next.backlog_events().is_empty());
}

#[tokio::test]
async fn switching_cursor_authority_refuses_existing_delivery_state() {
    let machine = common::Machine::boot().await;
    machine.pin_identity("delivery-domain").await;
    let daemon = machine.attach("delivery-domain-runtime").await;
    daemon.join("delivery-domain-room").await.unwrap();
    drop(
        daemon
            .prepare_agent_delivery("same-consumer", 8, 2)
            .await
            .unwrap(),
    );
    let embedded = airc_lib::Airc::open_with_wire_root_for_test(daemon.home(), daemon.wire_root())
        .await
        .unwrap();
    let result = embedded.prepare_agent_delivery("same-consumer", 8, 2).await;
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("changed delivery scope/state"));
    // Refusal preserves the daemon partition; a fresh daemon turn still opens.
    drop(
        daemon
            .prepare_agent_delivery("same-consumer", 8, 2)
            .await
            .unwrap(),
    );
}
