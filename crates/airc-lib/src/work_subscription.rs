//! Typed work-event subscription surface.
//!
//! Closes work card e1f8e2e0 (P0): "Work subscription API: typed
//! kanban/PR event stream for agents." Before this module, consumers
//! wanting work-card / claim / PR / availability events had to
//! subscribe to the raw substrate stream, call
//! `transcript_is_work_event` on each event, decode the body, then
//! pattern-match. After this module, consumers call
//! [`Airc::subscribe_work_events`] / [`Airc::recent_work_events`]
//! with a [`WorkEventFilter`] and get typed [`WorkEvent`] values.
//!
//! The substrate primitives this builds on were already shipped:
//! - `airc-work::transcript_is_work_event` — header-based discriminator
//! - `airc-work::decode_transcript_work_event` — decoder
//! - Headers `forge.work.repo` / `forge.work.lane_id` / `forge.work.card_id`
//!
//! This module adds the SDK API on top so consumers (Continuum
//! managers, agent schedulers, dashboards) don't reinvent the
//! filter + decode loop.

use std::sync::Arc;

use airc_core::{PeerId, TranscriptEvent};
use airc_work::{
    decode_transcript_work_event, transcript_is_work_event, LaneId, RepoId, WorkEvent,
};
use futures::stream::{Stream, StreamExt};

use crate::error::AircError;
use crate::Airc;

/// Header keys for filtering work events without decoding the body.
pub const HEADER_WORK_REPO: &str = "forge.work.repo";
pub const HEADER_WORK_LANE_ID: &str = "forge.work.lane_id";
pub const HEADER_WORK_CARD_ID: &str = "forge.work.card_id";

/// Filter applied at subscribe/query time. All fields are `Option`
/// — a `None` field matches anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkEventFilter {
    /// Restrict to events tagged with this `forge.work.repo`.
    pub repo: Option<RepoId>,
    /// Restrict to events tagged with this `forge.work.lane_id`.
    pub lane_id: Option<LaneId>,
    /// Restrict to events signed by this peer (the substrate-level
    /// signer of the transcript event, not the work-event's
    /// actor/owner field — those vary per variant).
    pub peer: Option<PeerId>,
}

impl WorkEventFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_repo(mut self, repo: RepoId) -> Self {
        self.repo = Some(repo);
        self
    }

    pub fn with_lane_id(mut self, lane_id: LaneId) -> Self {
        self.lane_id = Some(lane_id);
        self
    }

    pub fn with_peer(mut self, peer: PeerId) -> Self {
        self.peer = Some(peer);
        self
    }

    /// Cheap header-only check; avoids decoding the body if the
    /// substrate-level filters already rule the event out.
    pub(crate) fn matches_transcript(&self, event: &TranscriptEvent) -> bool {
        if let Some(peer) = self.peer {
            if event.peer_id != peer {
                return false;
            }
        }
        if let Some(repo) = self.repo.as_ref() {
            match event.headers.get(HEADER_WORK_REPO) {
                Some(value) if value == repo.as_str() => {}
                _ => return false,
            }
        }
        if let Some(lane_id) = self.lane_id.as_ref() {
            let expected = lane_id.to_string();
            match event.headers.get(HEADER_WORK_LANE_ID) {
                Some(value) if *value == expected => {}
                _ => return false,
            }
        }
        true
    }
}

impl Airc {
    /// Subscribe to typed work events. The returned stream yields
    /// `(transcript event, decoded WorkEvent)` pairs only for
    /// events matching `filter`. Subscription is live — events
    /// emitted after the call surface here; for historical events,
    /// use [`Airc::recent_work_events`].
    pub async fn subscribe_work_events(
        &self,
        filter: WorkEventFilter,
    ) -> Result<impl Stream<Item = (Arc<TranscriptEvent>, WorkEvent)>, AircError> {
        let inner = self.subscribe().await?;
        Ok(inner.filter_map(move |item| {
            let filter = filter.clone();
            async move {
                let event = item.ok()?;
                if !transcript_is_work_event(&event) {
                    return None;
                }
                if !filter.matches_transcript(&event) {
                    return None;
                }
                let item = decode_transcript_work_event(&event).ok()?;
                Some((event, item.event))
            }
        }))
    }

