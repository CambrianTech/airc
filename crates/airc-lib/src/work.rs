//! Consumer request structs for the work coordination API.

use airc_core::EventId;
use airc_protocol::FrameKind;
use airc_work::{
    encode_work_event, local_git_events_since, pull_request_events_since, AgentAvailabilityRecord,
    AgentAvailabilityReported, AgentAvailabilityState, BranchName, CardCreated, CardState,
    CardStateChanged, ClaimHeartbeat, ClaimId, ClaimReleased, CommandGitRunner, LaneCreated,
    LaneId, LaneState, LaneStateChanged, LocalGitObserver, LocalGitSnapshot, LocalGitWorkspace,
    ManagerHatClaimed, ManagerHatReleased, Priority, PullRequestObserver, PullRequestSource,
    RepoId, RepoPullRequestSnapshot, StaleClaim, WorkBoardProjection, WorkCard, WorkCardClaimed,
    WorkCardId, WorkEvent, WorkspaceAllocated, WorkspaceHeartbeat, WorkspaceId, WorkspaceReleased,
    WorkspaceRequested,
};
use airc_work_store::WorkEventStore;

use crate::time::now_ms;
use crate::{Airc, AircError};

const WORK_MUTATION_PAGE_SIZE: usize = 512;

/// Card 79953b4d: how many raw transcript events to fetch from the
/// daemon per requested work board page entry. Heartbeats with their
/// d4e3e350 coordination payload share the recent-event window with
/// work events; with N active peers heart-beating every 60s a flat
/// `limit`-event scan can be ~90% heartbeats. 4x over-fetch trades a
/// bit of IPC bandwidth for the property "user's requested `limit`
/// number of work events actually lands in the projection." Server-
/// side filtering at the daemon's page rpc would be cleaner; this is
/// the minimum-viable fix until that follow-up.
const WORK_BOARD_FETCH_MULTIPLIER: usize = 4;

