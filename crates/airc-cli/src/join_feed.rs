//! Cursor-backed live feed for `airc join`.
//!
//! `airc join` is the public recovery/live verb. Agent runtimes keep it
//! open as their event feed; scripts/tests let it return. This module
//! keeps the feed usable by storing a per-runtime cursor in
//! `airc-store` so each attach starts at "new since last seen" instead
//! of replaying the full transcript.

use airc_core::{MentionTarget, PeerId, RoomId, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, LiveLag};
use futures::stream::StreamExt;
use std::io::Write;

use crate::client_id::{current_client_id, RuntimeSelfFilter};

const CONSUMER_PREFIX: &str = "join-feed";
const CATCH_UP_LIMIT: usize = 64;

pub async fn run(airc: &Airc, room: RoomId) -> Result<(), Box<dyn std::error::Error>> {
    let runtime_client = current_client_id()?;
    let consumer_id = format!("{}:room:{room}", consumer_id(runtime_client.as_deref()));
    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let mut output = std::io::stdout();
    // Native Monitor keeps its supported subscription; notifications wake bounded
    // durable scans rather than directly advancing over unseen live predecessors.
    let mut stream = airc.subscribe_agent_feed().await?;
    println!("attached — Ctrl-C to detach.");
    let mut announce_history = true;
    loop {
        print_delivery_turn(
            airc,
            &consumer_id,
            &self_filter,
            &mut output,
            announce_history,
            Some(room),
        )
        .await?;
        announce_history = false;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
            next = stream.next() => match next {
                Some(Ok(_)) => {},
                Some(Err(lag @ LiveLag { .. })) => eprintln!("airc: {lag}; recovering from durable delivery state"),
                None => return Err("agent live subscription closed; checkpoints retained".into()),
            },
        }
    }
}
async fn print_delivery_turn(
    airc: &Airc,
    consumer: &str,
    self_filter: &RuntimeSelfFilter<'_>,
    output: &mut impl Write,
    announce_history: bool,
    room: Option<RoomId>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let turn = match room {
        Some(_) => {
            airc.prepare_native_delivery(consumer, announce_history, CATCH_UP_LIMIT, CATCH_UP_LIMIT)
                .await?
        }
        None => {
            airc.prepare_agent_delivery(consumer, CATCH_UP_LIMIT, CATCH_UP_LIMIT)
                .await?
        }
    };
    let live_end = turn.live_events().len().min(turn.live_budget);
    let mut ambient = 0usize;
    let mut ambient_rooms = std::collections::HashSet::new();
    for event in &turn.live_events()[..live_end] {
        if room.is_none_or(|focus| foreground(event, focus, airc.peer_id())) {
            print_event(event, self_filter, output)?;
        } else {
            ambient += 1;
            ambient_rooms.insert(event.room_id);
        }
    }
    if ambient > 0 {
        writeln!(output, "Ambient activity: {ambient} events in {} other rooms; recover with `airc inbox --room <room>`. Bodies remain in history.", ambient_rooms.len())?;
    }
    // History remains durable and unacknowledged. Do not replay old requests
    // into a native live feed: announce the bounded page once per attachment,
    // and let the agent deliberately recover history with the supported reader.
    if announce_history {
        if let Some(first) = turn.backlog_events().first() {
            writeln!(
                output,
                "Retained history (not live): at least {} retained events; first scanned timestamp {} ms since Unix epoch. Read deliberately in this same AIRC home with `airc inbox --all --json`; page a room with `airc inbox --room <room> --since-lamport <lamport> --since-event-id <event-id>`. History has not been acknowledged.",
                turn.backlog_events().len(),
                first.occurred_at_ms,
            )?;
        }
    }
    output.flush()?;
    let pending = turn.has_pending(live_end, 0);
    turn.acknowledge(live_end, 0).await?;
    Ok(pending)
}

fn foreground(event: &TranscriptEvent, focus: RoomId, peer: PeerId) -> bool {
    event.room_id == focus || matches!(event.target, MentionTarget::Peer(target) if target == peer)
}

