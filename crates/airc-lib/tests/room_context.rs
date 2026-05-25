//! Integration: budgeted room-context assembly produces a
//! deterministic, bounded slice of room evidence.
//!
//! Proves work card d3930e42 (first slice).

use std::time::Duration;

use airc_lib::{Airc, ClaimWorkCard, ContextBudget, ContextItem, CreateWorkCard, Priority, RepoId};
use tempfile::TempDir;

#[tokio::test]
async fn room_context_respects_max_items_budget() {
    let home = TempDir::new().expect("home");
    let airc = Airc::open(home.path()).await.expect("open");
    let wire = TempDir::new().expect("wire");
    airc.join_with_wire("budget-test", wire.path().join("wire.jsonl"))
        .await
        .expect("join");

    // Stuff the room with chat messages so the budget kicks in.
    for i in 0..20u32 {
        airc.say(&format!("noise #{i}")).await.expect("say");
    }

    let slice = airc
        .room_context(ContextBudget {
            max_items: 5,
            max_age_ms: None,
        })
        .await
        .expect("room_context");

    assert!(
        slice.items.len() <= 5,
        "items must respect max_items, got {}",
        slice.items.len()
    );
    assert_eq!(slice.totals.events_kept, slice.items.len());
    assert!(
        slice.totals.events_seen >= 20,
        "events_seen should report what was scanned, got {}",
        slice.totals.events_seen
    );
}

#[tokio::test]
async fn room_context_orders_events_newest_first() {
    let home = TempDir::new().expect("home");
    let airc = Airc::open(home.path()).await.expect("open");
    let wire = TempDir::new().expect("wire");
    airc.join_with_wire("ordering", wire.path().join("wire.jsonl"))
        .await
        .expect("join");

    // Need distinguishable timestamps + lamports.
    for i in 0..3u32 {
        airc.say(&format!("msg-{i}")).await.expect("say");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let slice = airc
        .room_context(ContextBudget {
            max_items: 64,
            max_age_ms: None,
        })
        .await
        .expect("room_context");

    let lamports: Vec<u64> = slice
        .items
        .iter()
        .filter_map(|item| match item {
            ContextItem::Event(e) => Some(e.lamport),
            _ => None,
        })
        .collect();
    assert!(!lamports.is_empty(), "should have events");
    for window in lamports.windows(2) {
        assert!(
            window[0] >= window[1],
            "events should be sorted lamport desc: {window:?}"
        );
    }
}

#[tokio::test]
async fn room_context_includes_work_cards_and_active_claims() {
    let home = TempDir::new().expect("home");
    let airc = Airc::open(home.path()).await.expect("open");
    let wire = TempDir::new().expect("wire");
    airc.join_with_wire("cards", wire.path().join("wire.jsonl"))
        .await
        .expect("join");

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("test-org/test-repo").expect("repo"),
            title: "context-test card".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .expect("create card");

    let _claim_id = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .expect("claim");

    let slice = airc
        .room_context(ContextBudget {
            max_items: 64,
            max_age_ms: None,
        })
        .await
        .expect("room_context");

    let saw_card = slice
        .items
        .iter()
        .any(|item| matches!(item, ContextItem::WorkCard(card) if card.card_id == card_id));
    assert!(
        saw_card,
        "context must include the work card we just created"
    );

    let saw_active_claim = slice
        .items
        .iter()
        .any(|item| matches!(item, ContextItem::ActiveClaim(claim) if claim.card_id == card_id));
    assert!(
        saw_active_claim,
        "context must include the active claim we just created"
    );
    assert!(
        slice.totals.cards_kept >= 1,
        "totals should account for the kept card: {:?}",
        slice.totals
    );
}

#[tokio::test]
async fn room_context_max_age_drops_old_events_and_records_seen() {
    let home = TempDir::new().expect("home");
    let airc = Airc::open(home.path()).await.expect("open");
    let wire = TempDir::new().expect("wire");
    airc.join_with_wire("age-cap", wire.path().join("wire.jsonl"))
        .await
        .expect("join");

    // Publish noise BEFORE the budget window opens.
    for i in 0..5u32 {
        airc.say(&format!("ancient #{i}")).await.expect("say");
    }

    tokio::time::sleep(Duration::from_millis(80)).await;

    // Tight max_age_ms: only events emitted in the last 30ms
    // count. The events above should be older and dropped.
    let slice = airc
        .room_context(ContextBudget {
            max_items: 64,
            max_age_ms: Some(30),
        })
        .await
        .expect("room_context");

    let kept_events = slice
        .items
        .iter()
        .filter(|item| matches!(item, ContextItem::Event(_)))
        .count();

    // The slice may still see room-join lifecycle events that
    // landed during assembly; the invariant we care about is
    // that the older `say` payloads dropped.
    assert!(
        kept_events <= 2,
        "tight age cap should drop the older chat noise, kept {kept_events}"
    );
    assert!(
        slice.totals.events_seen >= 5,
        "totals must still report what was scanned ({} seen)",
        slice.totals.events_seen
    );
}
