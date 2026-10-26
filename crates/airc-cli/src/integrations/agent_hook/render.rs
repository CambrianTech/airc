//! Hook context rendering: the compact digest, raw lines, and the one-line
//! history summary. Runtime-neutral; the only runtime fact a line names is the
//! verb a reader continues with.

use airc_core::{Body, TranscriptEvent};
use airc_lib::HistorySummary;

use crate::work_suggestions::is_work_queue_event;

/// How many items one hook notification shows: the digest's page, and the
/// default of `--max-items`. A notification is read between steps, not browsed,
/// so it carries a screenful and names how to read the rest. Unchanged from the
/// digest's long-standing default.
pub(crate) const DIGEST_PAGE: usize = 8;

// Automatic context is a notification, not an export of nested transcripts. Keep
// large bodies in durable history; explicit --raw reads retain the full payload.
// 4 KiB is a chosen per-message notification budget, not a transport limit.
pub(super) const DIGEST_BODY_BYTES: usize = 4096;

/// Peer correspondence, as opposed to work-queue churn and liveness beats.
pub(super) fn is_correspondence(event: &TranscriptEvent) -> bool {
    !is_work_queue_event(event) && !is_routine_liveness_event(event)
}

/// An event's original time, to the minute in UTC. A line that shows an event
/// carries this so old history can never read as new (card ba22c72c).
pub(crate) fn original_time(occurred_at_ms: u64) -> String {
    i64::try_from(occurred_at_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|at| at.format("%Y-%m-%dT%H:%MZ").to_string())
        .unwrap_or_else(|| format!("{occurred_at_ms} ms since the Unix epoch"))
}

/// One line for history this consumer was never attached for: counted by the
/// store, not replayed, with the original times of its oldest and newest events.
/// `None` when nothing was counted.
pub(crate) fn history_line(summary: &HistorySummary) -> Option<String> {
    let counted = summary.counted?;
    let (Some(oldest), Some(newest)) = (counted.oldest_ms, counted.newest_ms) else {
        return None;
    };
    let shown = match summary.recent.len() {
        0 => String::new(),
        n => format!("; the newest {n} follow, the rest are"),
    };
    (counted.count > 0).then(|| {
        format!(
            "AIRC: {} earlier events from before this session attached ({} to {}){shown} not replayed; read them with `airc inbox --all --json`.",
            counted.count,
            original_time(oldest),
            original_time(newest),
        )
    })
}

/// The line after the addressed page: requests still to come. They are never
/// settled unseen; later calls page them until every one is shown. `None` when
/// this page finished the range.
pub(crate) fn more_addressed_line(summary: &HistorySummary) -> Option<String> {
    let more = summary.more_addressed;
    if summary.addressed_complete {
        return None;
    }
    Some(if more > 0 {
        format!("AIRC: {more} more addressed to you; they follow on the next calls.")
    } else {
        "AIRC: older requests addressed to you, if any, follow on the next calls.".to_string()
    })
}

