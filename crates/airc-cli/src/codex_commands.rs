//! `airc codex-hook ...` handlers.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use airc_core::{Body, TranscriptCursor, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, EventFilter};
use airc_protocol::HEADER_AIRC_CLIENT;
use serde::Serialize;

use crate::client_id::current_client_id;

const DEFAULT_CURSOR_FILENAME: &str = "codex_hook_cursor.json";

pub async fn run_user_prompt_submit(
    home: &Path,
    cursor_file: Option<PathBuf>,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    drain_stdin()?;

    let airc = Airc::open(home).await?;
    let cursor_path = cursor_file.unwrap_or_else(|| home.join(DEFAULT_CURSOR_FILENAME));
    let filter = EventFilter {
        kinds: BTreeSet::from([TranscriptKind::Message, TranscriptKind::System]),
        ..EventFilter::default()
    };
    let previous = read_cursor_if_present(&cursor_path)?;
    let events = match previous {
        Some(cursor) => {
            airc.resume_from_subscribed_filtered(&cursor, filter, count)
                .await?
        }
        None => airc.page_recent_subscribed_filtered(filter, count).await?,
    };

    if let Some(newest) = events.last().map(TranscriptEvent::cursor) {
        write_cursor(&cursor_path, &newest)?;
    }

    let runtime_client = current_client_id()?;
    let visible: Vec<_> = events
        .into_iter()
        .filter(|event| include_self || !is_self_event(event, &airc, runtime_client.as_deref()))
        .collect();
    if visible.is_empty() {
        return Ok(());
    }

    let context = if raw {
        render_raw(&visible)
    } else {
        render_digest(&visible, max_items)
    };
    print_hook_payload(&context)?;
    Ok(())
}

fn is_self_event(event: &TranscriptEvent, airc: &Airc, runtime_client: Option<&str>) -> bool {
    if let Some(runtime_client) = runtime_client {
        if let Some(event_client) = event.headers.get(HEADER_AIRC_CLIENT) {
            return event_client == runtime_client;
        }
    }
    event.client_id == airc.client_id() || event.peer_id == airc.peer_id()
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

fn read_cursor_if_present(
    path: &Path,
) -> Result<Option<TranscriptCursor>, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_cursor(path: &Path, cursor: &TranscriptCursor) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(cursor)?)?;
    std::fs::rename(tmp, path)?;
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
