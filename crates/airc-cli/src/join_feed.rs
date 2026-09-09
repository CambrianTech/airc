//! Cursor-backed live feed for `airc join`.
//!
//! `airc join` is the public recovery/live verb. Agent runtimes keep it
//! open as their event feed; scripts/tests let it return. This module
//! keeps the feed usable by storing a per-runtime cursor in
//! `airc-store` so each attach starts at "new since last seen" instead
//! of replaying the full transcript.

use airc_core::{TranscriptCursor, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, EventFilter, FilteredEventStream, LiveLag};
use futures::stream::StreamExt;
use std::io::Write;
use std::sync::Arc;

use crate::client_id::{current_client_id, RuntimeSelfFilter};

const CONSUMER_PREFIX: &str = "join-feed";
const CATCH_UP_LIMIT: usize = 64;

pub async fn run(airc: &Airc, quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    let runtime_client = current_client_id()?;
    let consumer_id = consumer_id(runtime_client.as_deref());
    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let mut output = std::io::stdout();
    let mut attached = attach_feed(airc, &consumer_id, &self_filter, &mut output).await?;
    if !quiet {
        println!();
        println!("attached — Ctrl-C to detach.");
    }
    print_stream_advancing_cursor(
        airc,
        &mut attached.stream,
        &consumer_id,
        &self_filter,
        &mut output,
        attached.replayed_through,
    )
    .await
}

struct AttachedFeed {
    stream: FilteredEventStream,
    replayed_through: Option<TranscriptCursor>,
}

async fn attach_feed(
    airc: &Airc,
    consumer_id: &str,
    self_filter: &RuntimeSelfFilter<'_>,
    output: &mut impl Write,
) -> Result<AttachedFeed, Box<dyn std::error::Error>> {
    // Subscribe/ack BEFORE taking the replay snapshot. Arrivals during replay
    // are buffered; the fixed snapshot separates replay from overlapping live
    // delivery. A live checkpoint cannot overtake unread historical pages.
    let stream = airc
        .subscribe_subscribed_filtered(EventFilter::default())
        .await?;
    let replayed_through = print_catch_up(airc, consumer_id, self_filter, output).await?;
    Ok(AttachedFeed {
        stream,
        replayed_through,
    })
}

async fn print_catch_up(
    airc: &Airc,
    consumer_id: &str,
    self_filter: &RuntimeSelfFilter<'_>,
    output: &mut impl Write,
) -> Result<Option<TranscriptCursor>, Box<dyn std::error::Error>> {
    // A room-tip query does not let a newer unrelated-room event hide the
    // target. Freeze this cursor so replay never chases live traffic or the
    // SubscriptionAdvanced events emitted by our own checkpoints.
    let previous = airc.load_runtime_cursor(consumer_id).await?;
    let Some(target) = airc.latest_subscribed_cursor().await? else {
        return Ok(previous);
    };
    let Some(mut cursor) = previous else {
        // First attach starts at the existing live edge, as before.
        airc.save_runtime_cursor(consumer_id, &target).await?;
        return Ok(Some(target));
    };
    while cursor_before(&cursor, &target) {
        let page = airc
            .scan_subscribed_events(&cursor, EventFilter::default(), CATCH_UP_LIMIT)
            .await?;
        let Some(scanned_through) = page.scanned_through else {
            return Err(
                "join feed replay ended before its durable snapshot; cursor retained".into(),
            );
        };
        if !cursor_before(&cursor, &scanned_through) {
            return Err("join feed replay made no cursor progress; cursor retained".into());
        }
        for event in &page.events {
            if !cursor_before(&target, &event.cursor()) {
                print_event(event, self_filter, output)?;
            }
        }
        // Filtered-empty pages still advance the raw scan. Never checkpoint
        // beyond the fixed target, even if the final page includes new traffic.
        cursor = if cursor_before(&scanned_through, &target) {
            scanned_through
        } else {
            target.clone()
        };
        airc.save_runtime_cursor(consumer_id, &cursor).await?;
    }
    // A subscription change can lower the current tip. Retain the existing
    // consumer floor rather than replaying older buffered traffic as new.
    Ok(Some(cursor))
}