/// Card 79953b4d (pure helper for unit-testability): true when a
/// transcript event is a work-domain event. Distinguishes work events
/// from heartbeats / chat / other lifecycle by header presence rather
/// than body shape — every work event carries
/// `HEADER_FORGE_WORK_EVENT_KIND`; heartbeats and chat do not.
fn is_work_event_transcript(event: &airc_core::TranscriptEvent) -> bool {
    event
        .headers
        .iter()
        .any(|(key, _)| key == airc_work::HEADER_FORGE_WORK_EVENT_KIND)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkCard {
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
    /// If this card is a review of another card, the reviewed
    /// card's id. Card ad7e100b (peer-agent review loop) Sub-A:
    /// makes the relationship a typed link rather than a
    /// body-string convention. Defaults to `None` for non-review
    /// cards.
    #[doc(hidden)]
    pub reviews: Option<WorkCardId>,
}

impl CreateWorkCard {
    /// Default to `None` for the optional fields so the common
    /// path (a non-review card) doesn't need to spell out every
    /// new optional field as the request struct grows.
    pub fn new(repo: RepoId, title: impl Into<String>, priority: Priority) -> Self {
        Self {
            repo,
            title: title.into(),
            body: None,
            priority,
            lane_id: None,
            reviews: None,
        }
    }

    /// Builder-style setter for the typed reviews link.
    /// Convention: `airc work review <PARENT>` will populate
    /// this; manual callers can chain `.reviewing(parent)`.
    pub fn reviewing(mut self, parent: WorkCardId) -> Self {
        self.reviews = Some(parent);
        self
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeWorkCardState {
    pub card_id: WorkCardId,
    pub state: CardState,
}

/// Amend a card's editable fields after creation. Card 5ac0a359 —
/// addresses the recurring friction of needing to update a card's
/// title/body/priority post-creation without losing its id (which
/// would break `reviews` links, observer subscriptions, and the
/// projection's continuity guarantee).
///
/// Each field is `Option`; `None` means "leave alone." To clear a
/// body, pass `Some("".into())` — empty string is the markdown
/// "no body" idiom.
///
/// Construct via [`UpdateWorkCard::amend`] for convenience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateWorkCard {
    pub card_id: WorkCardId,
    pub title: Option<String>,
    pub body: Option<String>,
    pub priority: Option<Priority>,
}

impl UpdateWorkCard {
    /// Builder constructor: start with `card_id` and chain
    /// `.with_title(...) / .with_body(...) / .with_priority(...)`.
    pub fn amend(card_id: WorkCardId) -> Self {
        Self {
            card_id,
            title: None,
            body: None,
            priority: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the body. Pass `""` to clear (markdown "no body" idiom).
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }
}

/// Link a pull request to a work card (card 820629e9). The projection
/// (`apply_pull_request_linked`) atomically transitions the card into
/// `CardState::Review` and populates `WorkCard.pull_request` so any
/// downstream consumer — auto-spawn review card on Review state
/// (ad7e100b Sub-C), board renderers, gh check-suite observers —
/// reads from one source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCardPullRequest {
    pub card_id: WorkCardId,
    pub pull_request: airc_work::model::PullRequestRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatWorkClaim {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub ttl_ms: u64,
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
pub struct ReportAgentAvailability {
    pub repo: RepoId,
    pub state: AgentAvailabilityState,
    pub note: Option<String>,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimableWorkQuery {
    pub repo: Option<RepoId>,
    pub max_priority: Priority,
    pub include_stale_claims: bool,
    pub event_limit: usize,
    pub limit: usize,
}

impl Default for ClaimableWorkQuery {
    fn default() -> Self {
        Self {
            repo: None,
            max_priority: Priority::P1,
            include_stale_claims: true,
            event_limit: 512,
            limit: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimableWorkItem {
    pub card: WorkCard,
    pub stale_claim: Option<StaleClaim>,
}

impl ClaimableWorkItem {
    pub fn is_stale_claim(&self) -> bool {
        self.stale_claim.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkQueueStatusQuery {
    pub repo: Option<RepoId>,
    pub max_priority: Priority,
    pub include_stale_claims: bool,
    pub event_limit: usize,
    pub limit: usize,
}

impl Default for WorkQueueStatusQuery {
    fn default() -> Self {
        Self {
            repo: None,
            max_priority: Priority::P1,
            include_stale_claims: true,
            event_limit: 512,
            limit: 8,
        }
    }
}

impl From<ClaimableWorkQuery> for WorkQueueStatusQuery {
    fn from(value: ClaimableWorkQuery) -> Self {
        Self {
            repo: value.repo,
            max_priority: value.max_priority,
            include_stale_claims: value.include_stale_claims,
            event_limit: value.event_limit,
            limit: value.limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkQueueStatus {
    pub claimable: Vec<ClaimableWorkItem>,
    pub agent_availability: Vec<AgentAvailabilityRecord>,
    pub active_claims_for_peer: Vec<WorkCard>,
}

impl WorkQueueStatus {
    pub fn ready_count(&self, now_ms: u64) -> usize {
        self.agent_availability
            .iter()
            .filter(|record| {
                record.expires_at_ms > now_ms
                    && record.report.state == AgentAvailabilityState::Ready
            })
            .count()
    }

    pub fn busy_count(&self, now_ms: u64) -> usize {
        self.agent_availability
            .iter()
            .filter(|record| {
                record.expires_at_ms > now_ms && record.report.state == AgentAvailabilityState::Busy
            })
            .count()
    }

    pub fn away_count(&self, now_ms: u64) -> usize {
        self.agent_availability
            .iter()
            .filter(|record| {
                record.expires_at_ms > now_ms && record.report.state == AgentAvailabilityState::Away
            })
            .count()
    }

    pub fn stale_availability_count(&self, now_ms: u64) -> usize {
        self.agent_availability
            .iter()
            .filter(|record| record.expires_at_ms <= now_ms)
            .count()
    }
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

/// Observe pull-request state for `repo` against an optional prior
/// snapshot. The caller owns the source impl (gh-CLI, REST, or a stub)
/// so the SDK is free of any hard GitHub dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservePullRequests {
    pub repo: RepoId,
    pub previous: Option<RepoPullRequestSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPullRequests {
    pub snapshot: RepoPullRequestSnapshot,
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
            reviews: request.reviews,
        });
        self.publish_work_event(&event).await?;
        Ok(card_id)
    }

    /// Claim a work card for this peer. Returns the UUIDv4 claim id
    /// generated locally for the lease.
    pub async fn claim_work_card(&self, request: ClaimWorkCard) -> Result<ClaimId, AircError> {
        self.ensure_work_card_in_current_room(request.card_id)
            .await?;
        self.ensure_work_card_unclaimed(request.card_id).await?;
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
        self.ensure_work_card_in_current_room(request.card_id)
            .await?;
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

    /// Amend a work card's editable fields (title, body, priority)
    /// post-creation. Card 5ac0a359 — `None` on a field means "leave
    /// alone"; `body` is double-`Option` so the caller can
    /// distinguish "don't change body" from "clear the body".
    ///
    /// An all-`None` request still emits an event (with the latest
    /// `updated_at_ms`), which is useful as a liveness marker — a
    /// peer can "touch" a card to advertise that they're tracking it
    /// without changing semantics. The projection treats this as a
    /// pure `updated_at_ms` bump.
    ///
    /// Refuses on `WorkCardNotInCurrentRoom` so an amendment can't
    /// silently target a card from a different room — same guard
    /// `change_work_card_state` uses.
    pub async fn update_work_card(
        &self,
        request: UpdateWorkCard,
    ) -> Result<(), AircError> {
        self.ensure_work_card_in_current_room(request.card_id)
            .await?;
        let event = WorkEvent::CardUpdated(airc_work::event::CardUpdated {
            card_id: request.card_id,
            title: request.title,
            body: request.body,
            priority: request.priority,
            updated_by: self.peer_id(),
            updated_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Change a work card's lifecycle state through the work event
    /// stream. This is the queue hygiene path agents use to stop
    /// completed work from remaining claimable.
    pub async fn change_work_card_state(
        &self,
        request: ChangeWorkCardState,
    ) -> Result<(), AircError> {
        self.ensure_work_card_in_current_room(request.card_id)
            .await?;
        let event = WorkEvent::CardStateChanged(CardStateChanged {
            card_id: request.card_id,
            state: request.state,
            changed_by: self.peer_id(),
            changed_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Link a pull request to a work card (card 820629e9). Emits
    /// `WorkEvent::PullRequestLinked` whose projection atomically sets
    /// `card.pull_request = Some(pr)` and transitions
    /// `card.state = Review`. Used by `airc work state ... review`
    /// after `gh pr create` returns a PR number; future card-state
    /// observers don't need to ask "did the agent forget to fill the
    /// pr field" — the link IS the state transition.
    pub async fn link_card_pull_request(
        &self,
        request: LinkCardPullRequest,
    ) -> Result<(), AircError> {
        self.ensure_work_card_in_current_room(request.card_id)
            .await?;
        let event = WorkEvent::PullRequestLinked(airc_work::event::PullRequestLinked {
            card_id: request.card_id,
            pull_request: request.pull_request,
            linked_by: self.peer_id(),
            linked_at_ms: now_ms()?,
        });
        self.publish_work_event(&event).await?;
        Ok(())
    }

    /// Extend this peer's claim lease for a work card. Agents should
    /// heartbeat long-running work so stale claims become visible when
    /// a tab goes idle or dies instead of locking a lane indefinitely.
    pub async fn heartbeat_work_claim(&self, request: HeartbeatWorkClaim) -> Result<(), AircError> {
        self.ensure_work_card_in_current_room(request.card_id)
            .await?;
        let event = WorkEvent::ClaimHeartbeat(ClaimHeartbeat {
            card_id: request.card_id,
            claim_id: request.claim_id,
            owner: self.peer_id(),
            ttl_ms: request.ttl_ms,
            heartbeat_at_ms: now_ms()?,
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

    /// Publish this peer's current availability for a repo. This is a
    /// coordination signal, not a permission boundary: managers and
    /// peers use it to avoid idle lock by finding ready responders and
    /// detecting stale/busy agents without parsing recent chat.
    pub async fn report_agent_availability(
        &self,
        request: ReportAgentAvailability,
    ) -> Result<(), AircError> {
        let event = WorkEvent::AgentAvailabilityReported(AgentAvailabilityReported {
            repo: request.repo,
            peer: self.peer_id(),
            state: request.state,
            note: request.note,
            ttl_ms: request.ttl_ms,
            reported_at_ms: now_ms()?,
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

    /// Observe pull-request state via a caller-supplied
    /// [`PullRequestSource`] and publish any state changes as signed
    /// work events. Mirrors [`Airc::observe_local_git_workspace`]: the
    /// SDK owns the publish path, the caller owns the I/O surface.
    ///
    /// This is the substrate adapter boundary for PR/CI/review events
    /// — once a real gh-CLI source ships in a follow-up, agents,
    /// monitor renderers, Continuum/OpenClaw/Hermes all consume PR
    /// state changes through the same subscription stream they
    /// already use for chat and lifecycle.
    pub async fn observe_pull_requests<S: PullRequestSource>(
        &self,
        observer: &PullRequestObserver<S>,
        request: ObservePullRequests,
    ) -> Result<ObservedPullRequests, AircError> {
        let snapshot = observer.observe(&request.repo)?;
        let events = pull_request_events_since(
            request.previous.as_ref(),
            &snapshot,
            self.peer_id(),
            now_ms()?,
        );
        let mut emitted_event_ids = Vec::with_capacity(events.len());
        for event in events {
            emitted_event_ids.push(self.publish_work_event(&event).await?);
        }
        Ok(ObservedPullRequests {
            snapshot,
            emitted_event_ids,
        })
    }

    /// Rebuild the current room's work board from persisted work
    /// events. `limit` is the transcript page size for interactive
    /// board views; scheduling code should use
    /// [`Airc::work_board_complete`] instead.
    pub async fn work_board(&self, limit: usize) -> Result<WorkBoardProjection, AircError> {
        let room = self.current_room().await?;
        // Recent-window view (not the complete board): daemon reads the
        // most-recent `limit`; direct reads the recent store page.
        if self.is_daemon_attached() {
            // Card 79953b4d: heartbeats with the d4e3e350 coordination
            // payload (active_claims + doctrine_version) dominate the
            // recent-event window at scale. With three+ peers heart-
            // beating every 60s, a flat `limit`-event page fills with
            // heartbeats and squeezes card lifecycle events out — the
            // projection then misses recent cards even when they're
            // durably in the transcript. Filter to events carrying
            // HEADER_FORGE_WORK_EVENT_KIND (work-domain events only)
            // and over-fetch by WORK_BOARD_FETCH_MULTIPLIER so the
            // projection sees `limit` work events worth of history.
            let fetch_limit = limit.saturating_mul(WORK_BOARD_FETCH_MULTIPLIER);
            let transcripts: Vec<_> = self
                .daemon_page_recent(&room, fetch_limit)
                .await?
                .into_iter()
                .filter(is_work_event_transcript)
                .take(limit)
                .collect();
            return Ok(airc_work_store::project_transcripts(transcripts)?);
        }
        let store = WorkEventStore::new(self.event_store());
        Ok(store.project_recent(Some(room.channel), limit).await?)
    }

    /// Rebuild the current room's complete work board. Scheduling and
    /// mutation paths use this so old active cards do not disappear just
    /// because chat/status traffic pushed their creation event outside a
    /// recent transcript window.
    pub async fn work_board_complete(
        &self,
        page_size: usize,
    ) -> Result<WorkBoardProjection, AircError> {
        let room = self.current_room().await?;
        self.project_room_work_board(&room, page_size).await
    }

    /// Project `room`'s work board, reading work events from the daemon
    /// when attached (the one same-machine path) or the local store
    /// otherwise. Centralises the read so the whole work subsystem is
    /// daemon-aware — work is event-sourced, so its reads must follow the
    /// same path as its writes (`send_frame` → daemon when attached).
    async fn project_room_work_board(
        &self,
        room: &crate::Room,
        page_size: usize,
    ) -> Result<WorkBoardProjection, AircError> {
        if self.is_daemon_attached() {
            let transcripts = self
                .daemon_room_transcripts(room.channel, page_size)
                .await?;
            return Ok(airc_work_store::project_transcripts(transcripts)?);
        }
        let store = WorkEventStore::new(self.event_store());
        Ok(store
            .project_complete(Some(room.channel), page_size)
            .await?)
    }

    /// Return work cards this peer could reasonably take next. This
    /// is the typed scheduling surface agents/monitors should use
    /// instead of scraping `airc work board` output.
    pub async fn claimable_work(
        &self,
        query: ClaimableWorkQuery,
    ) -> Result<Vec<ClaimableWorkItem>, AircError> {
        Ok(self.work_queue_status(query.into()).await?.claimable)
    }

    /// Return the scheduling view agents need to avoid idle lock:
    /// claimable cards, current peer claims, and typed ready/busy/away
    /// availability records. Consumers should use this instead of
    /// parsing CLI output.
    pub async fn work_queue_status(
        &self,
        query: WorkQueueStatusQuery,
    ) -> Result<WorkQueueStatus, AircError> {
        let board = self.work_board_complete(query.event_limit).await?;
        let now_ms = now_ms()?;
        let stale_claims = board.stale_claims(now_ms);
        let snapshot = board.snapshot();
        let peer_id = self.peer_id();

        let mut items = Vec::new();
        let mut active_claims_for_peer = Vec::new();
        for card in snapshot.cards {
            if card.priority > query.max_priority {
                continue;
            }
            if query.repo.as_ref().is_some_and(|repo| &card.repo != repo) {
                continue;
            }

            if card.owner == Some(peer_id)
                && card
                    .claim_expires_at_ms
                    .is_some_and(|expires_at_ms| expires_at_ms > now_ms)
            {
                active_claims_for_peer.push(card.clone());
            }

            let stale_claim = stale_claims
                .iter()
                .find(|claim| claim.card_id == card.card_id)
                .cloned();
            let open = card.state == airc_work::CardState::Open && card.claim_id.is_none();
            let stale_claimable = query.include_stale_claims
                && stale_claim.is_some()
                && !matches!(
                    card.state,
                    airc_work::CardState::Merged | airc_work::CardState::Closed
                );
            if !open && !stale_claimable {
                continue;
            }

            items.push(ClaimableWorkItem { card, stale_claim });
        }

        items.sort_by(|left, right| {
            left.card
                .priority
                .cmp(&right.card.priority)
                .then_with(|| right.is_stale_claim().cmp(&left.is_stale_claim()))
                .then_with(|| left.card.updated_at_ms.cmp(&right.card.updated_at_ms))
                .then_with(|| left.card.card_id.cmp(&right.card.card_id))
        });
        items.truncate(query.limit);

        let mut agent_availability: Vec<_> = snapshot
            .agent_availability
            .into_iter()
            .filter(|record| {
                query
                    .repo
                    .as_ref()
                    .is_none_or(|repo| &record.report.repo == repo)
            })
            .collect();
        agent_availability.sort_by(|left, right| {
            left.report
                .repo
                .cmp(&right.report.repo)
                .then_with(|| {
                    availability_state_rank(left.report.state)
                        .cmp(&availability_state_rank(right.report.state))
                })
                .then_with(|| left.expires_at_ms.cmp(&right.expires_at_ms))
                .then_with(|| {
                    left.report
                        .peer
                        .to_string()
                        .cmp(&right.report.peer.to_string())
                })
        });

        active_claims_for_peer.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.updated_at_ms.cmp(&right.updated_at_ms))
                .then_with(|| left.card_id.cmp(&right.card_id))
        });

        Ok(WorkQueueStatus {
            claimable: items,
            agent_availability,
            active_claims_for_peer,
        })
    }

    async fn publish_work_event(&self, event: &WorkEvent) -> Result<EventId, AircError> {
        let (headers, body) = encode_work_event(event)?;
        self.send_frame(FrameKind::Event, body, headers).await
    }

    async fn ensure_work_card_in_current_room(&self, card_id: WorkCardId) -> Result<(), AircError> {
        let room = self.current_room().await?;
        let board = self
            .project_room_work_board(&room, WORK_MUTATION_PAGE_SIZE)
            .await?;
        if board.card(card_id).is_some() {
            return Ok(());
        }

        Err(AircError::WorkCardNotInCurrentRoom {
            card_id,
            room_name: room.name,
            room_id: room.channel,
        })
    }

    async fn ensure_work_card_unclaimed(&self, card_id: WorkCardId) -> Result<(), AircError> {
        let room = self.current_room().await?;
        let board = self
            .project_room_work_board(&room, WORK_MUTATION_PAGE_SIZE)
            .await?;
        let Some(card) = board.card(card_id) else {
            return Err(AircError::WorkCardNotInCurrentRoom {
                card_id,
                room_name: room.name,
                room_id: room.channel,
            });
        };
        let now_ms = now_ms()?;
        if card.claim_id.is_none()
            || card
                .claim_expires_at_ms
                .is_some_and(|expires_at_ms| expires_at_ms <= now_ms)
        {
            return Ok(());
        }

        Err(AircError::WorkCardAlreadyClaimed {
            card_id,
            claim_id: card.claim_id,
            owner: card.owner,
        })
    }
}

fn availability_state_rank(state: AgentAvailabilityState) -> u8 {
    match state {
        AgentAvailabilityState::Ready => 0,
        AgentAvailabilityState::Busy => 1,
        AgentAvailabilityState::Away => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::headers::Headers;
    use airc_core::{ClientId, EventId, MentionTarget, RoomId, TranscriptKind};

    fn event_with_headers(headers: Headers) -> airc_core::TranscriptEvent {
        airc_core::TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: airc_core::PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::System,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers,
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn work_event_transcript_filter_pins_header_presence_not_body_shape() {
        // Card 79953b4d: at scale, heartbeats with the d4e3e350
        // coordination payload share the recent-event window. The
        // distinguishing signal is HEADER_FORGE_WORK_EVENT_KIND —
        // every work event carries it, heartbeats / chat / generic
        // lifecycle do not.

        // Work event: header present → keep.
        let mut work_headers = Headers::new();
        work_headers.insert(
            airc_work::HEADER_FORGE_WORK_EVENT_KIND.to_owned(),
            "card_created".to_owned(),
        );
        assert!(is_work_event_transcript(&event_with_headers(work_headers)));

        // Empty headers (e.g. raw heartbeat alive) → drop.
        assert!(!is_work_event_transcript(&event_with_headers(Headers::new())));

        // Unrelated header only (e.g. a chat msg with bridge headers) → drop.
        let mut other_headers = Headers::new();
        other_headers.insert("airc.bridge.source".to_owned(), "slack".to_owned());
        assert!(!is_work_event_transcript(&event_with_headers(other_headers)));

        // Mixed headers including the work-kind header → keep
        // (work events often carry several headers like
        // forge.work.card_id alongside forge.work.kind).
        let mut mixed_headers = Headers::new();
        mixed_headers.insert(
            airc_work::HEADER_FORGE_WORK_EVENT_KIND.to_owned(),
            "card_claimed".to_owned(),
        );
        mixed_headers.insert("forge.work.card_id".to_owned(), "abc".to_owned());
        assert!(is_work_event_transcript(&event_with_headers(mixed_headers)));
    }
}
