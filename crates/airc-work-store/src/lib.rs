//! `airc-work-store` — projection queries over persisted AIRC events.
//!
//! This crate is deliberately thin: it depends on the generic
//! `airc-store::EventStore` trait, decodes recorded work-domain
//! transcripts through `airc-work`, and returns deterministic board
//! projections. No SQL, GitHub, CLI, hook, or Continuum policy lives
//! here.

#![deny(unsafe_code)]

use airc_core::{RoomId, TranscriptCursor, TranscriptEvent};
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
        Ok(WorkBoardProjection::replay(page.events)?)
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
