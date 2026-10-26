//! Same partition contract over the deployed daemon ScanInbox transport.
mod common;
// what this catches: over the daemon transport, history before attachment is one
// summary and the fresh request is live; once settled, neither returns (card ba22c72c).
#[tokio::test]
async fn daemon_history_is_summarized_once_and_live_is_never_behind_it() {
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
    let mut page = airc
        .prepare_agent_delivery("consumer", 32, 8)
        .await
        .unwrap();
    assert!(page.live_events().iter().any(|e| e.event_id == fresh));
    let summary = page.history_summary(|_| true).await.unwrap().unwrap();
    assert!(!summary.more && summary.count >= 96, "{summary:?}");
    assert!(summary.oldest_ms <= summary.newest_ms);
    let live = page.live_events().len();
    page.acknowledge(live, true).await.unwrap();
    let next = airc
        .prepare_agent_delivery("consumer", 32, 8)
        .await
        .unwrap();
    assert!(!next.live_events().iter().any(|e| e.event_id == fresh));
    assert!(!next.has_history());
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