async fn print_stream_advancing_cursor<S>(
    airc: &Airc,
    stream: &mut S,
    consumer_id: &str,
    self_filter: &RuntimeSelfFilter<'_>,
    output: &mut impl Write,
    mut replayed_through: Option<TranscriptCursor>,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: futures::stream::Stream<Item = Result<Arc<TranscriptEvent>, LiveLag>> + Unpin,
{
    let sigint = tokio::signal::ctrl_c();
    let mut sigint = Box::pin(sigint);
    loop {
        tokio::select! {
            biased;
            _ = &mut sigint => {
                println!();
                println!("interrupted; exiting.");
                return Ok(());
            }
            next = stream.next() => {
                match next {
                    Some(Ok(event)) => {
                        if replayed_through.as_ref().is_some_and(|cursor| {
                            !cursor_before(cursor, &event.cursor())
                        }) {
                            continue;
                        }
                        print_event(&event, self_filter, output)?;
                        airc.save_runtime_cursor_for_event(consumer_id, &event).await?;
                    }
                    Some(Err(lag)) => {
                        eprintln!("{lag}");
                        // The bounded live buffer can lag while replay is
                        // draining. Recover durably before any later live
                        // event is allowed to advance the checkpoint.
                        replayed_through = print_catch_up(airc, consumer_id, self_filter, output).await?;
                    }
                    None => {
                        println!("stream closed; exiting.");
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn consumer_id(runtime_client: Option<&str>) -> String {
    let suffix = runtime_client.unwrap_or("default");
    format!("{CONSUMER_PREFIX}:{suffix}")
}

fn cursor_before(left: &TranscriptCursor, right: &TranscriptCursor) -> bool {
    (left.lamport, left.event_id.0) < (right.lamport, right.event_id.0)
}

fn print_event(
    event: &TranscriptEvent,
    self_filter: &RuntimeSelfFilter<'_>,
    output: &mut impl Write,
) -> std::io::Result<()> {
    // Cursor lifecycle events are local delivery bookkeeping, not a message.
    // Suppress by the typed kind before inspecting/rendering any body; their
    // fresh durable client IDs cannot honestly be classified as runtime self.
    if event.kind == TranscriptKind::SubscriptionAdvanced || self_filter.is_self_event(event) {
        return Ok(());
    }
    // Structured events render by kind; `alive` heartbeats are suppressed
    // (None) so they don't drown the feed. See `event_render`.
    if let Some(line) = crate::event_render::render_feed_line(event) {
        writeln!(output, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use airc_core::{Body, ClientId, EventId, Headers, MentionTarget, RoomId, TranscriptKind};
    use airc_lib::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
    use airc_protocol::HEADER_AIRC_CLIENT;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn consumer_id_names_runtime_checkpoints() {
        assert_eq!(
            consumer_id(Some("codex:thread-1")),
            "join-feed:codex:thread-1"
        );
        assert_eq!(consumer_id(None), "join-feed:default");
    }

    #[tokio::test]
    async fn first_attach_checkpoints_hidden_self_without_replaying_history() {
        let (_dir, airc, room) = fixture().await;
        let event = event(&airc, room, 1, "codex:thread-1", "own message");
        let cursor = event.cursor();
        airc.append_event(event).await.unwrap();
        let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:thread-1"));
        let mut output = Vec::new();

        print_catch_up(&airc, "first-attach", &self_filter, &mut output)
            .await
            .unwrap();

        assert!(output.is_empty());
        assert_eq!(
            airc.load_runtime_cursor("first-attach").await.unwrap(),
            Some(cursor)
        );
    }

    #[tokio::test]
    async fn catch_up_and_live_share_runtime_filter_and_advance_past_hidden_self() {
        let (_dir, airc, room) = fixture().await;
        let seed = event(&airc, room, 1, "codex:thread-1", "already seen");
        // Seed only the checkpoint, avoiding incidental lifecycle events in
        // this deterministic transcript fixture. The consumer paths below use
        // the real Airc cursor API, including its lifecycle emission.
        airc.coordinator_store_for_test()
            .save_runtime_cursor("catch-up", &seed.cursor(), 1)
            .await
            .unwrap();
        let mut legacy = event(&airc, room, 3, "", "legacy own message");
        legacy.headers.clear();
        let mut foreign = event(&airc, room, 4, "claude:remote", "remote message");
        foreign.peer_id = airc_core::PeerId::new();
        foreign.client_id = ClientId::new();
        let events = vec![
            event(&airc, room, 2, "claude:session-1", "shared peer colleague"),
            legacy,
            foreign,
            event(&airc, room, 5, "codex:thread-1", "hidden final self"),
        ];
        let newest_cursor = events.last().unwrap().cursor();
        for event in &events {
            airc.append_event(event.clone()).await.unwrap();
        }
        let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:thread-1"));
        let mut catch_up = Vec::new();
        print_catch_up(&airc, "catch-up", &self_filter, &mut catch_up)
            .await
            .unwrap();

        let mut stream = futures::stream::iter(events.into_iter().map(|event| Ok(Arc::new(event))));
        let mut live = Vec::new();
        print_stream_advancing_cursor(&airc, &mut stream, "live", &self_filter, &mut live, None)
            .await
            .unwrap();

        assert_eq!(
            catch_up, live,
            "replay and live delivery must display the same events"
        );
        let visible = String::from_utf8(live).unwrap();
        assert!(visible.contains("shared peer colleague"));
        assert!(visible.contains("remote message"));
        assert!(!visible.contains("legacy own message"));
        assert!(!visible.contains("hidden final self"));
        assert_eq!(visible.lines().count(), 2);
        for consumer in ["catch-up", "live"] {
            assert_eq!(
                airc.load_runtime_cursor(consumer).await.unwrap(),
                Some(newest_cursor.clone()),
                "{consumer} must advance past the final hidden event"
            );
        }
    }

    #[tokio::test]
    async fn production_attach_drains_backlog_before_live_without_cursor_noise() {
        let (_dir, airc, room) = fixture().await;
        let seed = event(&airc, room, 0, "codex:thread-1", "already seen");
        airc.coordinator_store_for_test()
            .save_runtime_cursor("capped", &seed.cursor(), 0)
            .await
            .unwrap();
        for lamport in 1..=CATCH_UP_LIMIT as u64 {
            let event = event(&airc, room, lamport, "codex:thread-1", "hidden self");
            airc.append_event(event).await.unwrap();
        }
        let colleague = event(
            &airc,
            room,
            CATCH_UP_LIMIT as u64 + 1,
            "claude:session-1",
            "unread colleague after self page",
        );
        let colleague_cursor = colleague.cursor();
        airc.append_event(colleague).await.unwrap();
        let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:thread-1"));
        let mut output = Vec::new();

        // This is the exact production subscribe -> snapshot -> replay path,
        // invoked ONCE. The next live checkpoint must not overtake colleague65.
        let mut attached = attach_feed(&airc, "capped", &self_filter, &mut output)
            .await
            .unwrap();
        assert_eq!(
            airc.load_runtime_cursor("capped").await.unwrap(),
            Some(colleague_cursor)
        );
        assert!(String::from_utf8_lossy(&output).contains("unread colleague after self page"));

        airc.say_with_headers(
            "new live self message",
            Headers::from([(HEADER_AIRC_CLIENT.to_string(), "codex:thread-1".to_string())]),
        )
        .await
        .unwrap();
        // The real subscription buffers two cursor events emitted DURING
        // catch-up, this live message, then its resulting cursor event. If the
        // subscription moves after catch-up, this bounded run times out.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            print_stream_advancing_cursor(
                &airc,
                &mut attached.stream.by_ref().take(4),
                "capped",
                &self_filter,
                &mut output,
                attached.replayed_through,
            ),
        )
        .await
        .expect("actual live subscription must include catch-up cursor events")
        .unwrap();
        let visible = String::from_utf8(output).unwrap();
        assert_eq!(visible.lines().count(), 1);
        assert!(!visible.contains("SubscriptionAdvanced"));
        assert!(!visible.contains("new live self message"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), attached.stream.next())
                .await
                .is_err(),
            "cursor events must not recursively emit more cursor events"
        );
    }

    #[tokio::test]
    async fn replay_scans_filtered_empty_pages_and_full_cursor_ties() {
        let (_dir, airc, room) = fixture().await;
        let seed = event(&airc, room, 0, "codex:thread-1", "already seen");
        airc.coordinator_store_for_test()
            .save_runtime_cursor("filtered", &seed.cursor(), 0)
            .await
            .unwrap();
        let unrelated_room = RoomId::new();
        for id in 1..=CATCH_UP_LIMIT as u128 {
            let mut unrelated = event(&airc, unrelated_room, 1, "claude:other", "other room");
            unrelated.event_id = EventId::from_u128(id);
            airc.append_event(unrelated).await.unwrap();
        }
        let mut colleague = event(&airc, room, 1, "claude:session-1", "after filtered page");
        colleague.event_id = EventId::from_u128(CATCH_UP_LIMIT as u128 + 1);
        let expected_cursor = colleague.cursor();
        airc.append_event(colleague).await.unwrap();
        // A newer unrelated-room event must not hide the subscribed snapshot.
        airc.append_event(event(
            &airc,
            unrelated_room,
            2,
            "claude:other",
            "newest unrelated",
        ))
        .await
        .unwrap();
        let page = airc
            .scan_subscribed_events(&seed.cursor(), EventFilter::default(), CATCH_UP_LIMIT)
            .await
            .unwrap();
        assert!(page.events.is_empty());
        assert_eq!(
            page.scanned_through.unwrap().event_id,
            EventId::from_u128(CATCH_UP_LIMIT as u128)
        );

        let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:thread-1"));
        let mut output = Vec::new();
        let attached = attach_feed(&airc, "filtered", &self_filter, &mut output)
            .await
            .unwrap();
        assert_eq!(attached.replayed_through, Some(expected_cursor.clone()));
        assert_eq!(
            airc.load_runtime_cursor("filtered").await.unwrap(),
            Some(expected_cursor)
        );
        let visible = String::from_utf8(output).unwrap();
        assert_eq!(visible.lines().count(), 1);
        assert!(visible.contains("after filtered page"));
        assert!(!visible.contains("other room"));
    }

    #[tokio::test]
    async fn live_overlap_skips_replayed_events_and_recovers_lag_before_advancing() {
        let (_dir, airc, room) = fixture().await;
        let first = event(&airc, room, 1, "claude:session-1", "replayed once");
        let replayed = first.cursor();
        airc.append_event(first.clone()).await.unwrap();
        airc.coordinator_store_for_test()
            .save_runtime_cursor("overlap", &replayed, 1)
            .await
            .unwrap();
        let missing = event(
            &airc,
            room,
            2,
            "claude:session-1",
            "recovered lagged message",
        );
        let newest = missing.cursor();
        airc.append_event(missing.clone()).await.unwrap();
        let mut stream = futures::stream::iter([
            Ok(Arc::new(first)),
            Err(LiveLag { skipped: 1 }),
            Ok(Arc::new(missing)),
        ]);
        let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:thread-1"));
        let mut output = Vec::new();
        print_stream_advancing_cursor(
            &airc,
            &mut stream,
            "overlap",
            &self_filter,
            &mut output,
            Some(replayed),
        )
        .await
        .unwrap();
        assert_eq!(
            airc.load_runtime_cursor("overlap").await.unwrap(),
            Some(newest)
        );
        let visible = String::from_utf8(output).unwrap();
        assert_eq!(visible.lines().count(), 1);
        assert!(visible.contains("recovered lagged message"));
        assert!(!visible.contains("replayed once"));
        assert!(!visible.contains("SubscriptionAdvanced"));
    }

    #[tokio::test]
    async fn older_or_empty_snapshot_retains_the_existing_consumer_floor() {
        for has_older_tip in [true, false] {
            let (_dir, airc, room) = fixture().await;
            let saved = event(&airc, room, 100, "codex:thread-1", "already seen").cursor();
            airc.coordinator_store_for_test()
                .save_runtime_cursor("retained", &saved, 1)
                .await
                .unwrap();
            if has_older_tip {
                airc.append_event(event(&airc, room, 50, "claude:other", "older room tip"))
                    .await
                    .unwrap();
            }
            let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:thread-1"));
            let mut output = Vec::new();
            let attached = attach_feed(&airc, "retained", &self_filter, &mut output)
                .await
                .unwrap();
            assert_eq!(attached.replayed_through, Some(saved.clone()));
            assert_eq!(
                airc.load_runtime_cursor("retained").await.unwrap(),
                Some(saved)
            );
            assert!(output.is_empty());

            let stale = event(&airc, room, 75, "claude:other", "older overlap");
            let fresh = event(&airc, room, 101, "claude:other", "fresh message");
            let expected = fresh.cursor();
            let mut stream = futures::stream::iter([Ok(Arc::new(stale)), Ok(Arc::new(fresh))]);
            print_stream_advancing_cursor(
                &airc,
                &mut stream,
                "retained",
                &self_filter,
                &mut output,
                attached.replayed_through,
            )
            .await
            .unwrap();
            assert_eq!(
                airc.load_runtime_cursor("retained").await.unwrap(),
                Some(expected)
            );
            let visible = String::from_utf8(output).unwrap();
            assert_eq!(visible.lines().count(), 1);
            assert!(visible.contains("fresh message"));
            assert!(!visible.contains("older overlap"));
        }
    }

    async fn fixture() -> (TempDir, Airc, RoomId) {
        let dir = TempDir::new().unwrap();
        // Both stores live in this temporary home. No daemon, real account
        // store, environment mutation, room join, or network route is involved.
        let airc = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let mut set = SubscriptionSet::empty();
        let channel = ChannelName::new("join-feed-test").unwrap();
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

    fn event(
        airc: &Airc,
        room_id: RoomId,
        lamport: u64,
        runtime: &str,
        text: &str,
    ) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id,
            peer_id: airc.peer_id(),
            client_id: airc.client_id(),
            kind: TranscriptKind::Message,
            occurred_at_ms: lamport,
            lamport,
            target: MentionTarget::All,
            headers: Headers::from([(HEADER_AIRC_CLIENT.to_string(), runtime.to_string())]),
            body: Some(Body::text(text)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }
}
