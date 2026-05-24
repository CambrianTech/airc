//! Cursor-backed live feed for `airc join`.
//!
//! `airc join` is the public recovery/live verb. Agent runtimes keep it
//! open as their event feed; scripts/tests let it return. This module
//! keeps the feed usable by storing a per-runtime cursor in
//! `airc-store` so each attach starts at "new since last seen" instead
//! of replaying the full transcript.

use airc_core::{Body, TranscriptEvent};
use airc_lib::{Airc, EventFilter, LiveLag};
use futures::stream::StreamExt;
use std::sync::Arc;

const CONSUMER_PREFIX: &str = "join-feed";
const CATCH_UP_LIMIT: usize = 64;

pub async fn run(airc: &Airc) -> Result<(), Box<dyn std::error::Error>> {
    let filter = EventFilter::default();
    let consumer_id = consumer_id()?;
    print_catch_up(airc, filter.clone(), &consumer_id).await?;
    println!();
    println!("attached — Ctrl-C to detach.");
    let mut stream = airc.subscribe_subscribed_filtered(filter).await?;
    print_stream_advancing_cursor(airc, &mut stream, &consumer_id).await
}

async fn print_catch_up(
    airc: &Airc,
    filter: EventFilter,
    consumer_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match airc.load_runtime_cursor(consumer_id).await? {
        Some(cursor) => {
            let events = airc
                .resume_from_subscribed_filtered(&cursor, filter, CATCH_UP_LIMIT)
                .await?;
            for event in &events {
                print_event(event);
            }
            if let Some(newest) = events.last() {
                airc.save_runtime_cursor_for_event(consumer_id, newest)
                    .await?;
            }
            if events.len() == CATCH_UP_LIMIT {
                eprintln!(
                    "airc: join feed catch-up capped at {CATCH_UP_LIMIT}; older unread remains in transcript"
                );
            }
        }
        None => {
            let events = airc.page_recent_subscribed_filtered(filter, 1).await?;
            if let Some(newest) = events.last() {
                airc.save_runtime_cursor_for_event(consumer_id, newest)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn print_stream_advancing_cursor<S>(
    airc: &Airc,
    stream: &mut S,
    consumer_id: &str,
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
                        print_event(&event);
                        airc.save_runtime_cursor_for_event(consumer_id, &event).await?;
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

fn consumer_id() -> Result<String, Box<dyn std::error::Error>> {
    let suffix = match crate::client_id::current_client_id()? {
        Some(client_id) => client_id,
        None => "default".to_string(),
    };
    Ok(format!("{CONSUMER_PREFIX}:{suffix}"))
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

#[cfg(test)]
mod tests {
    use super::CONSUMER_PREFIX;

    #[test]
    fn consumer_prefix_names_join_feed_checkpoints() {
        assert_eq!(CONSUMER_PREFIX, "join-feed");
    }
}
