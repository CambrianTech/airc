use super::*;
use crate::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
use airc_core::{Body, Headers, MentionTarget, TranscriptKind};
async fn fixture() -> (tempfile::TempDir, Airc, RoomId) {
    let dir = tempfile::TempDir::new().unwrap();
    let airc = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
        .await
        .unwrap();
    let mut set = SubscriptionSet::empty();
    let name = ChannelName::new("delivery-test").unwrap();
    let room = set
        .subscribe_with_wire_root(dir.path(), &MeshIdentity::new("test"), name.clone())
        .unwrap()
        .room_id;
    set.set_default(name).unwrap();
    subscriptions::save(airc.coordinator_store_for_test(), &set)
        .await
        .unwrap();
    (dir, airc, room)
}
async fn append(airc: &Airc, room: RoomId, n: u64) {
    airc.append_event(TranscriptEvent {
        event_id: EventId::from_u128(n as u128),
        room_id: room,
        peer_id: airc.peer_id(),
        client_id: airc.client_id(),
        kind: TranscriptKind::Message,
        occurred_at_ms: n,
        lamport: n,
        target: MentionTarget::All,
        headers: Headers::new(),
        body: Some(Body::text(format!("event-{n}"))),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    })
    .await
    .unwrap();
}
const WINDOW: AddressedWindow = AddressedWindow {
    candidates: 64,
    page: 8,
};
async fn summary(turn: &mut AgentDeliveryTurn, airc: &Airc) -> Option<HistorySummary> {
    let me = airc.addressee().await.unwrap();
    turn.history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, WINDOW)
        .await
        .unwrap()
}
fn counted(count: u64, oldest: u64, newest: u64) -> airc_core::HistoryCount {
    airc_core::HistoryCount {
        count,
        oldest_ms: Some(oldest),
        newest_ms: Some(newest),
    }
}
// what this catches: a native reattach counts only what arrived while detached,
// never re-counting what live delivery already showed, and keeps unsettled history.
#[tokio::test]
async fn native_attach_summarizes_only_the_detached_gap() {
    let (_dir, airc, room) = fixture().await;
    let first = airc
        .prepare_native_delivery("native", true, 64, 64)
        .await
        .unwrap();
    assert!(!first.has_history());
    first.acknowledge(0, true).await.unwrap();
    append(&airc, room, 1).await;
    let live = airc
        .prepare_native_delivery("native", false, 64, 64)
        .await
        .unwrap();
    assert_eq!(live.live_events()[0].lamport, 1);
    live.acknowledge(1, true).await.unwrap();
    append(&airc, room, 2).await;
    append(&airc, room, 3).await;
    let mut attached = airc
        .prepare_native_delivery("native", true, 64, 64)
        .await
        .unwrap();
    assert!(attached.live_events().is_empty());
    assert_eq!(
        summary(&mut attached, &airc)
            .await
            .unwrap()
            .counted
            .unwrap(),
        counted(2, 2, 3)
    );
    drop(attached); // summary not shown: it stays unsettled across reattach
    append(&airc, room, 4).await;
    let mut again = airc
        .prepare_native_delivery("native", true, 64, 64)
        .await
        .unwrap();
    assert_eq!(
        summary(&mut again, &airc)
            .await
            .unwrap()
            .counted
            .unwrap()
            .count,
        3
    );
    again.acknowledge(0, true).await.unwrap();
    append(&airc, room, 5).await;
    let fresh = airc
        .prepare_native_delivery("native", false, 64, 64)
        .await
        .unwrap();
    assert!(!fresh.has_history());
    assert_eq!(fresh.live_events()[0].lamport, 5);
}
// what this catches: card ba22c72c. A long backlog is counted by the store once,
// with its original times (header-routed, no event read); it settles only when
// the bounded addressed search has covered it, and never shows the count twice;
// only events newer than attachment deliver, across a restart.
#[tokio::test]
async fn backlog_is_counted_once_and_settles_when_searched() {
    let (dir, airc, room) = fixture().await;
    for n in 1..=4999 {
        append(&airc, room, n).await;
    }
    airc.append_event(TranscriptEvent {
        event_id: EventId::from_u128(5000),
        room_id: room,
        peer_id: airc.peer_id(),
        client_id: airc.client_id(),
        kind: TranscriptKind::Message,
        occurred_at_ms: 5000,
        lamport: 5000,
        target: MentionTarget::All,
        headers: Headers::from([("airc.test.hidden".to_string(), "1".to_string())]),
        body: Some(Body::text("hidden by its header")),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    })
    .await
    .unwrap();
    let mut initial = airc
        .prepare_agent_delivery("consumer", 64, 8)
        .await
        .unwrap();
    assert!(initial.live_events().is_empty());
    let found = summary(&mut initial, &airc).await.unwrap();
    assert_eq!(found.counted.unwrap(), counted(5000, 1, 5000));
    assert!(found.addressed.is_empty());
    let routed = initial
        .history_summary(
            airc_core::HeaderFilter::Not(Box::new(airc_core::HeaderFilter::Has {
                key: "airc.test.hidden".into(),
            })),
            None,
            |_| true,
            &airc.addressee().await.unwrap(),
            WINDOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        routed.counted.unwrap().count,
        5000 - 1,
        "header-routed out of the count"
    );
    drop(initial); // output failed: the summary stays due
    append(&airc, room, 5001).await;
    let mut first = airc
        .prepare_agent_delivery("consumer", 64, 8)
        .await
        .unwrap();
    assert!(first.has_history());
    assert_eq!(first.live_events()[0].lamport, 5001);
    assert!(summary(&mut first, &airc).await.unwrap().counted.is_some());
    first.acknowledge(1, true).await.unwrap();
    // The interim addressed search walks one window (64) per turn, silently,
    // until it reaches the start: no turn settles past what it searched, and
    // the count is never shown again.
    let mut turns = 1;
    loop {
        let mut turn = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        if !turn.has_history() {
            break;
        }
        assert!(summary(&mut turn, &airc).await.is_none(), "silent walk");
        turn.acknowledge(0, true).await.unwrap();
        turns += 1;
        assert!(turns <= 5000 / 64 + 2, "the walk progresses every turn");
    }
    assert_eq!(turns, 5000 / 64 + 1);
    assert!(airc
        .load_runtime_cursor("consumer")
        .await
        .unwrap()
        .is_none()); // legacy untouched
    let reopened = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
        .await
        .unwrap();
    let mut settled = reopened
        .prepare_agent_delivery("consumer", 64, 8)
        .await
        .unwrap();
    assert!(
        !settled.has_history(),
        "settled once the walk reached the start"
    );
    assert!(summary(&mut settled, &reopened).await.is_none());
    assert!(settled.is_empty());
    drop(settled);
    append(&reopened, room, 5002).await;
    let next = reopened
        .prepare_agent_delivery("consumer", 64, 8)
        .await
        .unwrap();
    assert_eq!(next.live_events().len(), 1);
    assert_eq!(next.live_events()[0].lamport, 5002);
}
fn request(airc: &Airc, room: RoomId, n: u64) -> TranscriptEvent {
    TranscriptEvent {
        event_id: EventId::from_u128(n as u128),
        room_id: room,
        peer_id: airc.peer_id(),
        client_id: airc.client_id(),
        kind: TranscriptKind::Message,
        occurred_at_ms: n,
        lamport: n,
        target: MentionTarget::Peer(airc.peer_id()),
        headers: Headers::new(),
        body: Some(Body::text(format!("request-{n}"))),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}
// what this catches (Codex on #1617): summary settlement never buries a direct
// request. 20 addressed pre-attach requests at a page of 8 show 8, 8, then 4,
// newest first, none lost or repeated; the range settles only after the last.
#[tokio::test]
async fn addressed_requests_page_across_turns_until_every_one_is_shown() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=20 {
        airc.append_event(request(&airc, room, n)).await.unwrap();
    }
    let mut pages = Vec::new();
    for turn_no in 0..3 {
        let mut turn = airc.prepare_agent_delivery("paged", 64, 8).await.unwrap();
        assert!(turn.has_history(), "turn {turn_no} still owes requests");
        let found = summary(&mut turn, &airc).await.unwrap();
        assert_eq!(found.counted.is_some(), turn_no == 0, "count shown once");
        pages.push(
            found
                .addressed
                .iter()
                .map(|e| e.lamport)
                .collect::<Vec<_>>(),
        );
        assert_eq!(found.addressed_complete, turn_no == 2);
        turn.acknowledge(0, true).await.unwrap();
    }
    assert_eq!(
        pages,
        vec![
            (13..=20).rev().collect::<Vec<_>>(),
            (5..=12).rev().collect(),
            (1..=4).rev().collect(),
        ]
    );
    let done = airc.prepare_agent_delivery("paged", 64, 8).await.unwrap();
    assert!(!done.has_history());
}
// what this catches (Codex on #1617): when the bounded interim search cannot see
// the whole range, it never settles past a request it has not reached: an old
// request under 30 newer events is shown on the turn that reaches it, and the
// history settles only after that.
#[tokio::test]
async fn an_incomplete_search_never_settles_past_an_unseen_request() {
    let (_dir, airc, room) = fixture().await;
    airc.append_event(request(&airc, room, 1)).await.unwrap();
    for n in 2..=31 {
        append(&airc, room, n).await;
    }
    let narrow = AddressedWindow {
        candidates: 10,
        page: 8,
    };
    let me = airc.addressee().await.unwrap();
    let mut shown = Vec::new();
    for turn_no in 0..4 {
        let mut turn = airc.prepare_agent_delivery("narrow", 64, 8).await.unwrap();
        assert!(
            turn.has_history(),
            "not settled before the request is shown"
        );
        let found = turn
            .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, narrow)
            .await
            .unwrap();
        if let Some(found) = &found {
            assert_eq!(
                found.addressed_complete,
                turn_no == 3,
                "never claims completeness early"
            );
            shown.extend(found.addressed.iter().map(|e| e.lamport));
        }
        turn.acknowledge(0, true).await.unwrap();
    }
    assert_eq!(shown, vec![1]);
    let done = airc.prepare_agent_delivery("narrow", 64, 8).await.unwrap();
    assert!(!done.has_history());
}
/// Walk summary turns on `consumer` until its history settles; every
/// addressed lamport shown, in order.
async fn walk(airc: &Airc, consumer: &str, window: AddressedWindow, attach: bool) -> Vec<u64> {
    let me = airc.addressee().await.unwrap();
    let mut shown = Vec::new();
    for turn_no in 0..64 {
        let mut turn = airc
            .prepare_native_delivery(consumer, attach && turn_no == 0, 64, 64)
            .await
            .unwrap();
        if !turn.has_history() {
            return shown;
        }
        if let Some(found) = turn
            .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, window)
            .await
            .unwrap()
        {
            shown.extend(found.addressed.iter().map(|e| e.lamport));
        }
        turn.acknowledge(0, true).await.unwrap();
    }
    panic!("history never settled");
}
// what this catches (Codex (a) on #1617): a request OLDER than the bounded
// candidate tail survives a reattach that is dropped and a process restart, and
// is still emitted.
#[tokio::test]
async fn an_old_request_beyond_the_tail_survives_a_dropped_reattach_and_restart() {
    let (dir, airc, room) = fixture().await;
    airc.prepare_native_delivery("old", true, 64, 64)
        .await
        .unwrap()
        .acknowledge(0, true)
        .await
        .unwrap();
    airc.append_event(request(&airc, room, 1)).await.unwrap();
    for n in 2..=81 {
        append(&airc, room, n).await; // more than the 64-event candidate tail
    }
    drop(
        airc.prepare_native_delivery("old", true, 64, 64)
            .await
            .unwrap(),
    );
    drop(airc);
    let restarted = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
        .await
        .unwrap();
    assert_eq!(walk(&restarted, "old", WINDOW, true).await, vec![1]);
}
// what this catches (Codex (b) on #1617): the addressed checkpoint moves only
// through the range a turn actually scanned; the unscanned rest stays unsettled.
#[tokio::test]
async fn the_addressed_checkpoint_moves_only_through_scanned_ranges() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=30 {
        append(&airc, room, n).await;
    }
    let narrow = AddressedWindow {
        candidates: 10,
        page: 8,
    };
    let mut turn = airc.prepare_agent_delivery("scan", 64, 8).await.unwrap();
    let start = turn.state.summarized.clone();
    let me = airc.addressee().await.unwrap();
    turn.history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, narrow)
        .await
        .unwrap();
    turn.acknowledge(0, true).await.unwrap();
    let next = airc.prepare_agent_delivery("scan", 64, 8).await.unwrap();
    assert_eq!(
        next.state.summarized, start,
        "nothing below the scan is settled"
    );
    let rest = &next.state.unsearched;
    assert_eq!(rest.len(), 1, "one range left");
    assert_eq!(rest[0].after, start, "nothing below the scan is settled");
    assert_eq!(rest[0].top.lamport, 21, "only 21..=30 were scanned");
    assert!(rest[0].top_exclusive);
    assert!(next.has_history());
}
// what this catches (Codex (c) on #1617): showing the ambient count once does
// not stop the addressed pages that follow it.
#[tokio::test]
async fn the_ambient_count_once_does_not_suppress_later_addressed_pages() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=12 {
        airc.append_event(request(&airc, room, n)).await.unwrap();
    }
    for n in 13..=20 {
        append(&airc, room, n).await;
    }
    let me = airc.addressee().await.unwrap();
    let mut seen = Vec::new();
    for turn_no in 0..2 {
        let mut turn = airc.prepare_agent_delivery("mixed", 64, 8).await.unwrap();
        let found = turn
            .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, WINDOW)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.counted.is_some(), turn_no == 0);
        seen.push(found.addressed.len());
        turn.acknowledge(0, true).await.unwrap();
    }
    assert_eq!(seen, vec![8, 4]);
}
// what this catches (Codex (d) on #1617): output before acknowledgment may
// replay after a crash and never skips: a page shown and then lost to a crash is
// shown again, and the next page follows it.
#[tokio::test]
async fn a_crash_after_output_replays_the_page_and_never_skips() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=12 {
        airc.append_event(request(&airc, room, n)).await.unwrap();
    }
    let page = |found: Option<HistorySummary>| -> Vec<u64> {
        found
            .map(|f| f.addressed.iter().map(|e| e.lamport).collect())
            .unwrap_or_default()
    };
    let mut crashed = airc.prepare_agent_delivery("crash", 64, 8).await.unwrap();
    let first = page(summary(&mut crashed, &airc).await);
    drop(crashed); // output written, process gone before acknowledge
    let mut replay = airc.prepare_agent_delivery("crash", 64, 8).await.unwrap();
    assert_eq!(page(summary(&mut replay, &airc).await), first, "replayed");
    replay.acknowledge(0, true).await.unwrap();
    let mut next = airc.prepare_agent_delivery("crash", 64, 8).await.unwrap();
    assert_eq!(page(summary(&mut next, &airc).await), vec![4, 3, 2, 1]);
    assert_eq!(first, (5..=12).rev().collect::<Vec<_>>());
}
// what this catches (Cormac on #1617): a request that arrives during a SECOND
// outage, while older addressed pages are still owed, is searched too and
// emitted exactly once; no shown request is emitted again.
#[tokio::test]
async fn a_request_from_a_second_outage_is_emitted_exactly_once() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=20 {
        airc.append_event(request(&airc, room, n)).await.unwrap();
    }
    let me = airc.addressee().await.unwrap();
    let mut shown = Vec::new();
    let mut first = airc
        .prepare_native_delivery("outages", true, 64, 64)
        .await
        .unwrap();
    let page = first
        .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, WINDOW)
        .await
        .unwrap()
        .unwrap();
    shown.extend(page.addressed.iter().map(|e| e.lamport));
    first.acknowledge(0, true).await.unwrap(); // one page shown, then detach
    airc.append_event(request(&airc, room, 21)).await.unwrap(); // second outage
    shown.extend(walk(&airc, "outages", WINDOW, true).await);
    let mut sorted = shown.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        (1..=21).collect::<Vec<_>>(),
        "every request once: {shown:?}"
    );
    assert_eq!(&shown[..8], (13..=20).rev().collect::<Vec<_>>().as_slice());
    assert_eq!(
        shown[8], 21,
        "the late request is searched first, newest range first"
    );
}

