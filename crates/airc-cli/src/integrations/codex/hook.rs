//! `airc codex-hook ...` handlers.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use airc_core::{Body, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, EventFilter, LiveLag};
use airc_protocol::HEADER_AIRC_CLIENT;
use futures::StreamExt;
use serde::Serialize;

use crate::client_id::current_client_id;
use crate::work_suggestions::{is_work_queue_event, render_claimable_work};

const CONSUMER_PREFIX: &str = "codex-hook";

pub async fn run_user_prompt_submit(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    drain_stdin()?;

    let airc = Airc::open(home).await?;
    let filter = hook_filter();
    let runtime_client = current_client_id()?;
    let consumer_id = consumer_id(runtime_client.as_deref());
    let events = unread_events(&airc, &consumer_id, filter, count).await?;

    if let Some(newest) = events.last() {
        airc.save_runtime_cursor_for_event(&consumer_id, newest)
            .await?;
    }

    let work_context = work_context(&airc, &events).await?;
    let visible: Vec<_> = events
        .into_iter()
        .filter(|event| include_self || !is_self_event(event, &airc, runtime_client.as_deref()))
        .collect();
    let Some(context) = render_context(&airc, &visible, max_items, raw, work_context).await? else {
        return Ok(());
    };
    print_hook_payload(&context)?;
    Ok(())
}

pub async fn run_poll(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
    wait_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let runtime_client = current_client_id()?;
    let consumer_id = consumer_id(runtime_client.as_deref());
    let filter = hook_filter();

    let mut events = unread_events(&airc, &consumer_id, filter.clone(), count).await?;
    if events.is_empty() && wait_ms > 0 {
        events = wait_for_one_event(&airc, filter.clone(), wait_ms).await?;
        if events.is_empty() {
            events = unread_events(&airc, &consumer_id, filter, count).await?;
        }
    }

    if let Some(newest) = events.last() {
        airc.save_runtime_cursor_for_event(&consumer_id, newest)
            .await?;
    }

    let work_context = work_context(&airc, &events).await?;
    let visible: Vec<_> = events
        .into_iter()
        .filter(|event| include_self || !is_self_event(event, &airc, runtime_client.as_deref()))
        .collect();
    let Some(context) = render_context(&airc, &visible, max_items, raw, work_context).await? else {
        return Ok(());
    };
    println!("{context}");
    Ok(())
}

async fn work_context(
    airc: &Airc,
    events: &[TranscriptEvent],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if events.iter().any(is_work_queue_event) {
        render_claimable_work(airc).await
    } else {
        Ok(None)
    }
}

async fn render_context(
    _airc: &Airc,
    events: &[TranscriptEvent],
    max_items: usize,
    raw: bool,
    work_context: Option<String>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let chat_context = if raw {
        let text = render_raw(events);
        (!text.is_empty()).then_some(text)
    } else {
        let text = render_digest(events, max_items);
        (!text.is_empty()).then_some(text)
    };
    let mut sections = Vec::new();
    if let Some(context) = chat_context {
        sections.push(context);
    }
    if let Some(context) = work_context {
        sections.push(context);
    }
    Ok((!sections.is_empty()).then(|| sections.join("\n\n")))
}

fn consumer_id(runtime_client: Option<&str>) -> String {
    match runtime_client {
        Some(client) => format!("{CONSUMER_PREFIX}:{client}"),
        None => format!("{CONSUMER_PREFIX}:default"),
    }
}

fn hook_filter() -> EventFilter {
    EventFilter {
        kinds: BTreeSet::from([TranscriptKind::Message, TranscriptKind::System]),
        ..EventFilter::default()
    }
}

async fn unread_events(
    airc: &Airc,
    consumer_id: &str,
    filter: EventFilter,
    count: usize,
) -> Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>> {
    let previous = airc.load_runtime_cursor(consumer_id).await?;
    let events = match previous {
        Some(cursor) => {
            airc.resume_from_subscribed_filtered(&cursor, filter, count)
                .await?
        }
        None => airc.page_recent_subscribed_filtered(filter, count).await?,
    };
    Ok(events)
}