/// History in a hook: the count, the newest page of room events whole (oldest
/// first, as a channel opens), then one page of the requests addressed to this
/// identity, newest first, then what the page left out. Every shown event
/// carries its original time; a direct request is never folded into the count.
pub(super) fn history_section(summary: &HistorySummary) -> Option<String> {
    let mut lines: Vec<String> = history_line(summary).into_iter().collect();
    lines.extend(
        digest_messages(&summary.recent)
            .iter()
            .map(|m| format!("- {} [{}]: {}", m.peer, m.at, m.body)),
    );
    let addressed = digest_messages(&summary.addressed);
    if !addressed.is_empty() {
        lines
            .push("AIRC: addressed to you before this session attached, newest first:".to_string());
        lines.extend(
            addressed
                .iter()
                .map(|m| format!("- {} [{}]: {}", m.peer, m.at, m.body)),
        );
    }
    lines.extend(more_addressed_line(summary));
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub(super) fn render_raw<'a>(events: impl IntoIterator<Item = &'a TranscriptEvent>) -> String {
    events
        .into_iter()
        .map(|event| {
            format!(
                "[{}] {}: {}",
                event.occurred_at_ms,
                event.peer_id,
                summarize_body(event)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn render_digest<'a>(
    events: impl IntoIterator<Item = &'a TranscriptEvent>,
    max_items: usize,
    verb: &str,
) -> String {
    let messages = digest_messages(events);
    if messages.is_empty() {
        return String::new();
    }

    let max_items = max_items.max(1);
    let hidden = messages.len().saturating_sub(max_items);
    let shown = &messages[..messages.len().min(max_items)];
    let mut peers = Vec::new();
    for message in &messages {
        if !peers.contains(&message.peer) {
            peers.push(message.peer.clone());
        }
    }

    let mut lines = Vec::new();
    let mut first = format!("AIRC: {} unread", messages.len());
    if !peers.is_empty() {
        let shown_peers = peers.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        first.push_str(" from ");
        first.push_str(&shown_peers);
        if peers.len() > 3 {
            first.push_str(&format!(" +{}", peers.len() - 3));
        }
    }
    lines.push(first);
    if hidden > 0 {
        lines.push(format!(
            "first {} shown; {hidden} remain pending",
            shown.len()
        ));
    }
    for message in shown {
        lines.push(format!(
            "- {} [{}]: {}",
            message.peer, message.at, message.body
        ));
    }
    if hidden > 0 {
        lines.push(format!("continue: airc {verb} poll"));
    }
    lines.join("\n")
}

struct DigestMessage {
    peer: String,
    at: String,
    body: String,
}

fn digest_messages<'a>(
    events: impl IntoIterator<Item = &'a TranscriptEvent>,
) -> Vec<DigestMessage> {
    let mut messages = Vec::new();
    for event in events {
        if !is_correspondence(event) {
            continue;
        }
        let peer = event.peer_id.to_string();
        let full = summarize_body(event);
        let body = if full.len() <= DIGEST_BODY_BYTES {
            full
        } else {
            format!(
                "[body retained: {} bytes; event {}; room {}; cursor {} / {}] Read in this same AIRC home with `airc inbox --room {} --since-lamport {} --since-event-id 00000000-0000-0000-0000-000000000000 --json`; page forward to this event ID if needed. This is a reference, not the message body.",
                full.len(), event.event_id, event.room_id, event.lamport, event.event_id,
                event.room_id, event.lamport.saturating_sub(1),
            )
        };
        messages.push(DigestMessage {
            peer,
            at: original_time(event.occurred_at_ms),
            body,
        });
    }
    messages
}

fn is_routine_liveness_event(event: &TranscriptEvent) -> bool {
    // Lease renewals are liveness, not agent correspondence. Keep the event in
    // the durable transcript/raw output, but do not spend the digest budget on it.
    if event
        .headers
        .get(airc_lib::HEADER_FORGE_WORK_EVENT_KIND)
        .map(String::as_str)
        == Some("claim_heartbeat")
    {
        return true;
    }
    let Some(body) = &event.body else {
        return false;
    };
    match body {
        Body::Binary(_) => false,
        Body::Json(value) => {
            body_kind_is_alive(value)
                || body.as_text().is_some_and(|text| {
                    serde_json::from_str::<serde_json::Value>(text)
                        .is_ok_and(|value| body_kind_is_alive(&value))
                })
        }
    }
}

fn body_kind_is_alive(value: &serde_json::Value) -> bool {
    value.get("kind").and_then(|kind| kind.as_str()) == Some("alive")
}

pub(super) fn summarize_body(event: &TranscriptEvent) -> String {
    match &event.body {
        Some(body) => format_body(body),
        None => "<empty body>".to_string(),
    }
}

fn format_body(body: &Body) -> String {
    if let Some(text) = body.as_text() {
        return text.to_string();
    }
    match body {
        Body::Json(value) => value.to_string(),
        Body::Binary(bytes) => format!("<binary {} bytes>", bytes.len()),
    }
}