    /// Query recent work events from the persisted transcript.
    /// Walks the last `window` events, filters, decodes, and returns
    /// the matching `WorkEvent`s in transcript order (oldest →
    /// newest). Useful for "current state" queries that don't need
    /// a live subscription.
    pub async fn recent_work_events(
        &self,
        filter: WorkEventFilter,
        window: usize,
    ) -> Result<Vec<WorkEvent>, AircError> {
        let recent = self.page_recent(window).await?;
        let mut out = Vec::with_capacity(recent.len().min(window));
        for transcript_event in recent {
            if !transcript_is_work_event(&transcript_event) {
                continue;
            }
            if !filter.matches_transcript(&transcript_event) {
                continue;
            }
            if let Ok(item) = decode_transcript_work_event(&transcript_event) {
                out.push(item.event);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_work::WorkCardId;

    fn sample_filter() -> WorkEventFilter {
        WorkEventFilter::new()
            .with_repo(RepoId::new("test-org/test-repo").unwrap())
            .with_peer(PeerId::new())
    }

    #[test]
    fn empty_filter_matches_everything() {
        let filter = WorkEventFilter::default();
        let mut headers = airc_core::headers::Headers::new();
        headers.insert(
            crate::work_subscription::HEADER_WORK_REPO.to_string(),
            "anywhere".to_string(),
        );
        let event = TranscriptEvent {
            event_id: airc_core::EventId::new(),
            peer_id: PeerId::new(),
            client_id: airc_core::ClientId::new(),
            room_id: airc_core::RoomId::new(),
            kind: airc_core::TranscriptKind::System,
            occurred_at_ms: 0,
            lamport: 0,
            target: airc_core::transcript::MentionTarget::All,
            headers,
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        assert!(filter.matches_transcript(&event));
    }

    #[test]
    fn peer_filter_rejects_other_signers() {
        let target_peer = PeerId::new();
        let other_peer = PeerId::new();
        let filter = WorkEventFilter::new().with_peer(target_peer);
        let event = TranscriptEvent {
            event_id: airc_core::EventId::new(),
            peer_id: other_peer,
            client_id: airc_core::ClientId::new(),
            room_id: airc_core::RoomId::new(),
            kind: airc_core::TranscriptKind::System,
            occurred_at_ms: 0,
            lamport: 0,
            target: airc_core::transcript::MentionTarget::All,
            headers: airc_core::headers::Headers::new(),
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        assert!(!filter.matches_transcript(&event));
    }

    #[test]
    fn repo_filter_matches_header_value() {
        let repo = RepoId::new("CambrianTech/airc").unwrap();
        let filter = WorkEventFilter::new().with_repo(repo);
        let mut headers = airc_core::headers::Headers::new();
        headers.insert(
            HEADER_WORK_REPO.to_string(),
            "CambrianTech/airc".to_string(),
        );
        let event = TranscriptEvent {
            event_id: airc_core::EventId::new(),
            peer_id: PeerId::new(),
            client_id: airc_core::ClientId::new(),
            room_id: airc_core::RoomId::new(),
            kind: airc_core::TranscriptKind::System,
            occurred_at_ms: 0,
            lamport: 0,
            target: airc_core::transcript::MentionTarget::All,
            headers,
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        assert!(filter.matches_transcript(&event));
    }

    #[test]
    fn repo_filter_rejects_other_repo() {
        let filter =
            WorkEventFilter::new().with_repo(RepoId::new("expected-org/expected-repo").unwrap());
        let mut headers = airc_core::headers::Headers::new();
        headers.insert(
            HEADER_WORK_REPO.to_string(),
            "wrong-org/wrong-repo".to_string(),
        );
        let event = TranscriptEvent {
            event_id: airc_core::EventId::new(),
            peer_id: PeerId::new(),
            client_id: airc_core::ClientId::new(),
            room_id: airc_core::RoomId::new(),
            kind: airc_core::TranscriptKind::System,
            occurred_at_ms: 0,
            lamport: 0,
            target: airc_core::transcript::MentionTarget::All,
            headers,
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        assert!(!filter.matches_transcript(&event));
    }

    #[test]
    fn filter_builder_chains() {
        let filter = WorkEventFilter::new()
            .with_repo(RepoId::new("test/repo").unwrap())
            .with_peer(PeerId::new());
        assert!(filter.repo.is_some());
        assert!(filter.peer.is_some());
        assert!(filter.lane_id.is_none());
    }

    // Compile-time sanity check that the builder doesn't move the
    // owner — useful when extending tests later.
    #[test]
    fn filter_is_cloneable() {
        let _ = sample_filter().clone();
        let _: WorkCardId = WorkCardId::new();
    }
}
