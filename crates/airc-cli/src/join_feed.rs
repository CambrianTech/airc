//! Cursor-backed live feed for `airc join`.
//!
//! `airc join` is the public recovery/live verb. Agent runtimes keep it
//! open as their event feed; scripts/tests let it return. This module
//! keeps the feed usable by storing a per-runtime cursor so each attach
//! starts at "new since last seen" instead of replaying the full
//! transcript.

use std::path::{Path, PathBuf};

use airc_core::{Body, TranscriptCursor, TranscriptEvent};
use airc_lib::{Airc, EventFilter, LiveLag};
use futures::stream::StreamExt;

const CURSOR_PREFIX: &str = "join_feed_cursor";
const CATCH_UP_LIMIT: usize = 64;

pub async fn run(airc: &Airc, home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let filter = EventFilter::default();
    let cursor_path = cursor_path(home)?;
    print_catch_up(airc, filter.clone(), &cursor_path).await?;
    println!();
    println!("attached — Ctrl-C to detach.");
    let mut stream = airc.subscribe_subscribed_filtered(filter).await?;
    print_stream_advancing_cursor(&mut stream, &cursor_path).await
}

async fn print_catch_up(
    airc: &Airc,
    filter: EventFilter,
    cursor_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match read_cursor(cursor_path)? {
        Some(cursor) => {
            let events = airc
                .resume_from_subscribed_filtered(&cursor, filter, CATCH_UP_LIMIT)
                .await?;
            for event in &events {
                print_event(event);
            }
            if let Some(newest) = events.last().map(TranscriptEvent::cursor) {
                write_cursor(cursor_path, &newest)?;
            }
            if events.len() == CATCH_UP_LIMIT {
                eprintln!(
                    "airc: join feed catch-up capped at {CATCH_UP_LIMIT}; older unread remains in transcript"
                );
            }
        }
        None => {
            let events = airc.page_recent_subscribed_filtered(filter, 1).await?;
            if let Some(newest) = events.last().map(TranscriptEvent::cursor) {
                write_cursor(cursor_path, &newest)?;
            }
        }
    }
    Ok(())
}

async fn print_stream_advancing_cursor<S>(
    stream: &mut S,
    cursor_path: &Path,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: futures::stream::Stream<Item = Result<TranscriptEvent, LiveLag>> + Unpin,
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
                        print_event(&event);
                        write_cursor(cursor_path, &event.cursor())?;
                    }
                    Some(Err(lag)) => {
                        eprintln!("{lag}");
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

fn cursor_path(home: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let suffix = match crate::client_id::current_client_id()? {
        Some(client_id) => safe_filename_component(&client_id),
        None => "default".to_string(),
    };
    Ok(home.join(format!("{CURSOR_PREFIX}.{suffix}.json")))
}

fn read_cursor(path: &Path) -> Result<Option<TranscriptCursor>, Box<dyn std::error::Error>> {
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
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(cursor)?)?;
    replace_file(&tmp, path)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    match std::fs::rename(tmp, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(path)?;
            std::fs::rename(tmp, path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(windows))]
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, path)
}

fn print_event(event: &TranscriptEvent) {
    let text = event
        .body
        .as_ref()
        .and_then(Body::as_text)
        .unwrap_or("<non-text body>");
    println!(
        "[{kind:?}] {sender} → {channel}: {text}",
        kind = event.kind,
        sender = event.peer_id,
        channel = event.room_id,
    );
}

fn safe_filename_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::safe_filename_component;

    #[test]
    fn cursor_filename_is_safe_for_runtime_client_ids() {
        assert_eq!(
            safe_filename_component("codex:019dea28/with spaces"),
            "codex-019dea28-with-spaces"
        );
    }
}
