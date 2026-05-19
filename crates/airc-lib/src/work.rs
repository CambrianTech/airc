//! Consumer request structs for the work coordination API.

use airc_core::EventId;
use airc_protocol::FrameKind;
use airc_work::{
    encode_work_event, CardCreated, ClaimId, ClaimReleased, LaneId, Priority, RepoId,
    WorkBoardProjection, WorkCardClaimed, WorkCardId, WorkEvent,
};
use airc_work_store::WorkEventStore;

use crate::time::now_ms;
use crate::{Airc, AircError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkCard {
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimWorkCard {
    pub card_id: WorkCardId,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseWorkClaim {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub reason: Option<String>,
}

impl Airc {
    /// Create a work card in the current room and publish it as a
    /// signed work-domain event. Returns the UUIDv4 card id generated
    /// locally for this card.
    pub async fn create_work_card(&self, request: CreateWorkCard) -> Result<WorkCardId, AircError> {
        let card_id = WorkCardId::new();
        let event = WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: request.repo,
            title: request.title,
            body: request.body,
            priority: request.priority,
            lane_id: request.lane_id,
            created_by: self.peer_id(),
            created_at_ms: now_ms(),
        });
        self.publish_work_event(&event).await?;
        Ok(card_id)
    }

    /// Claim a work card for this peer. Returns the UUIDv4 claim id
    /// generated locally for the lease.
    pub async fn claim_work_card(&self, request: ClaimWorkCard) -> Result<ClaimId, AircError> {
        let claim_id = ClaimId::new();
        let event = WorkEvent::CardClaimed(WorkCardClaimed {
            card_id: request.card_id,
            claim_id,
            owner: self.peer_id(),
            ttl_ms: request.ttl_ms,
            claimed_at_ms: now_ms(),
        });
        self.publish_work_event(&event).await?;
        Ok(claim_id)
    }

    /// Release this peer's work claim.
    pub async fn release_work_claim(&self, request: ReleaseWorkClaim) -> Result<(), AircError> {
        let event = WorkEvent::ClaimReleased(ClaimReleased {
            card_id: request.card_id,
            claim_id: request.claim_id,
            owner: self.peer_id(),
            reason: request.reason,
            released_at_ms: now_ms(),
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Rebuild the current room's work board from persisted work
    /// events. `limit` is the transcript page size; callers that need
    /// complete history should pass a sufficiently high limit until a
    /// dedicated paged board API lands.
    pub async fn work_board(&self, limit: usize) -> Result<WorkBoardProjection, AircError> {
        let room = self.current_room().await?;
        self.ensure_room_subscriber(&room).await?;
        let store = WorkEventStore::new(self.event_store());
        Ok(store.project_recent(Some(room.channel), limit).await?)
    }

    async fn publish_work_event(&self, event: &WorkEvent) -> Result<EventId, AircError> {
        let (headers, body) = encode_work_event(event)?;
        self.send_frame(FrameKind::Event, body, headers).await
    }
}
