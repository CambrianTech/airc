use crate::event::{
    AgentAvailabilityReported, CardCreated, CardStateChanged, CardUpdated, ClaimHeartbeat,
    ClaimReleased, GitBranchMoved, GitCommitObserved, GitDirtyStateChanged, HygieneReportRecorded,
    LaneCreated, LaneStateChanged, ManagerHatClaimed, ManagerHatReleased,
    PullRequestCheckSuiteChanged, PullRequestLinked, PullRequestMergeStateChanged,
    PullRequestMerged, PullRequestReviewSubmitted, WorkCardClaimed, WorkEvent, WorkspaceAllocated,
    WorkspaceDrainCompleted, WorkspaceDrainRequested, WorkspaceHeartbeat,
    WorkspacePressureReported, WorkspaceReleased, WorkspaceRequested,
};
use crate::ids::{WorkCardId, WorkspaceId};
use crate::model::{CardState, WorkCard, WorkspaceLease, WorkspaceStatus};

use super::{
    pull_request_key, AgentAvailabilityRecord, BoardSnapshot, BranchTrackingRecord, LaneRecord,
    ManagerHat, ProjectionError, PullRequestRecord, RepoTrackingRecord, StaleClaim,
    WorkBoardProjection, WorkspaceRecord,
};

impl WorkBoardProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, event: &WorkEvent) -> Result<(), ProjectionError> {
        match event {
            WorkEvent::CardCreated(e) => self.apply_card_created(e),
            WorkEvent::CardUpdated(e) => self.apply_card_updated(e),
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
            WorkEvent::GitCommitObserved(e) => self.apply_git_commit_observed(e),
            WorkEvent::GitBranchMoved(e) => self.apply_git_branch_moved(e),
            WorkEvent::GitDirtyStateChanged(e) => self.apply_git_dirty_state_changed(e),
            WorkEvent::PullRequestCheckSuiteChanged(e) => {
                self.apply_pull_request_check_suite_changed(e);
                Ok(())
            }
            WorkEvent::PullRequestReviewSubmitted(e) => {
                self.apply_pull_request_review_submitted(e);
                Ok(())
            }
            WorkEvent::PullRequestMergeStateChanged(e) => {
                self.apply_pull_request_merge_state_changed(e);
                Ok(())
            }
            WorkEvent::PullRequestLinked(e) => self.apply_pull_request_linked(e),
            WorkEvent::PullRequestMerged(e) => self.apply_pull_request_merged(e),
            WorkEvent::HygieneReportRecorded(e) => self.apply_hygiene_report_recorded(e),
            WorkEvent::ManagerHatClaimed(e) => self.apply_manager_hat_claimed(e),
            WorkEvent::ManagerHatReleased(e) => self.apply_manager_hat_released(e),
            WorkEvent::AgentAvailabilityReported(e) => {
                self.apply_agent_availability_reported(e);
                Ok(())
            }
        }
    }

    pub fn replay(events: impl IntoIterator<Item = WorkEvent>) -> Result<Self, ProjectionError> {
        let mut projection = Self::new();
        for event in events {
            projection.apply(&event)?;
        }
        Ok(projection)
    }

    /// Apply one event with bounded-window tolerance: an event whose
    /// anchor entity is missing (its creation predates the window /
    /// snapshot) is skipped, exactly as [`Self::replay_window`] skips
    /// it; structural errors still fail. This is the single apply
    /// rule shared by windowed replay AND incremental resume from a
    /// cached projection (card 1291173d) — the two paths stay
    /// semantically identical by construction.
    pub fn apply_windowed(&mut self, event: &WorkEvent) -> Result<(), ProjectionError> {
        match self.apply(event) {
            Ok(()) => Ok(()),
            Err(error) if error.is_missing_window_anchor() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Replay a bounded transcript window. Events whose anchor entity
    /// was created before the window are skipped; structural errors
    /// inside the window still fail.
    pub fn replay_window(
        events: impl IntoIterator<Item = WorkEvent>,
    ) -> Result<Self, ProjectionError> {
        let mut projection = Self::new();
        for event in events {
            projection.apply_windowed(&event)?;
        }
        Ok(projection)
    }

    pub fn snapshot(&self) -> BoardSnapshot {
        BoardSnapshot {
            cards: self.cards.values().cloned().collect(),
            lanes: self.lanes.values().cloned().collect(),
            workspaces: self.workspaces.values().cloned().collect(),
            repo_tracking: self.repo_tracking.values().cloned().collect(),
            pull_requests: self.pull_requests.values().cloned().collect(),
            manager_hats: self.manager_hats.values().cloned().collect(),
            agent_availability: self.agent_availability.values().cloned().collect(),
            hygiene_reports: self.hygiene_reports.clone(),
        }
    }

    pub fn card(&self, card_id: WorkCardId) -> Option<&WorkCard> {
        self.cards.get(&card_id)
    }

    pub fn workspace(&self, workspace_id: WorkspaceId) -> Option<&WorkspaceRecord> {
        self.workspaces.get(&workspace_id)
    }

    /// Cards whose `reviews` link points at `parent_id` — i.e. the
    /// review cards that exist for the given card. Card ad7e100b
    /// (peer-agent review loop) Sub-A: lets schedulers and CLI
    /// renderers ask "what reviews exist for this PR's card?"
    /// without scanning bodies.
    ///
    /// Returns an iterator over `&WorkCard` so callers can filter
    /// further (e.g. by state, to find unclaimed reviews) without
    /// the projection imposing a policy. Iteration order is
    /// unspecified; callers that need it deterministic should sort
    /// on `created_at_ms` or `card_id`.
    pub fn review_cards_for(&self, parent_id: WorkCardId) -> impl Iterator<Item = &WorkCard> + '_ {
        self.cards
            .values()
            .filter(move |card| card.reviews == Some(parent_id))
    }

    pub fn stale_claims(&self, now_ms: u64) -> Vec<StaleClaim> {
        self.cards
            .values()
            .filter(|card| !matches!(card.state, CardState::Merged | CardState::Closed))
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
            reviews: e.reviews,
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
        if card
            .claim_expires_at_ms
            .is_some_and(|expires_at_ms| expires_at_ms > e.claimed_at_ms)
        {
            return Ok(());
        }
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
        // Idempotent-replay tolerance (closes work card c29506b8,
        // "Work board replay window must not fail on partial
        // history"): if this heartbeat names a claim that isn't
        // currently active on the card, drop it silently. The
        // claim was either never granted (the projection treats
        // `apply_card_claimed` as first-write-wins, so a racing
        // second claim is dropped without state change) or was
        // already superseded by another release. Heartbeating a
        // ghost claim is a no-op — refusing to project would just
        // poison the board for every downstream consumer.
        if card.claim_id != Some(e.claim_id) {
            return Ok(());
        }
        card.claim_expires_at_ms = Some(e.heartbeat_at_ms + e.ttl_ms);
        card.last_heartbeat_at_ms = Some(e.heartbeat_at_ms);
        card.updated_at_ms = e.heartbeat_at_ms;
        Ok(())
    }

    fn apply_claim_released(&mut self, e: &ClaimReleased) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        // Two tolerant-replay branches that converge to the same
        // no-op (closes work card c29506b8). Both happen in real
        // distributed runs:
        //
        // 1. `claim_id.is_none()` — card already had its active
        //    claim released; subsequent release events for any
        //    older claim are idempotent (Codex's #987 fix).
        //
        // 2. `claim_id != Some(e.claim_id)` — release names a
        //    claim that the projection considers superseded
        //    (e.g. `apply_card_claimed` drops a racing second
        //    claim, but the second claimant still later emits a
        //    release for "their" claim id). Treat as no-op
        //    instead of panicking — refusing here freezes every
        //    consumer's board until the operator surgically
        //    rewrites the wire.
        if card.claim_id != Some(e.claim_id) {
            return Ok(());
        }
        card.state = match card.state {
            CardState::Claimed | CardState::InProgress | CardState::Blocked => CardState::Open,
            CardState::Open | CardState::Review | CardState::Merged | CardState::Closed => {
                card.state
            }
        };
        card.owner = None;
        card.claim_id = None;
        card.claim_expires_at_ms = None;
        card.updated_at_ms = e.released_at_ms;
        Ok(())
    }

    /// Card 5ac0a359 — apply an amendment to a card's editable fields.
    /// Each `Some(...)` field writes; each `None` leaves the existing
    /// projection value alone. Per-event `updated_at_ms` always moves
    /// (even for an all-`None` no-op amendment), giving observers a
    /// liveness signal.
    ///
    /// Replay determinism: this is a pure event apply. Out-of-order
    /// updates (a later `updated_at_ms` arriving before an earlier
    /// one due to lamport churn) project deterministically — the
    /// projection writes whatever the latest applied event says.
    /// Callers depending on causality between two updates should
    /// sequence them via lamport, the same way other event chains
    /// already do.
    fn apply_card_updated(&mut self, e: &CardUpdated) -> Result<(), ProjectionError> {
        let card = self.card_mut(e.card_id)?;
        if let Some(ref title) = e.title {
            card.title = title.clone();
        }
        if let Some(body) = &e.body {
            // Set the body to whatever the amendment carries.
            // Empty string is the "clear" idiom.
            card.body = Some(body.clone());
        }
        if let Some(priority) = e.priority {
            card.priority = priority;
        }
        card.updated_at_ms = e.updated_at_ms;
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

    fn apply_git_commit_observed(&mut self, e: &GitCommitObserved) -> Result<(), ProjectionError> {
        let repo = self.repo_tracking_record(e.repo.clone());
        repo.observed_commits.push(e.clone());
        if let Some(branch) = &e.branch {
            repo.branches.insert(
                branch.clone(),
                BranchTrackingRecord {
                    branch: branch.clone(),
                    head: e.commit.clone(),
                    updated_at_ms: e.observed_at_ms,
                },
            );
        }
        Ok(())
    }

    fn apply_git_branch_moved(&mut self, e: &GitBranchMoved) -> Result<(), ProjectionError> {
        let repo = self.repo_tracking_record(e.repo.clone());
        repo.branches.insert(
            e.branch.clone(),
            BranchTrackingRecord {
                branch: e.branch.clone(),
                head: e.new_head.clone(),
                updated_at_ms: e.moved_at_ms,
            },
        );
        Ok(())
    }

    fn apply_git_dirty_state_changed(
        &mut self,
        e: &GitDirtyStateChanged,
    ) -> Result<(), ProjectionError> {
        let repo = self.repo_tracking_record(e.repo.clone());
        repo.dirty_states.push(e.clone());
        Ok(())
    }

    fn apply_pull_request_check_suite_changed(&mut self, e: &PullRequestCheckSuiteChanged) {
        self.pull_requests
            .entry(pull_request_key(
                &e.pull_request.repo,
                e.pull_request.number,
            ))
            .and_modify(|record| record.apply_check(e))
            .or_insert_with(|| PullRequestRecord::from_check(e));
    }

    fn apply_pull_request_review_submitted(&mut self, e: &PullRequestReviewSubmitted) {
        self.pull_requests
            .entry(pull_request_key(
                &e.pull_request.repo,
                e.pull_request.number,
            ))
            .and_modify(|record| record.apply_review(e))
            .or_insert_with(|| PullRequestRecord::from_review(e));
    }

    fn apply_pull_request_merge_state_changed(&mut self, e: &PullRequestMergeStateChanged) {
        self.pull_requests
            .entry(pull_request_key(
                &e.pull_request.repo,
                e.pull_request.number,
            ))
            .and_modify(|record| record.apply_merge(e))
            .or_insert_with(|| PullRequestRecord::from_merge(e));
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

    fn apply_agent_availability_reported(&mut self, e: &AgentAvailabilityReported) {
        self.agent_availability.insert(
            format!("{}:{}", e.repo, e.peer),
            AgentAvailabilityRecord {
                report: e.clone(),
                expires_at_ms: e.reported_at_ms + e.ttl_ms,
            },
        );
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

    fn repo_tracking_record(&mut self, repo: crate::RepoId) -> &mut RepoTrackingRecord {
        self.repo_tracking
            .entry(repo.clone())
            .or_insert_with(|| RepoTrackingRecord::new(repo))
    }
}
