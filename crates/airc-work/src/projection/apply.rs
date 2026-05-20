use crate::event::{
    CardCreated, CardStateChanged, ClaimHeartbeat, ClaimReleased, HygieneReportRecorded,
    LaneCreated, LaneStateChanged, ManagerHatClaimed, ManagerHatReleased, PullRequestLinked,
    PullRequestMerged, WorkCardClaimed, WorkEvent, WorkspaceAllocated, WorkspaceDrainCompleted,
    WorkspaceDrainRequested, WorkspaceHeartbeat, WorkspacePressureReported, WorkspaceReleased,
    WorkspaceRequested,
};
use crate::ids::{WorkCardId, WorkspaceId};
use crate::model::{CardState, WorkCard, WorkspaceLease, WorkspaceStatus};

use super::{
    BoardSnapshot, LaneRecord, ManagerHat, ProjectionError, StaleClaim, WorkBoardProjection,
    WorkspaceRecord,
};

impl WorkBoardProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, event: &WorkEvent) -> Result<(), ProjectionError> {
        match event {
            WorkEvent::CardCreated(e) => self.apply_card_created(e),
            WorkEvent::CardClaimed(e) => self.apply_card_claimed(e),
            WorkEvent::ClaimHeartbeat(e) => self.apply_claim_heartbeat(e),
            WorkEvent::ClaimReleased(e) => self.apply_claim_released(e),
            WorkEvent::CardStateChanged(e) => self.apply_card_state_changed(e),
            WorkEvent::LaneCreated(e) => self.apply_lane_created(e),
            WorkEvent::LaneStateChanged(e) => self.apply_lane_state_changed(e),
            WorkEvent::WorkspaceRequested(e) => self.apply_workspace_requested(e),
            WorkEvent::WorkspaceAllocated(e) => self.apply_workspace_allocated(e),
            WorkEvent::WorkspaceHeartbeat(e) => self.apply_workspace_heartbeat(e),
            WorkEvent::WorkspaceReleased(e) => self.apply_workspace_released(e),
            WorkEvent::WorkspacePressureReported(e) => self.apply_workspace_pressure_reported(e),
            WorkEvent::WorkspaceDrainRequested(e) => self.apply_workspace_drain_requested(e),
            WorkEvent::WorkspaceDrainCompleted(e) => self.apply_workspace_drain_completed(e),
            WorkEvent::PullRequestLinked(e) => self.apply_pull_request_linked(e),
            WorkEvent::PullRequestMerged(e) => self.apply_pull_request_merged(e),
            WorkEvent::HygieneReportRecorded(e) => self.apply_hygiene_report_recorded(e),
            WorkEvent::ManagerHatClaimed(e) => self.apply_manager_hat_claimed(e),
            WorkEvent::ManagerHatReleased(e) => self.apply_manager_hat_released(e),
        }
    }

    pub fn replay(events: impl IntoIterator<Item = WorkEvent>) -> Result<Self, ProjectionError> {
        let mut projection = Self::new();
        for event in events {
            projection.apply(&event)?;
        }
        Ok(projection)
    }

    pub fn snapshot(&self) -> BoardSnapshot {
        BoardSnapshot {
            cards: self.cards.values().cloned().collect(),
            lanes: self.lanes.values().cloned().collect(),
            workspaces: self.workspaces.values().cloned().collect(),
            manager_hats: self.manager_hats.values().cloned().collect(),
            hygiene_reports: self.hygiene_reports.clone(),
        }
    }

    pub fn card(&self, card_id: WorkCardId) -> Option<&WorkCard> {
        self.cards.get(&card_id)
    }

    pub fn workspace(&self, workspace_id: WorkspaceId) -> Option<&WorkspaceRecord> {
        self.workspaces.get(&workspace_id)
    }

    pub fn stale_claims(&self, now_ms: u64) -> Vec<StaleClaim> {
        self.cards
            .values()
            .filter_map(|card| {
                let (owner, claim_id, expires_at_ms) =
                    (card.owner?, card.claim_id?, card.claim_expires_at_ms?);
                (expires_at_ms <= now_ms).then_some(StaleClaim {
                    card_id: card.card_id,
                    claim_id,
                    owner,
                    expired_at_ms: expires_at_ms,
                })
            })
            .collect()
    }

    fn apply_card_created(&mut self, e: &CardCreated) -> Result<(), ProjectionError> {
        if self.cards.contains_key(&e.card_id) {
            return Err(ProjectionError::DuplicateCard(e.card_id));
        }
        let card = WorkCard {
            card_id: e.card_id,
            repo: e.repo.clone(),
            title: e.title.clone(),
            body: e.body.clone(),
            priority: e.priority,
            lane_id: e.lane_id,
            state: CardState::Open,
            owner: None,
            claim_id: None,
            claim_expires_at_ms: None,
            last_heartbeat_at_ms: None,
            pull_request: None,
            created_by: e.created_by,
            created_at_ms: e.created_at_ms,
            updated_at_ms: e.created_at_ms,
        };
        self.cards.insert(e.card_id, card);
        if let Some(lane_id) = e.lane_id {
            if let Some(lane) = self.lanes.get_mut(&lane_id) {
                lane.card_ids.push(e.card_id);
            }
        }
        Ok(())
    }

    fn apply_card_claimed(&mut self, e: &WorkCardClaimed) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        card.state = CardState::Claimed;
        card.owner = Some(e.owner);
        card.claim_id = Some(e.claim_id);
        card.claim_expires_at_ms = Some(e.claimed_at_ms + e.ttl_ms);
        card.last_heartbeat_at_ms = Some(e.claimed_at_ms);
        card.updated_at_ms = e.claimed_at_ms;
        Ok(())
    }

    fn apply_claim_heartbeat(&mut self, e: &ClaimHeartbeat) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        ensure_claim(card, e.claim_id)?;
        card.claim_expires_at_ms = Some(e.heartbeat_at_ms + e.ttl_ms);
        card.last_heartbeat_at_ms = Some(e.heartbeat_at_ms);
        card.updated_at_ms = e.heartbeat_at_ms;
        Ok(())
    }

    fn apply_claim_released(&mut self, e: &ClaimReleased) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        ensure_claim(card, e.claim_id)?;
        card.state = CardState::Open;
        card.owner = None;
        card.claim_id = None;
        card.claim_expires_at_ms = None;
        card.updated_at_ms = e.released_at_ms;
        Ok(())
    }

    fn apply_card_state_changed(&mut self, e: &CardStateChanged) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        card.state = e.state;
        card.updated_at_ms = e.changed_at_ms;
        Ok(())
    }

    fn apply_lane_created(&mut self, e: &LaneCreated) -> Result<(), ProjectionError> {
        if self.lanes.contains_key(&e.lane_id) {
            return Err(ProjectionError::DuplicateLane(e.lane_id));
        }
        self.lanes.insert(
            e.lane_id,
            LaneRecord {
                lane_id: e.lane_id,
                repo: e.repo.clone(),
                title: e.title.clone(),
                state: e.state,
                card_ids: Vec::new(),
                created_by: e.created_by,
                created_at_ms: e.created_at_ms,
                updated_at_ms: e.created_at_ms,
            },
        );
        Ok(())
    }

    fn apply_lane_state_changed(&mut self, e: &LaneStateChanged) -> Result<(), ProjectionError> {
        let lane = self
            .lanes
            .get_mut(&e.lane_id)
            .ok_or(ProjectionError::UnknownLane(e.lane_id))?;
        lane.state = e.state;
        lane.updated_at_ms = e.changed_at_ms;
        Ok(())
    }

    fn apply_workspace_requested(&mut self, e: &WorkspaceRequested) -> Result<(), ProjectionError> {
        if !self.cards.contains_key(&e.card_id) {
            return Err(ProjectionError::UnknownCard(e.card_id));
        }
        self.workspaces.insert(
            e.workspace_id,
            WorkspaceRecord {
                lease: WorkspaceLease {
                    workspace_id: e.workspace_id,
                    card_id: e.card_id,
                    claim_id: e.claim_id,
                    owner: e.owner,
                    repo: e.repo.clone(),
                    path: String::new(),
                    branch: e.branch.clone(),
                    base: e.base.clone(),
                    status: WorkspaceStatus::Requested,
                    disk_bytes: None,
                    created_at_ms: e.requested_at_ms,
                    heartbeat_at_ms: e.requested_at_ms,
                    released_at_ms: None,
                },
            },
        );
        Ok(())
    }

    fn apply_workspace_allocated(&mut self, e: &WorkspaceAllocated) -> Result<(), ProjectionError> {
        let workspace = self.workspace_mut(e.workspace_id)?;
        workspace.lease.path = e.path.clone();
        workspace.lease.status = WorkspaceStatus::Allocated;
        workspace.lease.heartbeat_at_ms = e.allocated_at_ms;
        Ok(())
    }

    fn apply_workspace_heartbeat(&mut self, e: &WorkspaceHeartbeat) -> Result<(), ProjectionError> {
        let workspace = self.workspace_mut(e.workspace_id)?;
        workspace.lease.status = WorkspaceStatus::Active;
        workspace.lease.disk_bytes = e.disk_bytes;
        workspace.lease.heartbeat_at_ms = e.heartbeat_at_ms;
        Ok(())
    }

    fn apply_workspace_released(&mut self, e: &WorkspaceReleased) -> Result<(), ProjectionError> {
        let workspace = self.workspace_mut(e.workspace_id)?;
        workspace.lease.status = WorkspaceStatus::Released;
        workspace.lease.released_at_ms = Some(e.released_at_ms);
        Ok(())
    }

    fn apply_workspace_pressure_reported(
        &mut self,
        e: &WorkspacePressureReported,
    ) -> Result<(), ProjectionError> {
        // Workspace-id-keyed; intentionally tolerates pressure on
        // workspaces that don't yet have a lease record. Replaces
        // previous observation; the projection holds only the latest
        // reading per workspace.
        self.workspace_pressure.insert(e.workspace_id, e.clone());
        Ok(())
    }

    fn apply_workspace_drain_requested(
        &mut self,
        e: &WorkspaceDrainRequested,
    ) -> Result<(), ProjectionError> {
        // Same workspace + same policy rule already pending = overwrite.
        // Two concurrent drains under the same rule is a policy bug;
        // the projection surfaces it by keeping the latest, not by
        // erroring (errors would hide the bug from observers).
        self.pending_drains
            .insert((e.workspace_id, e.policy_rule_id.clone()), e.clone());
        Ok(())
    }

    fn apply_workspace_drain_completed(
        &mut self,
        e: &WorkspaceDrainCompleted,
    ) -> Result<(), ProjectionError> {
        self.pending_drains
            .remove(&(e.workspace_id, e.policy_rule_id.clone()));
        self.drain_history.push(e.clone());
        Ok(())
    }

    fn apply_pull_request_linked(&mut self, e: &PullRequestLinked) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        card.pull_request = Some(e.pull_request.clone());
        card.state = CardState::Review;
        card.updated_at_ms = e.linked_at_ms;
        Ok(())
    }

    fn apply_pull_request_merged(&mut self, e: &PullRequestMerged) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        card.pull_request = Some(e.pull_request.clone());
        card.state = CardState::Merged;
        card.updated_at_ms = e.merged_at_ms;
        Ok(())
    }

    fn apply_hygiene_report_recorded(
        &mut self,
        e: &HygieneReportRecorded,
    ) -> Result<(), ProjectionError> {
        self.hygiene_reports.push(e.report.clone());
        Ok(())
    }

    fn apply_manager_hat_claimed(&mut self, e: &ManagerHatClaimed) -> Result<(), ProjectionError> {
        self.manager_hats.insert(
            e.repo.clone(),
            ManagerHat {
                repo: e.repo.clone(),
                manager: e.manager,
                expires_at_ms: e.claimed_at_ms + e.ttl_ms,
                claimed_at_ms: e.claimed_at_ms,
            },
        );
        Ok(())
    }

    fn apply_manager_hat_released(
        &mut self,
        e: &ManagerHatReleased,
    ) -> Result<(), ProjectionError> {
        if let Some(hat) = self.manager_hats.get(&e.repo) {
            if hat.manager == e.manager {
                self.manager_hats.remove(&e.repo);
            }
        }
        Ok(())
    }

    fn card_mut(&mut self, card_id: WorkCardId) -> Result<&mut WorkCard, ProjectionError> {
        self.cards
            .get_mut(&card_id)
            .ok_or(ProjectionError::UnknownCard(card_id))
    }

    fn workspace_mut(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<&mut WorkspaceRecord, ProjectionError> {
        self.workspaces
            .get_mut(&workspace_id)
            .ok_or(ProjectionError::UnknownWorkspace(workspace_id))
    }
}

fn ensure_claim(card: &WorkCard, got: crate::ids::ClaimId) -> Result<(), ProjectionError> {
    if card.claim_id == Some(got) {
        return Ok(());
    }
    Err(ProjectionError::ClaimMismatch {
        card_id: card.card_id,
        expected: card.claim_id,
        got,
    })
}
