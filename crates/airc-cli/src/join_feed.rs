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

pub async fn run(
    airc: &Airc,
    room: RoomId,
    home: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
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
        let turn = print_delivery_turn(
            airc,
            &consumer_id,
            &self_filter,
            &mut output,
            announce_history,
            Some(room),
        )
        .await;
        if let Err(error) = turn {
            // Decided by the error's type, never by probing afterwards: a replacement daemon
            // can answer a probe before the old one's failure is handled (Codex on #1603).
            if !daemon_absent(error.as_ref()) {
                return Err(error);
            }
            stream = reattach(airc, home).await?;
            continue;
        }
        announce_history = false;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
            next = stream.next() => match next {
                Some(Ok(_)) => {},
                Some(Err(lag @ LiveLag { .. })) => eprintln!("airc: {lag}; recovering from durable delivery state"),
                None => stream = reattach(airc, home).await?,
            },
        }
    }
}

/// The daemon was not there to answer (`AircError::is_daemon_absent`): a daemon between
/// processes. Output, store and protocol errors are not this, and end the feed.
fn daemon_absent(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<airc_lib::AircError>()
        .is_some_and(airc_lib::AircError::is_daemon_absent)
}

/// Wait out the daemon's absence and subscribe again (card 201417d4). A supported update
/// hands the daemon over in seconds, and that used to end every attached feed (Claude
/// Monitor, a Codex join session) until someone re-ran `airc join`. Delivery resumes from
/// this consumer's durable checkpoint, so nothing published in the gap is lost. Delivery is
/// at-least-once across the gap: a turn flushes its output before it acknowledges, so an
/// event printed just before the daemon went away can print again. Only an operator stop
/// ends the feed here: it is the one absence meant to last.
async fn reattach(
    airc: &Airc,
    home: &std::path::Path,
) -> Result<airc_lib::FilteredEventStream, Box<dyn std::error::Error>> {
    eprintln!("airc join: the daemon went away; waiting for it to return (an update hands it over in seconds)");
    let mut wait = std::time::Duration::from_millis(250);
    loop {
        if airc_lib::daemon_lifecycle::operator_stop_recorded(home)? {
            return Err("the daemon was stopped by the operator; run `airc join` to resume".into());
        }
        match airc.subscribe_agent_feed().await {
            Ok(stream) => {
                eprintln!("airc join: reattached; delivery resumes from this feed's checkpoint");
                return Ok(stream);
            }
            Err(error) if error.is_daemon_absent() => {}
            Err(error) => return Err(error.into()),
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Err("detached while waiting for the daemon".into()),
            _ = tokio::time::sleep(wait) => {},
        }
        wait = (wait * 2).min(std::time::Duration::from_secs(5));
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
        // Exercise the public publish path, not a hand-crafted transcript target.
        // A different runtime represents a peer session on this shared account.
        airc.publish(
            airc_lib::PublishTarget::RoomByName(other.to_string()).to_peer(airc.peer_id()),
            airc_protocol::FrameKind::Message,
            Body::text("direct-challenge"),
            Headers::from([(HEADER_AIRC_CLIENT.to_string(), "claude:peer".to_string())]),
        )
        .await
        .unwrap();
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

    // what this catches (card 201417d4): the wait that keeps a feed alive across a daemon
    // handoff swallowing the one absence meant to last. With an operator stop recorded and no
    // daemon answering, the feed ends with the resume instruction instead of waiting forever.
    #[tokio::test]
    async fn an_operator_stop_ends_the_wait_for_the_daemon() {
        let (dir, airc, _room) = fixture().await;
        let stop =
            airc_lib::daemon_lifecycle::DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        stop.record_operator_stop().unwrap();
        drop(stop);
        let ended = reattach(&airc, dir.path()).await.err().unwrap().to_string();
        assert!(ended.contains("stopped by the operator"), "{ended}");
    }

    // what this catches (Codex on #1603): ending the feed on a daemon handoff because a fast
    // replacement answered a probe taken after the failure. The decision is the error's own
    // type: a daemon that was not there (not connected, dropped mid-call, timed out) reattaches
    // whatever answers later; a refused request, an output error or a store error ends the feed.
    #[test]
    fn a_daemon_absence_reattaches_and_every_other_error_ends_the_feed() {
        use airc_lib::AircError;
        let boxed = |error: AircError| -> Box<dyn std::error::Error> { error.into() };
        let gone = || std::io::Error::from(std::io::ErrorKind::NotFound);
        for absent in [
            AircError::DaemonClient(airc_ipc::ClientError::NotConnected(gone())),
            AircError::DaemonClient(airc_ipc::ClientError::Io(gone())),
            AircError::DaemonClient(airc_ipc::ClientError::Timeout),
        ] {
            assert!(daemon_absent(boxed(absent).as_ref()));
        }
        for real in [
            boxed(AircError::DaemonClient(airc_ipc::ClientError::Daemon(
                "refused".into(),
            ))),
            boxed(AircError::Route(
                "agent delivery: invalid delivered prefix".into(),
            )),
            Box::new(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                as Box<dyn std::error::Error>,
        ] {
            assert!(!daemon_absent(real.as_ref()), "{real}");
        }
    }

    // what this catches (Codex on #1603): a reattach that loses what was published while the
    // daemon was away, or prints again what was already delivered and acknowledged. Delivery
    // resumes from this consumer's durable checkpoint: the turn after a reattach prints the
    // event from the gap, and not the one acknowledged before it.
    #[tokio::test]
    async fn a_reattach_resumes_at_the_checkpoint() {
        let (_dir, airc, room) = fixture().await;
        let filter = RuntimeSelfFilter::new(airc.client_id(), Some("codex:local"));
        print_delivery_turn(&airc, "resume", &filter, &mut Vec::new(), true, Some(room))
            .await
            .unwrap();
        airc.append_event(event(&airc, room, 1, "claude:peer", "before-the-gap"))
            .await
            .unwrap();
        let mut first = Vec::new();
        print_delivery_turn(&airc, "resume", &filter, &mut first, false, Some(room))
            .await
            .unwrap();
        assert!(String::from_utf8(first).unwrap().contains("before-the-gap"));
        airc.append_event(event(&airc, room, 2, "claude:peer", "during-the-gap"))
            .await
            .unwrap();
        let _stream = reattach(&airc, _dir.path()).await.unwrap();
        let mut after = Vec::new();
        print_delivery_turn(&airc, "resume", &filter, &mut after, false, Some(room))
            .await
            .unwrap();
        let after = String::from_utf8(after).unwrap();
        assert!(after.contains("during-the-gap"), "{after}");
        assert!(
            !after.contains("before-the-gap"),
            "acknowledged before the gap: {after}"
        );
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
