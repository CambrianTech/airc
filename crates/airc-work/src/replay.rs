//! Replay work-domain events from recorded AIRC transcripts.

use airc_core::{EventId, TranscriptCursor, TranscriptEvent};
use airc_protocol::HEADER_FORGE_BODY_HINT;

use crate::{
    decode_work_event, ProjectionError, WorkBoardProjection, WorkEvent, BODY_HINT_FORGE_WORK_EVENT,
};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkReplayItem {
    pub cursor: TranscriptCursor,
    pub event: WorkEvent,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkReplayError {
    #[error("transcript event {event_id} is not a work-domain event")]
    NotWorkEvent { event_id: EventId },
    #[error("transcript event {event_id} has invalid work payload: {source}")]
    Codec {
        event_id: EventId,
        #[source]
        source: crate::WorkEventCodecError,
    },
    #[error("work projection failed at transcript event {event_id}: {source}")]
    Projection {
        event_id: EventId,
        #[source]
        source: ProjectionError,
    },
}

pub fn transcript_is_work_event(event: &TranscriptEvent) -> bool {
    event
        .headers
        .get(HEADER_FORGE_BODY_HINT)
        .is_some_and(|hint| hint == BODY_HINT_FORGE_WORK_EVENT)
}

pub fn decode_transcript_work_event(
    event: &TranscriptEvent,
) -> Result<WorkReplayItem, WorkReplayError> {
    if !transcript_is_work_event(event) {
        return Err(WorkReplayError::NotWorkEvent {
            event_id: event.event_id,
        });
    }
    let work_event = decode_work_event(&event.headers, event.body.as_ref()).map_err(|source| {
        WorkReplayError::Codec {
            event_id: event.event_id,
            source,
        }
    })?;
    Ok(WorkReplayItem {
        cursor: event.cursor(),
        event: work_event,
    })
}

pub fn project_transcript_work_events(
    events: impl IntoIterator<Item = TranscriptEvent>,
) -> Result<WorkBoardProjection, WorkReplayError> {
    let mut events: Vec<_> = events.into_iter().collect();
    events.sort_by(transcript_order);

    let mut projection = WorkBoardProjection::new();
    for transcript_event in events {
        let item = decode_transcript_work_event(&transcript_event)?;
        projection
            .apply(&item.event)
            .map_err(|source| WorkReplayError::Projection {
                event_id: transcript_event.event_id,
                source,
            })?;
    }
    Ok(projection)
}

fn transcript_order(left: &TranscriptEvent, right: &TranscriptEvent) -> std::cmp::Ordering {
    left.lamport
        .cmp(&right.lamport)
        .then_with(|| left.event_id.0.cmp(&right.event_id.0))
}

#[cfg(test)]
mod tests;