async fn wait_for_one_event(
    airc: &Airc,
    filter: EventFilter,
    wait_ms: u64,
) -> Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe_subscribed_filtered(filter).await?;
    match tokio::time::timeout(Duration::from_millis(wait_ms), stream.next()).await {
        Ok(Some(Ok(event))) => Ok(vec![event.as_ref().clone()]),
        Ok(Some(Err(LiveLag { .. }))) | Ok(None) | Err(_) => Ok(Vec::new()),
    }
}

fn is_self_event(event: &TranscriptEvent, airc: &Airc, runtime_client: Option<&str>) -> bool {
    // Header-stamped self-detection is the only reliable signal on a
    // shared HOME. Two scopes (Claude tab + Codex tab) using the
    // same `~/.airc/` share the singleton local identity row →
    // share peer_id AND client_id. The peer_id/client_id equality
    // check would incorrectly classify EVERY frame from the other
    // runtime as "self" and filter it out.
    //
    // Rule: if the event has an `airc.client` header, that IS the
    // self attestation. Equal to OUR runtime client tag → self.
    // Different (or our tag absent) → NOT self. Peer_id never
    // overrides a stamped header on a shared HOME.
    if let Some(event_client) = event.headers.get(HEADER_AIRC_CLIENT) {
        return runtime_client.is_some_and(|rc| event_client == rc);
    }
    // No header: unstamped historical frame. Drop the peer_id check
    // entirely (Codex's review on PR #869): peer_id is too coarse
    // for shared-HOME multi-agent operation — every cross-runtime
    // frame would be suppressed. client_id alone is still subject
    // to identity-collision on shared HOME, but it's a tighter
    // signal than peer_id. Current Rust-emitted frames always stamp
    // the airc.client header in send_frame, so this branch only
    // fires for historical frames where false-self
    // suppression is the lesser harm.
    event.client_id == airc.client_id()
}

fn drain_stdin() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    if raw.trim().is_empty() {
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err("Codex hook stdin must be a JSON object".into());
    }
    Ok(())
}

fn print_hook_payload(context: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HookPayload<'a> {
        hook_specific_output: HookSpecificOutput<'a>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HookSpecificOutput<'a> {
        hook_event_name: &'a str,
        additional_context: &'a str,
    }

    let payload = HookPayload {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "UserPromptSubmit",
            additional_context: context,
        },
    };
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}

fn render_raw(events: &[TranscriptEvent]) -> String {
    events
        .iter()
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

fn render_digest(events: &[TranscriptEvent], max_items: usize) -> String {
    let messages = dedupe(events);
    if messages.is_empty() {
        return String::new();
    }

    let max_items = max_items.max(1);
    let hidden = messages.len().saturating_sub(max_items);
    let shown = &messages[messages.len().saturating_sub(max_items)..];
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
            "latest {} shown; {hidden} older omitted",
            shown.len()
        ));
    }
    for message in shown {
        lines.push(format!(
            "- {}: {}",
            message.peer,
            summarize_text(&message.body, 120)
        ));
    }
    if hidden > 0 {
        lines.push("more: airc codex-hook user-prompt-submit --raw".to_string());
    }
    lines.join("\n")
}

#[derive(Clone)]
struct DigestMessage {
    peer: String,
    body: String,
}

fn dedupe(events: &[TranscriptEvent]) -> Vec<DigestMessage> {
    let mut seen = BTreeSet::new();
    let mut messages = Vec::new();
    for event in events {
        if is_work_queue_event(event) {
            continue;
        }
        let peer = event.peer_id.to_string();
        let body = summarize_body(event);
        if seen.insert((peer.clone(), body.clone())) {
            messages.push(DigestMessage { peer, body });
        }
    }
    messages
}

fn summarize_body(event: &TranscriptEvent) -> String {
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

fn summarize_text(value: &str, max_len: usize) -> String {
    let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= max_len {
        return one_line;
    }
    format!("{}...", one_line[..max_len.saturating_sub(3)].trim_end())
}

#[cfg(test)]
mod tests {
    use super::consumer_id;

    #[test]
    fn consumer_id_uses_runtime_client_when_present() {
        assert_eq!(
            consumer_id(Some("codex:thread-1")),
            "codex-hook:codex:thread-1"
        );
        assert_eq!(consumer_id(None), "codex-hook:default");
    }
}