// what this catches (Cormac on #1617): at the scan cap, "reached the start" is
// read from the raw scan. With --count 4096 and more than 4096 events below an
// exclusive top, the oldest event (a request to me) is still emitted, not settled
// unseen.
#[tokio::test]
async fn at_the_scan_cap_the_oldest_request_is_still_emitted() {
    let (_dir, airc, room) = fixture().await;
    airc.append_event(request(&airc, room, 1)).await.unwrap();
    for n in 2..=8300 {
        append(&airc, room, n).await;
    }
    let me = airc.addressee().await.unwrap();
    // At the cap, and (Codex) far above it: both clamp to one 4096-event scan.
    for (consumer, candidates) in [("capped", 4096), ("over-cap", 10_000)] {
        let window = AddressedWindow {
            candidates,
            page: 8,
        };
        // Turn 1 reads the newest 4096 (inclusive top); the rest of the range is
        // preserved as an exclusive unsearched range, never settled.
        let mut first = airc
            .prepare_native_delivery(consumer, false, 64, 64)
            .await
            .unwrap();
        first
            .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, window)
            .await
            .unwrap();
        first.acknowledge(0, true).await.unwrap();
        let after_first = airc
            .prepare_native_delivery(consumer, false, 64, 64)
            .await
            .unwrap();
        assert!(after_first.has_history(), "{consumer}: capped, not settled");
        let rest = &after_first.state.unsearched;
        assert_eq!(rest.len(), 1, "{consumer}");
        assert!(rest[0].top_exclusive, "{consumer}");
        assert_eq!(rest[0].top.lamport, 8300 - 4096 + 1, "{consumer}");
        drop(after_first);
        // Turn 2 hits the cap on that exclusive range; the request is still
        // emitted on a later turn, never settled unseen.
        assert_eq!(
            walk(&airc, consumer, window, false).await,
            vec![1],
            "{consumer}"
        );
    }
}

