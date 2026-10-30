//! Replay work-domain events from recorded AIRC transcripts.

use airc_core::{EventId, TranscriptCursor, TranscriptEvent};

use crate::{decode_work_event, ProjectionError, WorkBoardProjection, WorkEvent};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkReplayItem {
    pub cursor: TranscriptCursor,
    pub event: WorkEvent,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkReplayError {
    #[error("review transcript {event_id} rejected: {source}")]
    RejectedReview {
        event_id: EventId,
        #[source]
        source: crate::WorkEventCodecError,
    },
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
    crate::work_event_selection().matches(event.class)
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
            WorkReplayError::Codec {
                event_id: event.event_id,
                source,
            }
        })?;
    let expected = crate::work_event_class(&work_event);
    if event.class != expected {
        return Err(WorkReplayError::Codec {
            event_id: event.event_id,
            source: crate::WorkEventCodecError::ClassMismatch {
                expected,
                actual: event.class,
            },
        });
    }
    if let WorkEvent::CardUpdated(update) = &mut work_event {
        if update.claim_selection.as_ref().is_some_and(|choice| {
            choice.owner != event.peer_id || update.updated_by != event.peer_id
        }) {
            // Ignore unauthenticated intent without poisoning independent amendments.
            update.claim_selection = None;
        }
    }
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
    if let WorkEvent::WorkSubmissionReviewed(review) = &work_event {
        let reason = if review.reviewer != event.peer_id {
            Some(crate::WorkReviewRejectionReason::ReviewerMismatch)
        } else {
            review.validate().err()
        };
        if let Some(reason) = reason {
            let mut rejected = review.rejected(reason);
            rejected.reviewer = event.peer_id;
            work_event = WorkEvent::ReviewRejected(rejected);
        }
    }
    if let WorkEvent::SubmissionReviewRequested(request) = &work_event {
        if request.card.created_by != event.peer_id {
            return Err(WorkReplayError::RejectedReview {
                event_id: event.event_id,
                source: crate::WorkEventCodecError::ReviewRequestAuthorMismatch,
            });
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
            Err(
                error @ (WorkReplayError::RejectedSubmission { .. }
                | WorkReplayError::RejectedReview { .. }),
            ) => {
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
