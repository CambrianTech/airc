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
    #[error("submission transcript {event_id} rejected: {source}")]
    RejectedSubmission {
        event_id: EventId,
        #[source]
        source: crate::WorkEventCodecError,
    },
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
    let mut work_event =
        decode_work_event(&event.headers, event.body.as_ref()).map_err(|source| {
            // The body discriminator wins over the optional routing hint. A
            // spoofed hint must not hide corruption in another event family.
            let body_kind = match event.body.as_ref() {
                Some(airc_core::Body::Json(value)) => {
                    value.get("kind").and_then(|kind| kind.as_str())
                }
                _ => None,
            };
            let kind = body_kind.or_else(|| {
                event
                    .headers
                    .get(crate::HEADER_FORGE_WORK_EVENT_KIND)
                    .map(String::as_str)
            });
            if kind == Some("work_submitted") {
                WorkReplayError::RejectedSubmission {
                    event_id: event.event_id,
                    source,
                }
            } else {
                WorkReplayError::Codec {
                    event_id: event.event_id,
                    source,
                }
            }
        })?;
    if let WorkEvent::WorkSubmitted(submission) = &work_event {
        let reason = if submission.publisher != event.peer_id {
            Some(crate::event::SubmissionRejectionReason::PublisherMismatch)
        } else {
            submission.validate().err()
        };
        if let Some(reason) = reason {
            let mut rejected = submission.rejected(reason);
            rejected.publisher = event.peer_id;
            work_event = WorkEvent::SubmissionRejected(rejected);
        }
    }
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
        let item = match decode_transcript_work_event(&transcript_event) {
            Ok(item) => item,
            Err(error @ WorkReplayError::RejectedSubmission { .. }) => {
                eprintln!("airc work replay: {error}");
                continue;
            }
            Err(error) => return Err(error),
        };
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
