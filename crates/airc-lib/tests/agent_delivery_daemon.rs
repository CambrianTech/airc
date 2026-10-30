//! Same partition contract over the deployed daemon ScanInbox transport.
mod common;
// what this catches: over the daemon transport, history before attachment is one
// summary and the fresh request is live; once settled, neither returns (card ba22c72c).
#[tokio::test]
async fn daemon_history_is_counted_once_and_settles_when_searched() {
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
    let me = airc.addressee().await.unwrap();
    let window = airc_lib::AddressedWindow {
        candidates: 32,
        page: 8,
    };
    let summary = page
        .history_summary(
            airc_lib::EventFilter::default(),
            None,
            |_| true,
            &me,
            window,
        )
        .await
        .unwrap()
        .unwrap();
    // Counted by the daemon's store over CountInbox: every historical send, once.
    assert!(summary.counted.unwrap().count >= 96, "{summary:?}");
    assert!(summary.counted.unwrap().oldest_ms <= summary.counted.unwrap().newest_ms);
    let live = page.live_events().len();
    page.acknowledge(live, true).await.unwrap();
    // The bounded addressed search walks the rest one window per turn, silently,
    // and the history settles once it reaches the start; the fresh request is
    // never delivered again.
    for _ in 0..8 {
        let mut next = airc
            .prepare_agent_delivery("consumer", 32, 8)
            .await
            .unwrap();
        assert!(!next.live_events().iter().any(|e| e.event_id == fresh));
        if !next.has_history() {
            return;
        }
        let quiet = next
            .history_summary(
                airc_lib::EventFilter::default(),
                None,
                |_| true,
                &me,
                window,
            )
            .await
            .unwrap();
        assert!(quiet.is_none(), "count shown once, nothing addressed");
        next.acknowledge(0, true).await.unwrap();
    }
    panic!("history never settled");
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

// Measurement, not a gate: one hook-shaped turn (prepare, history summary,
// acknowledge) for a fresh consumer over a 50k-event pre-attach backlog, on the
// real daemon transport. Run: cargo test -p airc-lib --test agent_delivery_daemon
// measure_ -- --ignored --nocapture
#[tokio::test]
#[ignore = "manual measurement: one history turn over a 50k-event backlog; --ignored --nocapture"]
async fn measure_history_turn_on_a_50k_backlog() {
    let machine = common::Machine::boot().await;
    machine.pin_identity("delivery-measure").await;
    let airc = machine.attach("delivery-measure-runtime").await;
    airc.join("delivery-measure-room").await.unwrap();
    let started = std::time::Instant::now();
    for n in 0..50_000 {
        airc.say(&format!(
            "backlog message {n} with an ordinary sentence of chat"
        ))
        .await
        .unwrap();
    }
    eprintln!("seeded 50000 events in {:?}", started.elapsed());
    let me = airc.addressee().await.unwrap();
    let window = airc_lib::AddressedWindow {
        candidates: 50,
        page: 8,
    };
    for run in 0..3 {
        let consumer = format!("measure-{run}");
        let started = std::time::Instant::now();
        let mut turn = airc.prepare_agent_delivery(&consumer, 50, 8).await.unwrap();
        let summary = turn
            .history_summary(
                airc_lib::EventFilter::default(),
                None,
                |_| true,
                &me,
                window,
            )
            .await
            .unwrap();
        turn.acknowledge(0, true).await.unwrap();
        let first = started.elapsed();
        // Until card 6fb2ea16, the addressed search walks one window per call.
        let (mut walk_turns, mut slowest) = (0, std::time::Duration::ZERO);
        let walk_started = std::time::Instant::now();
        loop {
            let started = std::time::Instant::now();
            let mut turn = airc.prepare_agent_delivery(&consumer, 50, 8).await.unwrap();
            if !turn.has_history() {
                break;
            }
            turn.history_summary(
                airc_lib::EventFilter::default(),
                None,
                |_| true,
                &me,
                window,
            )
            .await
            .unwrap();
            turn.acknowledge(0, true).await.unwrap();
            walk_turns += 1;
            slowest = slowest.max(started.elapsed());
        }
        let walk = walk_started.elapsed();
        // The hot path once settled: every later hook call on the same consumer.
        let started = std::time::Instant::now();
        let next = airc.prepare_agent_delivery(&consumer, 50, 8).await.unwrap();
        assert!(!next.has_history());
        next.acknowledge(0, true).await.unwrap();
        eprintln!(
            "run {run}: first turn {first:?} (count {:?}); walk {walk_turns} turns in {walk:?} (slowest {slowest:?}); settled turn {:?}",
            summary.and_then(|s| s.counted).map(|c| c.count),
            started.elapsed()
        );
    }
}
