use airc_core::{
    Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
    TranscriptKind,
};
use airc_lib::Airc;
use serde_json::json;

use super::render::{render_digest, render_raw, summarize_body, DIGEST_BODY_BYTES};
use super::{consumer_id, delivery_context, delivery_end, DeliverySelection};

const WINDOW: airc_lib::AddressedWindow = airc_lib::AddressedWindow {
    candidates: 50,
    page: 8,
};

/// The store-counted summary every event in the range contributes to.
async fn summarize(
    turn: &mut airc_lib::AgentDeliveryTurn,
    me: &airc_lib::Addressee,
    keep: impl Fn(&TranscriptEvent) -> bool,
) -> Option<airc_lib::HistorySummary> {
    turn.history_summary(airc_core::HeaderFilter::Any, None, keep, me, WINDOW)
        .await
        .unwrap()
}
use crate::integrations::claude::CLAUDE;
use crate::integrations::codex::hook::CODEX;

#[test]
fn lease_renewals_do_not_bury_peer_messages_and_remain_in_raw_output() {
    let mut renewal = event(Body::Json(json!({"kind":"claim_heartbeat"})));
    renewal.headers.insert(
        airc_lib::HEADER_FORGE_WORK_EVENT_KIND.into(),
        "claim_heartbeat".into(),
    );
    let events = vec![renewal, event(Body::text("review the current head"))];
    let digest = render_digest(&events, 1, "codex-hook");
    assert!(digest.contains("review the current head"));
    assert!(!digest.contains("claim_heartbeat"));
    assert!(render_raw(&events).contains("claim_heartbeat"));
    // Ordinary messages discussing a renewal are still correspondence.
    assert!(render_digest(
        &[event(Body::text("claim_heartbeat failed"))],
        1,
        "codex-hook"
    )
    .contains("claim_heartbeat failed"));
}

// what this catches: the adapters differ only in their verb; a session's cursor
// is named by its runtime client, and Codex keeps its existing checkpoint names.
#[test]
fn consumer_id_uses_the_runtime_verb_and_client() {
    assert_eq!(
        consumer_id(&CODEX, Some("codex:thread-1")),
        "codex-hook:codex:thread-1"
    );
    assert_eq!(consumer_id(&CODEX, None), "codex-hook:default");
    assert_eq!(
        consumer_id(&CLAUDE, Some("claude:s-1")),
        "claude-hook:claude:s-1"
    );
}