// what this catches (Codex on #1617): `live` is the delivery proof. A reattach
// after an outage that is dropped before any output leaves it where it was; the
// outage events are history, never marked delivered by the attach.
#[tokio::test]
async fn a_dropped_reattach_does_not_move_the_delivered_position() {
    let (_dir, airc, room) = fixture().await;
    airc.prepare_native_delivery("outage", true, 64, 64)
        .await
        .unwrap()
        .acknowledge(0, true)
        .await
        .unwrap();
    append(&airc, room, 1).await;
    let live = airc
        .prepare_native_delivery("outage", false, 64, 64)
        .await
        .unwrap();
    assert_eq!(live.live_events()[0].lamport, 1);
    live.acknowledge(1, true).await.unwrap(); // delivered through 1
    append(&airc, room, 2).await; // the outage
    append(&airc, room, 3).await;
    let reattach = airc
        .prepare_native_delivery("outage", true, 64, 64)
        .await
        .unwrap();
    assert!(
        reattach.live_events().is_empty(),
        "2..3 are history, not live"
    );
    drop(reattach); // no output, no acknowledgment
    let after = airc
        .prepare_native_delivery("outage", false, 64, 64)
        .await
        .unwrap();
    assert_eq!(after.state.live.lamport, 1, "delivered is still 1");
    assert!(after.has_history(), "2..3 remain unsettled history");
    assert!(after.live_events().is_empty());
}
// what this catches (#1617 CI, red on 3 OSes): the history count and the
// addressed page apply the SAME self-echo rule as live delivery, translated by
// the store: an unstamped own send is left out, and a send stamped by another
// runtime on this shared client still counts.
#[tokio::test]
async fn history_applies_the_one_self_echo_rule_in_the_store() {
    let (_dir, airc, room) = fixture().await;
    let send = |n: u64, stamp: Option<&str>, target: MentionTarget| TranscriptEvent {
        event_id: EventId::from_u128(n as u128),
        room_id: room,
        peer_id: airc.peer_id(),
        client_id: airc.client_id(),
        kind: TranscriptKind::Message,
        occurred_at_ms: n,
        lamport: n,
        target,
        headers: stamp
            .map(|tag| {
                Headers::from([(
                    airc_core::headers::HEADER_AIRC_CLIENT.to_string(),
                    tag.to_string(),
                )])
            })
            .unwrap_or_default(),
        body: Some(Body::text(format!("send-{n}"))),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    };
    let me_peer = MentionTarget::Peer(airc.peer_id());
    airc.append_event(send(1, None, MentionTarget::All))
        .await
        .unwrap();
    airc.append_event(send(2, Some("codex:other"), MentionTarget::All))
        .await
        .unwrap();
    airc.append_event(send(3, None, me_peer.clone()))
        .await
        .unwrap();
    airc.append_event(send(4, Some("codex:other"), me_peer))
        .await
        .unwrap();
    let mut turn = airc
        .prepare_agent_delivery("self-rule", 64, 8)
        .await
        .unwrap();
    let me = airc.addressee().await.unwrap();
    let own = airc_core::RuntimeSelf::new(airc.client_id(), None);
    let found = turn
        .history_summary(
            airc_core::HeaderFilter::Any,
            Some(&own),
            |_| true,
            &me,
            WINDOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found.counted.unwrap().count,
        2,
        "the two other-runtime sends"
    );
    let lamports: Vec<_> = found.addressed.iter().map(|e| e.lamport).collect();
    assert_eq!(
        lamports,
        [4],
        "the own request is hidden, the other runtime's shown"
    );
    let everything = summary(&mut turn, &airc).await.unwrap();
    assert_eq!(everything.counted.unwrap().count, 4);
}
// what this catches (Codex on #1617): the summary watermark and the delivery
// proof are separate persisted fields. Settling a summary marks no live event
// delivered, and delivering live events marks no history summarized.
#[tokio::test]
async fn settling_a_summary_and_delivering_live_move_separate_watermarks() {
    let (_dir, airc, room) = fixture().await;
    append(&airc, room, 1).await;
    let first = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
    let (summarized, live) = (first.state.summarized.clone(), first.state.live.clone());
    assert!(first.has_history());
    drop(first);
    append(&airc, room, 2).await;
    // Settle the summary but deliver nothing: live 2 stays undelivered.
    let mut settle = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
    assert_eq!(settle.live_events()[0].lamport, 2);
    assert!(summary(&mut settle, &airc).await.is_some());
    settle.acknowledge(0, true).await.unwrap();
    let after_settle = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
    assert!(!after_settle.has_history());
    assert_eq!(
        after_settle.state.live.lamport, live.lamport,
        "nothing delivered"
    );
    assert_eq!(after_settle.live_events()[0].lamport, 2, "live 2 still due");
    assert!(key(&after_settle.state.summarized) > key(&summarized));
    drop(after_settle);
    // A fresh consumer delivers live without settling: history stays due.
    let other = airc.prepare_agent_delivery("marks-2", 64, 8).await.unwrap();
    let summarized = other.state.summarized.clone();
    drop(other);
    append(&airc, room, 3).await;
    let deliver = airc.prepare_agent_delivery("marks-2", 64, 8).await.unwrap();
    assert_eq!(deliver.live_events()[0].lamport, 3);
    deliver.acknowledge(1, false).await.unwrap();
    let after_deliver = airc.prepare_agent_delivery("marks-2", 64, 8).await.unwrap();
    assert!(after_deliver.has_history(), "history still unsummarized");
    assert_eq!(after_deliver.state.summarized.lamport, summarized.lamport);
    assert_eq!(after_deliver.state.live.lamport, 3, "delivered");
    // The persisted key is unchanged, so rows written before the rename load.
    let row = airc
        .inner
        .store
        .get_scoped_state(SCOPE, "marks-2")
        .await
        .unwrap()
        .unwrap();
    assert!(row.value_json.contains("\"backlog\""));
}
// what this catches: output that failed before acknowledgment replays (at least once).
#[tokio::test]
async fn unacknowledged_output_replays() {
    let (_dir, airc, room) = fixture().await;
    airc.prepare_agent_delivery("one", 8, 1)
        .await
        .unwrap()
        .acknowledge(0, true)
        .await
        .unwrap();
    append(&airc, room, 1).await;
    append(&airc, room, 2).await;
    let first = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
    assert!(first.has_pending(1));
    drop(first); // includes crash-after-output: at-least-once, not silent loss
    let replay = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
    assert_eq!(replay.live_events()[0].lamport, 1);
    replay.acknowledge(1, false).await.unwrap();
    let second = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
    assert_eq!(second.live_events()[0].lamport, 2);
}
#[tokio::test]
async fn aliases_and_concurrent_prepares_share_ownership_and_new_scope_refuses() {
    let (dir, airc, room) = fixture().await;
    append(&airc, room, 1).await;
    let turn = airc.prepare_agent_delivery("same", 8, 8).await.unwrap();
    assert!(lock(&dir.path().join("."), "same").is_err());
    assert!(airc.prepare_agent_delivery("same", 8, 8).await.is_err());
    assert!(lock(dir.path(), "different").is_ok());
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        assert!(lock(&alias, "same").is_err());
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent_delivery::tests::lock_child",
            "--nocapture",
        ])
        .env("AIRC_DELIVERY_LOCK_TEST_HOME", dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    drop(turn);
    assert!(lock(dir.path(), "same").is_ok());
    let mut set = airc.subscription_set().await.unwrap();
    set.subscribe_with_wire_root(
        dir.path(),
        &MeshIdentity::new("test"),
        ChannelName::new("added-room").unwrap(),
    )
    .unwrap();
    subscriptions::save(airc.coordinator_store_for_test(), &set)
        .await
        .unwrap();
    assert!(airc.prepare_agent_delivery("same", 8, 8).await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn dropping_turn_releases_lock_even_with_a_duplicated_descriptor() {
    let (dir, airc, _) = fixture().await;
    let turn = airc
        .prepare_agent_delivery("duplicated", 8, 8)
        .await
        .unwrap();
    let duplicate = turn._guard.0.try_clone().unwrap();
    assert!(lock(dir.path(), "duplicated").is_err());
    drop(turn);
    // The duplicate remains open: close-only ownership fails this assertion.
    let next = lock(dir.path(), "duplicated").unwrap();
    drop(duplicate);
    assert!(lock(dir.path(), "duplicated").is_err());
    drop(next);
    assert!(lock(dir.path(), "duplicated").is_ok());
}
#[test]
fn lock_child() {
    if let Some(home) = std::env::var_os("AIRC_DELIVERY_LOCK_TEST_HOME") {
        assert!(lock(Path::new(&home), "same").is_err());
    }
}
#[tokio::test]
async fn unknown_state_version_refuses_without_overwriting() {
    let (_dir, airc, _) = fixture().await;
    let turn = airc.prepare_agent_delivery("future", 8, 8).await.unwrap();
    let mut state = turn.state.clone();
    state.version = 99;
    drop(turn);
    persist(&airc, "future", &state).await.unwrap();
    assert!(airc.prepare_agent_delivery("future", 8, 8).await.is_err());
    let row = airc
        .inner
        .store
        .get_scoped_state(SCOPE, "future")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<State>(&row.value_json)
            .unwrap()
            .version,
        99
    );
}
