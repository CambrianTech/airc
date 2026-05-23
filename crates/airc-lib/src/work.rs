//! Consumer request structs for the work coordination API.

use airc_core::EventId;
use airc_protocol::FrameKind;
use airc_work::{
    encode_work_event, local_git_events_since, BranchName, CardCreated, ClaimId, ClaimReleased,
    CommandGitRunner, LaneCreated, LaneId, LaneState, LaneStateChanged, LocalGitObserver,
    LocalGitSnapshot, LocalGitWorkspace, ManagerHatClaimed, ManagerHatReleased, Priority, RepoId,
    WorkBoardProjection, WorkCardClaimed, WorkCardId, WorkEvent, WorkspaceAllocated,
    WorkspaceHeartbeat, WorkspaceId, WorkspaceReleased, WorkspaceRequested,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkLane {
    pub repo: RepoId,
    pub title: String,
    pub state: LaneState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeWorkLaneState {
    pub lane_id: LaneId,
    pub state: LaneState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestWorkspace {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub repo: RepoId,
    pub branch: BranchName,
    pub base: BranchName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocateWorkspace {
    pub workspace_id: WorkspaceId,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatWorkspace {
    pub workspace_id: WorkspaceId,
    pub disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseWorkspace {
    pub workspace_id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimManagerHat {
    pub repo: RepoId,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManagerHat {
    pub repo: RepoId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserveLocalGitWorkspace {
    pub repo: RepoId,
    pub workspace_id: Option<WorkspaceId>,
    pub path: std::path::PathBuf,
    pub previous: Option<LocalGitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedLocalGitWorkspace {
    pub snapshot: LocalGitSnapshot,
    pub emitted_event_ids: Vec<EventId>,
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
            created_at_ms: now_ms()?,
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
            claimed_at_ms: now_ms()?,
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
            released_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Create a work lane in the current room. Kanban boards are
    /// projections over lane + card events; this returns the UUIDv4
    /// lane id generated locally.
    pub async fn create_work_lane(&self, request: CreateWorkLane) -> Result<LaneId, AircError> {
        let lane_id = LaneId::new();
        let event = WorkEvent::LaneCreated(LaneCreated {
            lane_id,
            repo: request.repo,
            title: request.title,
            state: request.state,
            created_by: self.peer_id(),
            created_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(lane_id)
    }

    /// Change a lane's state through the work event stream.
    pub async fn change_work_lane_state(
        &self,
        request: ChangeWorkLaneState,
    ) -> Result<(), AircError> {
        let event = WorkEvent::LaneStateChanged(LaneStateChanged {
            lane_id: request.lane_id,
            state: request.state,
            changed_by: self.peer_id(),
            changed_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Request a workspace lease for an already-claimed work card.
    /// The request is a typed event; actual worktree allocation is a
    /// later adapter that consumes this event.
    pub async fn request_workspace(
        &self,
        request: RequestWorkspace,
    ) -> Result<WorkspaceId, AircError> {
        let workspace_id = WorkspaceId::new();
        let event = WorkEvent::WorkspaceRequested(WorkspaceRequested {
            workspace_id,
            card_id: request.card_id,
            claim_id: request.claim_id,
            owner: self.peer_id(),
            repo: request.repo,
            branch: request.branch,
            base: request.base,
            requested_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(workspace_id)
    }

    /// Mark a requested workspace as allocated at a concrete path.
    pub async fn allocate_workspace(&self, request: AllocateWorkspace) -> Result<(), AircError> {
        let event = WorkEvent::WorkspaceAllocated(WorkspaceAllocated {
            workspace_id: request.workspace_id,
            path: request.path,
            allocated_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Heartbeat a workspace lease with optional disk usage.
    pub async fn heartbeat_workspace(&self, request: HeartbeatWorkspace) -> Result<(), AircError> {
        let event = WorkEvent::WorkspaceHeartbeat(WorkspaceHeartbeat {
            workspace_id: request.workspace_id,
            disk_bytes: request.disk_bytes,
            heartbeat_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Release a workspace lease.
    pub async fn release_workspace(&self, request: ReleaseWorkspace) -> Result<(), AircError> {
        let event = WorkEvent::WorkspaceReleased(WorkspaceReleased {
            workspace_id: request.workspace_id,
            released_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Claim the manager hat for a repo in the current room. This is
    /// a lease, not a permission boundary; peers can see who is
    /// coordinating and when that claim expires.
    pub async fn claim_manager_hat(&self, request: ClaimManagerHat) -> Result<(), AircError> {
        let event = WorkEvent::ManagerHatClaimed(ManagerHatClaimed {
            repo: request.repo,
            manager: self.peer_id(),
            ttl_ms: request.ttl_ms,
            claimed_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Release this peer's manager hat for a repo.
    pub async fn release_manager_hat(&self, request: ReleaseManagerHat) -> Result<(), AircError> {
        let event = WorkEvent::ManagerHatReleased(ManagerHatReleased {
            repo: request.repo,
            manager: self.peer_id(),
            released_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Observe a local git worktree and publish typed work-domain
    /// events for changes since the caller's previous snapshot.
    ///
    /// This is the local adapter boundary: it shells out to `git`, but
    /// only typed `airc-work` events cross the substrate.
    pub async fn observe_local_git_workspace(
        &self,
        request: ObserveLocalGitWorkspace,
    ) -> Result<ObservedLocalGitWorkspace, AircError> {
        let workspace = LocalGitWorkspace {
            repo: request.repo,
            workspace_id: request.workspace_id,
            path: request.path,
        };
        let snapshot = LocalGitObserver::new(CommandGitRunner).observe(&workspace)?;
        let events = local_git_events_since(
            &workspace,
            request.previous.as_ref(),
            &snapshot,
            self.peer_id(),
            now_ms()?,
        );
        let mut emitted_event_ids = Vec::with_capacity(events.len());
        for event in events {
            emitted_event_ids.push(self.publish_work_event(&event).await?);
        }

        Ok(ObservedLocalGitWorkspace {
            snapshot,
            emitted_event_ids,
        })
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