fn consumer_id(runtime_client: Option<&str>) -> String {
    let suffix = runtime_client.unwrap_or("default");
    format!("{CONSUMER_PREFIX}:{suffix}")
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
    use airc_core::{Body, EventId, Headers, MentionTarget, RoomId, TranscriptKind};
    use airc_lib::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
    use airc_protocol::HEADER_AIRC_CLIENT;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn native_feed_keeps_cross_room_direct_messages_and_summarizes_ambient() {
        let (_dir, airc, room) = fixture().await;
        let mut set = airc.subscription_set().await.unwrap();
        let other = set
            .subscribe_with_wire_root(
                airc.wire_root(),
                &MeshIdentity::new("test"),
                ChannelName::new("elsewhere").unwrap(),
            )
            .unwrap()
            .room_id;
        subscriptions::save(airc.coordinator_store_for_test(), &set)
            .await
            .unwrap();
        let filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:local"));
        print_delivery_turn(&airc, "focused", &filter, &mut Vec::new(), true, Some(room))
            .await
            .unwrap();
        airc.append_event(event(&airc, other, 1, "claude:peer", "ambient-body"))
            .await
            .unwrap();
        let mut directed = event(&airc, other, 2, "claude:peer", "direct-challenge");
        directed.target = MentionTarget::Peer(airc.peer_id());
        airc.append_event(directed).await.unwrap();
        airc.append_event(event(&airc, room, 3, "claude:peer", "room-message"))
            .await
            .unwrap();
        let mut output = Vec::new();
        print_delivery_turn(&airc, "focused", &filter, &mut output, false, Some(room))
            .await
            .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("direct-challenge"));
        assert!(text.contains("room-message"));
        assert!(!text.contains("ambient-body"));
        assert!(text.contains("Ambient activity: 1 events in 1 other rooms"));
    }
    #[tokio::test]
    async fn native_turn_announces_history_after_fresh_and_failed_output_does_not_ack() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=200 {
            airc.append_event(event(&airc, room, n, "claude:peer", &format!("old-{n}")))
                .await
                .unwrap();
        }
        drop(
            airc.prepare_agent_delivery("native-new", 64, 64)
                .await
                .unwrap(),
        );
        airc.append_event(event(&airc, room, 201, "claude:peer", "fresh-request"))
            .await
            .unwrap();
        let self_filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:local"));
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected output failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(
            print_delivery_turn(&airc, "native-new", &self_filter, &mut Broken, true, None)
                .await
                .is_err()
        );
        let mut output = Vec::new();
        assert!(
            print_delivery_turn(&airc, "native-new", &self_filter, &mut output, true, None)
                .await
                .unwrap()
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.find("fresh-request").unwrap() < text.find("Retained history").unwrap());
        assert!(!text.contains("old-200"));
        let mut next = Vec::new();
        print_delivery_turn(&airc, "native-new", &self_filter, &mut next, true, None)
            .await
            .unwrap();
        assert!(!String::from_utf8(next).unwrap().contains("fresh-request"));
    }

    #[tokio::test]
    async fn history_is_announced_once_and_retained_while_fresh_delivery_continues() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=3 {
            airc.append_event(event(&airc, room, n, "claude:peer", &format!("old-{n}")))
                .await
                .unwrap();
        }
        let filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:local"));
        let mut output = Vec::new();
        print_delivery_turn(&airc, "paced", &filter, &mut output, true, None)
            .await
            .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("at least 3 retained events"));
        assert!(!text.contains("old-2"));
        airc.append_event(event(&airc, room, 4, "claude:peer", "fresh-request"))
            .await
            .unwrap();
        let mut fresh = Vec::new();
        print_delivery_turn(&airc, "paced", &filter, &mut fresh, false, None)
            .await
            .unwrap();
        let text = String::from_utf8(fresh).unwrap();
        assert!(text.contains("fresh-request"));
        assert!(!text.contains("old-"));
        let mut resumed = Vec::new();
        print_delivery_turn(&airc, "paced", &filter, &mut resumed, false, None)
            .await
            .unwrap();
        let text = String::from_utf8(resumed).unwrap();
        assert!(text.is_empty());
        let retained = airc.prepare_agent_delivery("paced", 64, 64).await.unwrap();
        assert_eq!(retained.backlog_events().len(), 3);
        assert!(!text.contains("old-1"));
        assert!(!text.contains("old-3"));
    }

    #[test]
    fn consumer_id_names_runtime_checkpoints() {
        assert_eq!(
            consumer_id(Some("codex:thread-1")),
            "join-feed:codex:thread-1"
        );
        assert_eq!(consumer_id(None), "join-feed:default");
    }

    #[tokio::test]
    async fn native_delivery_filters_self_and_streams_without_skipping_warning() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=130 {
            let mut chunk = event(&airc, room, n, "claude:peer", "fragment");
            chunk.kind = TranscriptKind::System;
            chunk
                .headers
                .insert(airc_lib::HEADER_STREAM_ID.into(), "stream-test".into());
            airc.append_event(chunk).await.unwrap();
        }
        airc.append_event(event(&airc, room, 131, "codex:local", "own hidden"))
            .await
            .unwrap();
        drop(
            airc.prepare_agent_delivery("filtered", 64, 64)
                .await
                .unwrap(),
        );
        let mut warning = event(&airc, room, 132, "claude:peer", "ordinary warning");
        warning.kind = TranscriptKind::System;
        airc.append_event(warning).await.unwrap();
        let filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:local"));
        let mut output = Vec::new();
        assert!(
            print_delivery_turn(&airc, "filtered", &filter, &mut output, true, None)
                .await
                .unwrap()
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("ordinary warning"));
        assert!(!text.contains("fragment"));
        assert!(!text.contains("own hidden"));
        let mut retry = Vec::new();
        assert!(
            print_delivery_turn(&airc, "filtered", &filter, &mut retry, false, None)
                .await
                .unwrap()
        );
        assert!(retry.is_empty());
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
