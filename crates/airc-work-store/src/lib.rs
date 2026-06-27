//! `airc-work-store` — projection queries over persisted AIRC events.
//!
//! This crate is deliberately thin: it depends on the generic
//! `airc-store::EventStore` trait, decodes recorded work-domain
//! transcripts through `airc-work`, and returns deterministic board
//! projections. No SQL, GitHub, CLI, hook, or Continuum policy lives
//! here.

#![deny(unsafe_code)]

use airc_core::{EventId, RoomId, TranscriptCursor, TranscriptEvent};
use airc_store::{EventStore, StoreError};
use airc_work::{
    decode_transcript_work_event, transcript_is_work_event, ProjectionError, WorkBoardProjection,
    WorkEvent, WorkReplayError,
};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkEventPage {
    pub events: Vec<WorkEvent>,
    pub newest_cursor: Option<TranscriptCursor>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkStoreError {
    #[error("event store failed: {0}")]
    Store(#[from] StoreError),
    #[error("work replay failed: {0}")]
    Replay(#[from] WorkReplayError),
    #[error("work projection failed: {0}")]
    Projection(#[from] ProjectionError),
}

pub struct WorkEventStore<'store, S: ?Sized> {
    store: &'store S,
}

impl<'store, S> WorkEventStore<'store, S>
where
    S: EventStore + ?Sized,
{
    pub fn new(store: &'store S) -> Self {
        Self { store }
    }

    pub async fn page_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<WorkEventPage, WorkStoreError> {
        let transcripts = self.store.page_recent(channel, limit).await?;
        decode_page(transcripts)
    }

    pub async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<WorkEventPage, WorkStoreError> {
        let transcripts = self.store.resume_from(cursor, channel, limit).await?;
        decode_page(transcripts)
    }

    pub async fn project_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<WorkBoardProjection, WorkStoreError> {
        let page = self.page_recent(channel, limit).await?;
        Ok(WorkBoardProjection::replay_window(page.events)?)
    }

    pub async fn project_from(
        &self,
        cursor: &TranscriptCursor,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<WorkBoardProjection, WorkStoreError> {
        let page = self.resume_from(cursor, channel, limit).await?;
        Ok(WorkBoardProjection::replay(page.events)?)
    }

    pub async fn project_complete(
        &self,
        channel: Option<RoomId>,
        page_size: usize,
    ) -> Result<WorkBoardProjection, WorkStoreError> {
        let page_size = page_size.max(1);
        let mut cursor = TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        };
        let mut events = Vec::new();

        loop {
            let transcripts = self.store.resume_from(&cursor, channel, page_size).await?;
            let transcript_count = transcripts.len();
            let Some(newest_cursor) = transcripts.last().map(TranscriptEvent::cursor) else {
                break;
            };
            let page = decode_page(transcripts)?;
            events.extend(page.events);
            cursor = newest_cursor;
            if transcript_count < page_size {
                break;
            }
        }

        Ok(WorkBoardProjection::replay_window(events)?)
    }
}

/// Project a complete work board from a caller-supplied transcript page
/// (e.g. events fetched via the daemon inbox) — no `EventStore` needed.
/// Same decode + replay as [`WorkEventStore::project_complete`], for the
/// daemon-attached read path.
pub fn project_transcripts(
    transcripts: Vec<TranscriptEvent>,
) -> Result<WorkBoardProjection, WorkStoreError> {
    let mut projection = WorkBoardProjection::new();
    apply_transcripts(&mut projection, transcripts)?;
    Ok(projection)
}

/// Apply a caller-supplied transcript page **incrementally** onto an
/// existing projection (card 1291173d: cached work-board resume).
/// Decode + apply rule is byte-identical to [`project_transcripts`] —
/// both funnel through `WorkBoardProjection::apply_windowed` — so
/// `project_transcripts(a ++ b)` ≡ `project_transcripts(a)` then
/// `apply_transcripts(b)`. Returns the cursor of the newest transcript
/// event consumed (work event or not), i.e. the resume point for the
/// next increment.
pub fn apply_transcripts(
    projection: &mut WorkBoardProjection,
    transcripts: Vec<TranscriptEvent>,
) -> Result<Option<TranscriptCursor>, WorkStoreError> {
    let page = decode_page(transcripts)?;
    for event in &page.events {
        projection.apply_windowed(event)?;
    }
    Ok(page.newest_cursor)
}

fn decode_page(transcripts: Vec<TranscriptEvent>) -> Result<WorkEventPage, WorkStoreError> {
    let newest_cursor = transcripts.last().map(TranscriptEvent::cursor);
    let mut events = Vec::new();

    for transcript in transcripts {
        if !transcript_is_work_event(&transcript) {
            continue;
        }
        let item = decode_transcript_work_event(&transcript)?;
        events.push(item.event);
    }

    Ok(WorkEventPage {
        events,
        newest_cursor,
    })
}

#[cfg(test)]
mod tests;
