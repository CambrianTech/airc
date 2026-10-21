//! Cursor-backed live feed for `airc join`.
//!
//! `airc join` is the public recovery/live verb. Agent runtimes keep it
//! open as their event feed; scripts/tests let it return. This module
//! keeps the feed usable by storing a per-runtime cursor in
//! `airc-store` so each attach starts at "new since last seen" instead
//! of replaying the full transcript.

use airc_core::{TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, LiveLag};
use futures::stream::StreamExt;
use std::io::Write;

use crate::client_id::{current_client_id, RuntimeSelfFilter};

const CONSUMER_PREFIX: &str = "join-feed";
const CATCH_UP_LIMIT: usize = 64;

pub async fn run(airc: &Airc) -> Result<(), Box<dyn std::error::Error>> {
    let runtime_client = current_client_id()?;
    let consumer_id = consumer_id(runtime_client.as_deref());
    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let mut output = std::io::stdout();
    // Native Monitor keeps its supported subscription; notifications wake bounded
    // durable scans rather than directly advancing over unseen live predecessors.
    let mut stream = airc.subscribe_agent_feed().await?;
    println!("attached — Ctrl-C to detach.");
    loop {
        let pending = print_delivery_turn(airc, &consumer_id, &self_filter, &mut output).await?;
        if pending {
            tokio::task::yield_now().await;
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(if pending { std::time::Duration::from_millis(1) } else { std::time::Duration::from_secs(1) }) => {},
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
) -> Result<bool, Box<dyn std::error::Error>> {
    let turn = airc
        .prepare_agent_delivery(consumer, CATCH_UP_LIMIT, CATCH_UP_LIMIT)
        .await?;
    let live_end = turn.live_events().len().min(turn.live_budget);
    let backlog_end = turn.backlog_events().len().min(turn.backlog_budget);
    for event in &turn.live_events()[..live_end] {
        print_event(event, self_filter, output)?;
    }
    if backlog_end > 0 {
        writeln!(output, "historical catch-up:")?;
    }
    for event in &turn.backlog_events()[..backlog_end] {
        print_event(event, self_filter, output)?;
    }
    output.flush()?;
    let pending = turn.has_pending(live_end, backlog_end);
    turn.acknowledge(live_end, backlog_end).await?;
    Ok(pending)
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
    async fn native_turn_delivers_fresh_before_backlog_and_failed_output_does_not_ack() {
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
            print_delivery_turn(&airc, "native-new", &self_filter, &mut Broken)
                .await
                .is_err()
        );
        let mut output = Vec::new();
        assert!(
            print_delivery_turn(&airc, "native-new", &self_filter, &mut output)
                .await
                .unwrap()
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.find("fresh-request").unwrap() < text.find("old-1").unwrap());
        assert!(!text.contains("old-200"));
        let mut next = Vec::new();
        print_delivery_turn(&airc, "native-new", &self_filter, &mut next)
            .await
            .unwrap();
        assert!(!String::from_utf8(next).unwrap().contains("fresh-request"));
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
        let mut warning = event(&airc, room, 132, "claude:peer", "ordinary warning");
        warning.kind = TranscriptKind::System;
        airc.append_event(warning).await.unwrap();
        let filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:local"));
        let mut output = Vec::new();
        assert!(
            !print_delivery_turn(&airc, "filtered", &filter, &mut output)
                .await
                .unwrap()
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("ordinary warning"));
        assert!(!text.contains("fragment"));
        assert!(!text.contains("own hidden"));
        let mut retry = Vec::new();
        assert!(!print_delivery_turn(&airc, "filtered", &filter, &mut retry)
            .await
            .unwrap());
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
