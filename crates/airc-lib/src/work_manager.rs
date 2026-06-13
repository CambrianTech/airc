//! Typed manager-loop evaluation for work coordination.
//!
//! This module does not run a bot and does not parse command output.
//! It composes the work queue and roster projections into a stable
//! scheduling view that agents, monitors, and future manager personas
//! can consume directly.

use airc_work::{CardState, LaneId, Priority, RepoId, StaleClaim, WorkCard, WorkCardId};

use crate::time::now_ms;
use crate::{
    AgentAvailabilityState, Airc, AircError, CreateWorkCard, WorkQueueStatus, WorkQueueStatusQuery,
    WorkRosterQuery, WorkRosterRow, WorkRosterStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkManagerQuery {
    pub repo: Option<RepoId>,
    pub max_priority: Priority,
    pub include_stale_claims: bool,
    pub event_limit: usize,
    pub limit: usize,
    pub active_within_ms: u64,
}

impl Default for WorkManagerQuery {
    fn default() -> Self {
        Self {
            repo: None,
            max_priority: Priority::P1,
            include_stale_claims: true,
            event_limit: 512,
            limit: 8,
            active_within_ms: 180_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkManagerStatus {
    pub queue: WorkQueueStatus,
    pub roster: WorkRosterStatus,
    pub recommendations: Vec<WorkManagerRecommendation>,
}

impl WorkManagerStatus {
    pub fn needs_backlog_seed(&self) -> bool {
        self.recommendations
            .iter()
            .any(|recommendation| recommendation.kind == WorkManagerRecommendationKind::SeedBacklog)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkManagerRecommendation {
    pub kind: WorkManagerRecommendationKind,
    pub reason: WorkManagerReason,
    pub card: Option<WorkCard>,
    pub stale_claim: Option<StaleClaim>,
    pub agent: Option<WorkManagerAgent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkManagerRecommendationKind {
    ClaimWork,
    RecoverStaleClaim,
    PublishAvailability,
    SeedBacklog,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkManagerReason {
    ClaimableCardAvailable,
    StaleClaimAvailable,
    LiveAgentHasNoAvailability,
    LiveIdleAgentsAndNoClaimableWork,
    NoLiveAgents,
    AllLiveAgentsBusyOrClaimed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkManagerAgent {
    pub peer: crate::PeerId,
    pub client_id: Option<String>,
    pub runtime: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBacklogSeedCandidate {
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
    /// Stable source identifier supplied by a roadmap/RAG/issue
    /// adapter. AIRC stores it in the typed result for auditability;
    /// duplicate suppression remains repo+title+lane so adapters can
    /// change evidence wording without creating duplicate work.
    pub evidence_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBacklogSeedResult {
    pub items: Vec<SeededWorkCard>,
}

impl WorkBacklogSeedResult {
    pub fn created_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.outcome == WorkBacklogSeedOutcome::Created)
            .count()
    }

    pub fn represented_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.outcome == WorkBacklogSeedOutcome::AlreadyRepresented)
            .count()
    }

    pub fn completed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.outcome == WorkBacklogSeedOutcome::AlreadyCompleted)
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeededWorkCard {
    pub candidate: WorkBacklogSeedCandidate,
    pub card_id: WorkCardId,
    pub outcome: WorkBacklogSeedOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkBacklogSeedOutcome {
    Created,
    AlreadyRepresented,
    AlreadyCompleted,
}

impl Airc {
    /// Return a typed manager-loop evaluation. This is the substrate
    /// surface for "why is the room idle?" and "what should happen
    /// next?" Consumers should use this instead of scraping
    /// `airc work next` / `airc work roster` output.
    pub async fn work_manager_status(
        &self,
        query: WorkManagerQuery,
    ) -> Result<WorkManagerStatus, AircError> {
        let queue = self
            .work_queue_status(WorkQueueStatusQuery {
                repo: query.repo.clone(),
                max_priority: query.max_priority,
                include_stale_claims: query.include_stale_claims,
                event_limit: query.event_limit,
                limit: query.limit,
            })
            .await?;
        let roster = self
            .work_roster_status(WorkRosterQuery {
                repo: query.repo,
                event_limit: query.event_limit,
                active_within_ms: query.active_within_ms,
            })
            .await?;
        let recommendations = evaluate_manager_recommendations(&queue, &roster);
        Ok(WorkManagerStatus {
            queue,
            roster,
            recommendations,
        })
    }

    /// Idempotently materialize typed backlog candidates into the
    /// current room. This is the manager flywheel's write boundary:
    /// roadmap/RAG/issue adapters propose candidates; AIRC creates
    /// only the missing cards and treats represented/completed cards
    /// as the recursion base case.
    pub async fn seed_work_backlog(
        &self,
        candidates: Vec<WorkBacklogSeedCandidate>,
    ) -> Result<WorkBacklogSeedResult, AircError> {
        let mut board = self.work_board_complete(512).await?;
        let mut items = Vec::with_capacity(candidates.len());

        for candidate in candidates {
            let snapshot = board.snapshot();
            if let Some(card) = find_seed_match(&snapshot.cards, &candidate) {
                let outcome = if work_card_is_complete(card.state) {
                    WorkBacklogSeedOutcome::AlreadyCompleted
                } else {
                    WorkBacklogSeedOutcome::AlreadyRepresented
                };
                items.push(SeededWorkCard {
                    candidate,
                    card_id: card.card_id,
                    outcome,
                });
                continue;
            }

            let card_id = self
                .create_work_card(CreateWorkCard {
                    repo: candidate.repo.clone(),
                    title: candidate.title.clone(),
                    body: candidate.body.clone(),
                    priority: candidate.priority,
                    lane_id: candidate.lane_id,
                    reviews: None,
                })
                .await?;
            items.push(SeededWorkCard {
                candidate,
                card_id,
                outcome: WorkBacklogSeedOutcome::Created,
            });
            board = self.work_board_complete(512).await?;
        }

        Ok(WorkBacklogSeedResult { items })
    }
}

pub fn evaluate_manager_recommendations(
    queue: &WorkQueueStatus,
    roster: &WorkRosterStatus,
) -> Vec<WorkManagerRecommendation> {
    let mut recommendations = Vec::new();
    let idle_agents = idle_live_agents(roster);

    for item in &queue.claimable {
        if let Some(stale_claim) = &item.stale_claim {
            recommendations.push(WorkManagerRecommendation {
                kind: WorkManagerRecommendationKind::RecoverStaleClaim,
                reason: WorkManagerReason::StaleClaimAvailable,
                card: Some(item.card.clone()),
                stale_claim: Some(stale_claim.clone()),
                agent: idle_agents.first().cloned(),
            });
        } else {
            recommendations.push(WorkManagerRecommendation {
                kind: WorkManagerRecommendationKind::ClaimWork,
                reason: WorkManagerReason::ClaimableCardAvailable,
                card: Some(item.card.clone()),
                stale_claim: None,
                agent: idle_agents.first().cloned(),
            });
        }
    }

    for row in live_rows_without_availability(roster) {
        recommendations.push(WorkManagerRecommendation {
            kind: WorkManagerRecommendationKind::PublishAvailability,
            reason: WorkManagerReason::LiveAgentHasNoAvailability,
            card: None,
            stale_claim: None,
            agent: Some(agent_from_row(row)),
        });
    }

    if queue.claimable.is_empty() {
        if roster.alive_count() == 0 {
            recommendations.push(WorkManagerRecommendation {
                kind: WorkManagerRecommendationKind::Wait,
                reason: WorkManagerReason::NoLiveAgents,
                card: None,
                stale_claim: None,
                agent: None,
            });
        } else if idle_agents.is_empty() {
            recommendations.push(WorkManagerRecommendation {
                kind: WorkManagerRecommendationKind::Wait,
                reason: WorkManagerReason::AllLiveAgentsBusyOrClaimed,
                card: None,
                stale_claim: None,
                agent: None,
            });
        } else {
            recommendations.push(WorkManagerRecommendation {
                kind: WorkManagerRecommendationKind::SeedBacklog,
                reason: WorkManagerReason::LiveIdleAgentsAndNoClaimableWork,
                card: None,
                stale_claim: None,
                agent: idle_agents.first().cloned(),
            });
        }
    }

    recommendations
}

fn idle_live_agents(roster: &WorkRosterStatus) -> Vec<WorkManagerAgent> {
    let now_ms = now_ms().unwrap_or(0);
    roster
        .rows
        .iter()
        .filter(|row| row.liveness.is_some())
        .filter(|row| row.active_claims.is_empty())
        .filter(|row| {
            row.availability
                .as_ref()
                .is_none_or(|record| record.expires_at_ms <= now_ms)
                || row.availability.as_ref().is_some_and(|record| {
                    record.report.state == AgentAvailabilityState::Ready
                        && record.expires_at_ms > now_ms
                })
        })
        .map(agent_from_row)
        .collect()
}

fn live_rows_without_availability(roster: &WorkRosterStatus) -> Vec<&WorkRosterRow> {
    let now_ms = now_ms().unwrap_or(0);
    roster
        .rows
        .iter()
        .filter(|row| row.liveness.is_some())
        .filter(|row| {
            row.availability
                .as_ref()
                .is_none_or(|record| record.expires_at_ms <= now_ms)
        })
        .collect()
}

fn agent_from_row(row: &WorkRosterRow) -> WorkManagerAgent {
    WorkManagerAgent {
        peer: row.peer,
        client_id: row.client_id.clone(),
        runtime: row
            .liveness
            .as_ref()
            .map(|liveness| liveness.runtime.clone()),
        scope: row
            .liveness
            .as_ref()
            .and_then(|liveness| liveness.scope.clone()),
    }
}

fn find_seed_match<'a>(
    cards: &'a [WorkCard],
    candidate: &WorkBacklogSeedCandidate,
) -> Option<&'a WorkCard> {
    let title = normalize_seed_title(&candidate.title);
    cards.iter().find(|card| {
        card.repo == candidate.repo
            && card.lane_id == candidate.lane_id
            && normalize_seed_title(&card.title) == title
    })
}

fn normalize_seed_title(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn work_card_is_complete(state: CardState) -> bool {
    matches!(state, CardState::Merged | CardState::Closed)
}

#[cfg(test)]
mod tests {
    use airc_core::PeerId;
    use airc_work::{
        AgentAvailabilityRecord, AgentAvailabilityReported, AgentAvailabilityState, CardState,
        Priority, RepoId, WorkCard, WorkCardId,
    };

    use crate::agent_heartbeat::AgentLiveness;
    use crate::ClaimableWorkItem;

    use super::*;

    #[test]
    fn recommends_claim_when_card_and_idle_agent_exist() {
        let card = work_card("claimable");
        let status = evaluate_manager_recommendations(
            &queue(vec![ClaimableWorkItem {
                card: card.clone(),
                stale_claim: None,
            }]),
            &roster(vec![live_row(PeerId::new(), None, Vec::new())]),
        );

        assert_eq!(status[0].kind, WorkManagerRecommendationKind::ClaimWork);
        assert_eq!(
            status[0].card.as_ref().map(|card| card.card_id),
            Some(card.card_id)
        );
        assert!(status[0].agent.is_some());
    }

    #[test]
    fn recommends_backlog_seed_when_live_idle_agents_have_no_work() {
        let status = WorkManagerStatus {
            queue: queue(Vec::new()),
            roster: roster(vec![live_row(PeerId::new(), None, Vec::new())]),
            recommendations: evaluate_manager_recommendations(
                &queue(Vec::new()),
                &roster(vec![live_row(PeerId::new(), None, Vec::new())]),
            ),
        };

        assert!(status.needs_backlog_seed());
        assert!(status.recommendations.iter().any(|recommendation| {
            recommendation.reason == WorkManagerReason::LiveIdleAgentsAndNoClaimableWork
        }));
    }

    #[test]
    fn waits_when_every_live_agent_is_busy_or_claimed() {
        let peer = PeerId::new();
        let status = evaluate_manager_recommendations(
            &queue(Vec::new()),
            &roster(vec![live_row(
                peer,
                Some(availability(peer, AgentAvailabilityState::Busy)),
                vec![work_card("active")],
            )]),
        );

        assert_eq!(
            status.last().map(|recommendation| recommendation.reason),
            Some(WorkManagerReason::AllLiveAgentsBusyOrClaimed)
        );
    }

    fn queue(claimable: Vec<ClaimableWorkItem>) -> WorkQueueStatus {
        WorkQueueStatus {
            claimable,
            agent_availability: Vec::new(),
            active_claims_for_peer: Vec::new(),
        }
    }

    fn roster(rows: Vec<WorkRosterRow>) -> WorkRosterStatus {
        WorkRosterStatus {
            rows,
            claimable_count: 0,
        }
    }

    fn live_row(
        peer: PeerId,
        availability: Option<AgentAvailabilityRecord>,
        active_claims: Vec<WorkCard>,
    ) -> WorkRosterRow {
        WorkRosterRow {
            peer,
            client_id: Some("agent:test".to_string()),
            liveness: Some(AgentLiveness {
                peer,
                client_id: Some("agent:test".to_string()),
                build: Some("test-build".to_string()),
                runtime: "agent".to_string(),
                scope: Some("/tmp/agent".to_string()),
                last_seen_ms: now_ms().unwrap_or(1),
                coordination: Default::default(),
            }),
            availability,
            active_claims,
        }
    }

    fn availability(peer: PeerId, state: AgentAvailabilityState) -> AgentAvailabilityRecord {
        let now_ms = now_ms().unwrap_or(1);
        AgentAvailabilityRecord {
            report: AgentAvailabilityReported {
                repo: RepoId::new("CambrianTech/airc").unwrap(),
                peer,
                state,
                note: None,
                ttl_ms: 60_000,
                reported_at_ms: now_ms,
            },
            expires_at_ms: now_ms + 60_000,
        }
    }

    fn work_card(title: &str) -> WorkCard {
        WorkCard {
            card_id: WorkCardId::new(),
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: title.to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: None,
            state: CardState::Open,
            owner: None,
            claim_id: None,
            claim_expires_at_ms: None,
            last_heartbeat_at_ms: None,
            pull_request: None,
            created_by: PeerId::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
            reviews: None,
        }
    }
}