#[test]
fn digest_suppresses_runtime_alive_events_and_carries_original_times() {
    let mut real = event(Body::text("real update"));
    real.occurred_at_ms = 1_760_000_000_000;
    let events = vec![
        event(Body::Json(json!({
            "kind": "alive",
            "client_id": "claude:session-1",
        }))),
        real,
    ];
    let digest = render_digest(&events, 8, "claude-hook");
    assert!(digest.contains("AIRC: 1 unread"));
    assert!(
        digest.contains("[2025-10-09T08:53Z]: real update"),
        "{digest}"
    );
    assert!(!digest.contains("\"kind\":\"alive\""));
    // Raw output keeps them for debugging, including text-encoded beats.
    assert!(render_raw(&events).contains("\"kind\":\"alive\""));
    assert!(render_digest(&[event(Body::text(r#"{"kind":"alive"}"#))], 8, "x").is_empty());
}

#[test]
fn oversized_transcript_is_recoverable_without_burying_the_next_request() {
    let huge = "界".repeat(30_000);
    let retained = event(Body::text(&huge));
    let id = retained.event_id.to_string();
    let events = vec![retained, event(Body::text("review the new head"))];
    let digest = render_digest(&events, 8, "codex-hook");
    assert!(digest.len() < DIGEST_BODY_BYTES);
    assert!(digest.contains(&id));
    assert!(digest.contains("airc inbox --room"));
    assert!(digest.contains("review the new head"));
    assert!(!digest.contains(&huge));
    assert!(
        render_raw(&events).contains(&huge),
        "explicit recovery remains whole"
    );
}

async fn fixture() -> (tempfile::TempDir, Airc, RoomId) {
    use airc_lib::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
    let dir = tempfile::TempDir::new().unwrap();
    let airc = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
        .await
        .unwrap();
    let mut set = SubscriptionSet::empty();
    let channel = ChannelName::new("hook-test").unwrap();
    let room = set
        .subscribe_with_wire_root(dir.path(), &MeshIdentity::new("test"), channel.clone())
        .unwrap()
        .room_id;
    set.set_default(channel).unwrap();
    subscriptions::save(airc.coordinator_store_for_test(), &set)
        .await
        .unwrap();
    (dir, airc, room)
}

async fn append(airc: &Airc, room: RoomId, n: u64, body: Body, self_event: bool, excluded: bool) {
    let mut e = event(body);
    e.room_id = room;
    e.lamport = n;
    e.occurred_at_ms = n;
    e.event_id = EventId::from_u128(n as u128);
    if self_event {
        e.client_id = airc.client_id();
    }
    if excluded {
        e.kind = TranscriptKind::Presence;
    }
    airc.append_event(e).await.unwrap();
}

#[tokio::test]
async fn lease_renewal_does_not_spend_delivery_budget_or_reset_the_cursor() {
    let (_dir, airc, room) = fixture().await;
    airc.prepare_agent_delivery("lease-test", 50, 1)
        .await
        .unwrap()
        .acknowledge(0, true)
        .await
        .unwrap();
    let mut renewal = event(Body::Json(json!({"kind":"claim_heartbeat"})));
    renewal.room_id = room;
    renewal.lamport = 1;
    renewal.headers.insert(
        airc_lib::HEADER_FORGE_WORK_EVENT_KIND.into(),
        "claim_heartbeat".into(),
    );
    airc.append_event(renewal).await.unwrap();
    append(&airc, room, 2, Body::text("urgent review"), false, false).await;
    let turn = airc
        .prepare_agent_delivery("lease-test", 50, 1)
        .await
        .unwrap();
    let end = delivery_end(&turn, false, |_| true);
    assert_eq!(end, 2);
    assert_eq!(delivery_end(&turn, true, |_| true), 1);
    let context = delivery_context(&turn, end, false, None, None, "codex-hook", |_| true).unwrap();
    assert!(context.contains("urgent review"));
    assert!(!context.contains("claim_heartbeat"));
    turn.acknowledge(end, true).await.unwrap();
    let next = airc
        .prepare_agent_delivery("lease-test", 50, 1)
        .await
        .unwrap();
    assert!(next.is_empty());
}

// what this catches: card ba22c72c in the hook. A scope with a long unread backlog
// yields ONE history line (count and original times), live items first in the
// digest's budget, self/presence hidden; failed output replays the same context;
// once acknowledged, the history never returns and only new events deliver.
#[tokio::test]
async fn backlog_is_one_summary_then_only_new_events() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=44 {
        let body = Body::text(format!("{} old instruction", "界".repeat(50)));
        append(&airc, room, n, body, false, false).await;
    }
    let selection = DeliverySelection::default();
    let visible = |e: &TranscriptEvent| e.client_id != airc.client_id();
    let mut first = airc.prepare_agent_delivery("hook", 50, 3).await.unwrap();
    first.retain(&selection.filter);
    assert!(first.is_empty(), "history is never live");
    drop(first);
    append(&airc, room, 45, Body::text("hidden self"), true, false).await;
    append(
        &airc,
        room,
        46,
        Body::text("excluded presence"),
        false,
        true,
    )
    .await;
    append(
        &airc,
        room,
        47,
        Body::Json(json!({"kind":"alive"})),
        false,
        false,
    )
    .await;
    append(&airc, room, 48, Body::text("fresh-first"), false, false).await;
    append(&airc, room, 49, Body::text("fresh-second"), false, false).await;
    let mut turn = airc.prepare_agent_delivery("hook", 50, 3).await.unwrap();
    turn.retain(&selection.filter);
    let end = delivery_end(&turn, false, visible);
    let me = airc.addressee().await.unwrap();
    let summary = summarize(&mut turn, &me, |e| {
        selection.filter.matches(e) && visible(e)
    })
    .await
    .unwrap();
    assert_eq!(
        summary.counted,
        Some(airc_core::HistoryCount {
            count: 44,
            oldest_ms: Some(1),
            newest_ms: Some(44)
        })
    );
    let context = delivery_context(
        &turn,
        end,
        false,
        None,
        Some(&summary),
        "codex-hook",
        visible,
    )
    .unwrap();
    assert!(context.starts_with(
        "AIRC: 44 earlier events from before this session attached (1970-01-01T00:00Z to 1970-01-01T00:00Z); the newest 8 follow"
    ), "{context}");
    assert!(context.contains("fresh-first") && context.contains("fresh-second"));
    // The newest page of the room (as a channel opens), never the older 36.
    assert_eq!(context.matches("old instruction").count(), 8, "{context}");
    assert!(!context.contains("hidden self"));
    assert!(!context.contains("excluded presence"));
    drop(turn); // output failure: no acknowledgment, the same context is due again
    let mut retry = airc.prepare_agent_delivery("hook", 50, 3).await.unwrap();
    retry.retain(&selection.filter);
    assert_eq!(delivery_end(&retry, false, visible), end);
    assert!(retry.has_history());
    assert!(summarize(&mut retry, &me, visible).await.is_some());
    retry.acknowledge(end, true).await.unwrap();
    append(&airc, room, 50, Body::text("after-attach"), false, false).await;
    let mut next = airc.prepare_agent_delivery("hook", 50, 3).await.unwrap();
    next.retain(&selection.filter);
    assert!(!next.has_history());
    let end = delivery_end(&next, false, visible);
    let context = delivery_context(&next, end, false, None, None, "codex-hook", visible).unwrap();
    assert!(context.contains("after-attach"));
    assert!(!context.contains("fresh-first") && !context.contains("old instruction"));
    assert_eq!(summarize_body(&next.live_events()[0]), "after-attach");
}

// what this catches: a request addressed to this reader two minutes before its first
// hook call is shown whole with its original time, while 500 old room messages stay
// one summary line (the DeliverySelection contract: a direct request is never a count).
#[tokio::test]
async fn a_direct_request_before_attach_is_shown_while_room_history_is_one_line() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=500 {
        append(
            &airc,
            room,
            n,
            Body::text(format!("old-room-{n}")),
            false,
            false,
        )
        .await;
    }
    let two_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 120_000;
    let mut direct = event(Body::text("please review #1617"));
    direct.room_id = room;
    direct.lamport = 501;
    direct.event_id = EventId::from_u128(501);
    direct.kind = TranscriptKind::Message;
    direct.occurred_at_ms = two_minutes_ago;
    direct.target = MentionTarget::Peer(airc.peer_id());
    airc.append_event(direct).await.unwrap();
    let options = super::HookOptions {
        count: 50,
        max_items: 8,
        raw: false,
        selection: DeliverySelection::default(),
    };
    let mut turn = super::prepare(&airc, "claude-hook:direct", &options)
        .await
        .unwrap();
    let (context, end) = super::compose(&CLAUDE, &airc, &mut turn, None, &options, None)
        .await
        .unwrap();
    let context = context.unwrap();
    assert_eq!(
        context
            .matches("earlier events from before this session attached")
            .count(),
        1,
        "{context}"
    );
    assert!(context.contains("AIRC: 501 earlier events"), "{context}");
    // One bounded read of the newest page (8 events): the direct request is one of
    // them and shows in its own section, so 7 room events follow the count.
    assert!(context.contains("the newest 7 follow"), "{context}");
    let shows = |body: &str| {
        context
            .lines()
            .any(|line| line.ends_with(&format!(": {body}")))
    };
    assert!(shows("old-room-500") && shows("old-room-494"), "{context}");
    assert!(
        !shows("old-room-493"),
        "the older 493 are counted, not shown: {context}"
    );
    let shown = format!(
        "[{}]: please review #1617",
        super::render::original_time(two_minutes_ago)
    );
    assert!(context.contains(&shown), "{context}");
    turn.acknowledge(end, true).await.unwrap();
    let mut next = super::prepare(&airc, "claude-hook:direct", &options)
        .await
        .unwrap();
    let (again, _) = super::compose(&CLAUDE, &airc, &mut next, None, &options, None)
        .await
        .unwrap();
    assert!(again.is_none(), "shown once: {again:?}");
}

async fn append_text(airc: &Airc, room: RoomId, n: u64, text: &str, target: MentionTarget) {
    let mut e = event(Body::text(text));
    e.room_id = room;
    e.lamport = n;
    e.occurred_at_ms = n;
    e.event_id = EventId::from_u128(n as u128);
    e.kind = TranscriptKind::Message;
    e.target = target;
    airc.append_event(e).await.unwrap();
}

// what this catches (Cormac on #1617): today's requests are `@name` text broadcast to
// the room. A pre-attach "@<my name> ..." with target All is shown whole; "@Fable
// @Codex" is shown to each; a name mid-sentence is ambient and only counted.
#[tokio::test]
async fn leading_at_names_address_pre_attach_history_and_mid_sentence_names_do_not() {
    let (_dir, airc, room) = fixture().await;
    let mine = format!("@{} please rebase", airc.agent_name().to_uppercase());
    append_text(&airc, room, 1, &mine, MentionTarget::All).await;
    append_text(
        &airc,
        room,
        2,
        "@Fable @Codex review #1617",
        MentionTarget::All,
    )
    .await;
    append_text(&airc, room, 3, "I told Fable already", MentionTarget::All).await;
    let mut turn = airc.prepare_agent_delivery("names", 50, 8).await.unwrap();
    let me = airc.addressee().await.unwrap();
    let lamports =
        |events: &[TranscriptEvent]| events.iter().map(|e| e.lamport).collect::<Vec<_>>();
    let summary = summarize(&mut turn, &me, |_| true).await.unwrap();
    assert_eq!(
        lamports(&summary.addressed),
        [1],
        "only my leading name addresses me"
    );
    assert_eq!(
        lamports(&summary.recent),
        [2, 3],
        "the rest is room history, shown whole"
    );
    let text = super::render::history_section(&summary).unwrap();
    assert!(text.contains("please rebase"), "{text}");
    assert!(text.contains("AIRC: 3 earlier events"), "{text}");
    for name in ["Fable", "codex"] {
        let other = airc_lib::Addressee::new(PeerId::new(), [name.to_string()]);
        let summary = summarize(&mut turn, &other, |_| true).await.unwrap();
        assert_eq!(
            lamports(&summary.addressed),
            [2],
            "{name}: both leading names address"
        );
        assert_eq!(
            lamports(&summary.recent),
            [1, 3],
            "{name}: mid-sentence is not addressed"
        );
    }
}

// what this catches: 120 requests addressed to me before attach show one reader
// page (the digest page), newest first, and one line counting the rest; never a dump.
// (The interim search reading only the newest candidates is pinned in airc-lib.)
#[tokio::test]
async fn many_addressed_requests_show_one_page_and_a_count() {
    let (_dir, airc, room) = fixture().await;
    for n in 1..=120 {
        let body = format!("request-{n:03}");
        append_text(&airc, room, n, &body, MentionTarget::Peer(airc.peer_id())).await;
    }
    // `--count` above the backlog: the interim addressed search covers it all.
    let options = super::HookOptions {
        count: 200,
        max_items: super::render::DIGEST_PAGE,
        raw: false,
        selection: DeliverySelection::default(),
    };
    let mut turn = super::prepare(&airc, "claude-hook:many", &options)
        .await
        .unwrap();
    let (context, _) = super::compose(&CLAUDE, &airc, &mut turn, None, &options, None)
        .await
        .unwrap();
    let context = context.unwrap();
    let shown = context.matches("request-").count();
    assert_eq!(shown, super::render::DIGEST_PAGE, "{context}");
    assert!(context.find("request-120").unwrap() < context.find("request-113").unwrap());
    assert!(!context.contains("request-112"), "{context}");
    assert!(context.contains("AIRC: 120 earlier events"), "{context}");
    let more = 120 - super::render::DIGEST_PAGE;
    assert!(
        context.contains(&format!("AIRC: {more} more addressed to you")),
        "{context}"
    );
}

#[tokio::test]
async fn production_empty_bootstrap_preserves_burst_larger_than_page() {
    let (_dir, airc, room) = fixture().await;
    let empty = airc.prepare_agent_delivery("burst", 3, 3).await.unwrap();
    assert!(empty.is_empty());
    empty.acknowledge(0, false).await.unwrap(); // actual pre-wait path
    for n in 1..=9 {
        append(
            &airc,
            room,
            n,
            Body::text(format!("burst-{n}")),
            false,
            false,
        )
        .await;
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
        let page = airc.prepare_agent_delivery("burst", 3, 3).await.unwrap();
        let l = delivery_end(&page, false, |_| true);
        seen.extend(page.live_events()[..l].iter().map(|e| e.lamport));
        assert!(delivery_context(&page, l, false, None, None, "codex-hook", |_| true).is_some());
        page.acknowledge(l, true).await.unwrap();
    }
    assert_eq!(seen, (1..=9).collect::<Vec<_>>());
    assert!(airc
        .prepare_agent_delivery("burst", 3, 3)
        .await
        .unwrap()
        .is_empty());
}

fn event(body: Body) -> TranscriptEvent {
    TranscriptEvent {
        event_id: EventId::new(),
        room_id: RoomId::new(),
        peer_id: PeerId::new(),
        client_id: ClientId::new(),
        kind: TranscriptKind::System,
        occurred_at_ms: 1,
        lamport: 1,
        target: MentionTarget::All,
        headers: Headers::new(),
        body: Some(body),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}
