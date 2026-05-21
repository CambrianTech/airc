//! `airc events ...` handlers.

use std::path::Path;

use airc_lib::{Airc, Body, EventFilter, HeaderFilter, TranscriptEvent, TranscriptKind};

use crate::events_cli::CliTranscriptKind;

pub async fn run_list(
    home: &Path,
    kinds: Vec<CliTranscriptKind>,
    exact_headers: Vec<String>,
    prefix_headers: Vec<String>,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let filter = EventFilter {
        kinds: kinds.into_iter().map(TranscriptKind::from).collect(),
        headers_filter: parse_header_filters(exact_headers, prefix_headers)?,
        ..EventFilter::default()
    };
    let events = airc.page_recent_subscribed_filtered(filter, limit).await?;
    print_events(&events);
    Ok(())
}

fn print_events(events: &[TranscriptEvent]) {
    if events.is_empty() {
        println!("(no matching events)");
        return;
    }

    println!("events: {}", events.len());
    for event in events {
        let body = event
            .body
            .as_ref()
            .map(format_body)
            .unwrap_or_else(|| "<empty body>".to_string());
        println!(
            "{event_id}  {kind:?}  room={room}  peer={peer}  lamport={lamport}  body={body}",
            event_id = event.event_id,
            kind = event.kind,
            room = event.room_id,
            peer = event.peer_id,
            lamport = event.lamport,
        );
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

fn parse_header_filters(
    exact: Vec<String>,
    prefix: Vec<String>,
) -> Result<HeaderFilter, Box<dyn std::error::Error>> {
    let mut filters = Vec::new();
    for item in exact {
        let (key, value) = split_header_filter(&item)?;
        filters.push(HeaderFilter::Exact { key, value });
    }
    for item in prefix {
        let (key, value_prefix) = split_header_filter(&item)?;
        filters.push(HeaderFilter::Prefix { key, value_prefix });
    }
    Ok(match filters.len() {
        0 => HeaderFilter::Any,
        1 => filters.remove(0),
        _ => HeaderFilter::All(filters),
    })
}

fn split_header_filter(input: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let (key, value) = input
        .split_once('=')
        .ok_or_else(|| format!("header filter {input:?} must be KEY=VALUE"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(format!("header filter {input:?} has empty key").into());
    }
    Ok((key.to_string(), value.to_string()))
}

impl From<CliTranscriptKind> for TranscriptKind {
    fn from(value: CliTranscriptKind) -> Self {
        match value {
            CliTranscriptKind::Message => Self::Message,
            CliTranscriptKind::Attachment => Self::Attachment,
            CliTranscriptKind::Receipt => Self::Receipt,
            CliTranscriptKind::Presence => Self::Presence,
            CliTranscriptKind::SessionControl => Self::SessionControl,
            CliTranscriptKind::System => Self::System,
        }
    }
}
