//! Typed roster projection for agent coordination.
//!
//! This module composes the existing work-board projection and
//! heartbeat query into one SDK object. Terminal commands render this
//! object for humans; integrations should consume the typed structs
//! directly.

use std::collections::BTreeMap;
use std::time::Duration;

use airc_core::PeerId;
use airc_work::{AgentAvailabilityRecord, AgentAvailabilityState, CardState, RepoId, WorkCard};

use crate::agent_heartbeat::AgentLiveness;
use crate::time::now_ms;
use crate::{Airc, AircError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRosterQuery {
    pub repo: Option<RepoId>,
    pub event_limit: usize,
    pub active_within_ms: u64,
}

impl Default for WorkRosterQuery {
    fn default() -> Self {
        Self {
            repo: None,
            event_limit: 512,
            active_within_ms: 180_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRosterStatus {
    pub rows: Vec<WorkRosterRow>,
    pub claimable_count: usize,
}

impl WorkRosterStatus {
    pub fn ready_count(&self, now_ms: u64) -> usize {
        self.availability_count(now_ms, AgentAvailabilityState::Ready)
    }

    pub fn busy_count(&self, now_ms: u64) -> usize {
        self.availability_count(now_ms, AgentAvailabilityState::Busy)
    }

    pub fn away_count(&self, now_ms: u64) -> usize {
        self.availability_count(now_ms, AgentAvailabilityState::Away)
    }

    pub fn stale_availability_count(&self, now_ms: u64) -> usize {
        self.rows
            .iter()
            .filter(|row| {
                row.availability
                    .as_ref()
                    .is_some_and(|record| record.expires_at_ms <= now_ms)
            })
            .count()
    }

    pub fn alive_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.liveness.is_some())
            .count()
    }

    fn availability_count(&self, now_ms: u64, state: AgentAvailabilityState) -> usize {
        self.rows
            .iter()
            .filter(|row| {
                row.availability
                    .as_ref()
                    .is_some_and(|record| record.expires_at_ms > now_ms)
                    && row
                        .availability
                        .as_ref()
                        .is_some_and(|record| record.report.state == state)
            })
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRosterRow {
    pub peer: PeerId,
    pub liveness: Option<AgentLiveness>,
    pub availability: Option<AgentAvailabilityRecord>,
    pub active_claims: Vec<WorkCard>,
}

impl Airc {
    /// Return the typed roster view used by managers, monitors, and
    /// agent loops: who is alive, who is ready/busy/away, and what
    /// each peer is already claiming.
    pub async fn work_roster_status(
        &self,
        query: WorkRosterQuery,
    ) -> Result<WorkRosterStatus, AircError> {
        let board = self.work_board(query.event_limit).await?;
        let snapshot = board.snapshot();
        let now_ms = now_ms()?;
        let active_agents = self
            .active_agents(
                Duration::from_millis(query.active_within_ms),
                query.event_limit,
            )
            .await?;

        let mut rows = RosterRows::new();
        let mut claimable_count = 0;

        for liveness in active_agents {
            rows.upsert_liveness(liveness);
        }
        for availability in snapshot.agent_availability {
            if query
                .repo
                .as_ref()
                .is_none_or(|repo| &availability.report.repo == repo)
            {
                rows.upsert_availability(availability);
            }
        }
        for card in snapshot.cards {
            if query.repo.as_ref().is_some_and(|repo| &card.repo != repo) {
                continue;
            }
            if card.state == CardState::Open && card.claim_id.is_none() {
                claimable_count += 1;
            } else if is_active_claim(&card, now_ms) {
                rows.add_active_claim(card);
            }
        }

        Ok(WorkRosterStatus {
            rows: rows.into_sorted_rows(now_ms),
            claimable_count,
        })
    }
}

struct RosterRows {
    rows: BTreeMap<String, WorkRosterRow>,
}

impl RosterRows {
    fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
        }
    }

    fn upsert_liveness(&mut self, liveness: AgentLiveness) {
        let peer = liveness.peer;
        self.row_mut(peer).liveness = Some(liveness);
    }

    fn upsert_availability(&mut self, availability: AgentAvailabilityRecord) {
        let peer = availability.report.peer;
        self.row_mut(peer).availability = Some(availability);
    }

    fn add_active_claim(&mut self, card: WorkCard) {
        let Some(owner) = card.owner else {
            return;
        };
        self.row_mut(owner).active_claims.push(card);
    }

    fn row_mut(&mut self, peer: PeerId) -> &mut WorkRosterRow {
        self.rows
            .entry(peer.to_string())
            .or_insert_with(|| WorkRosterRow {
                peer,
                liveness: None,
                availability: None,
                active_claims: Vec::new(),
            })
    }

    fn into_sorted_rows(self, now_ms: u64) -> Vec<WorkRosterRow> {
        let mut rows: Vec<_> = self.rows.into_values().collect();
        for row in &mut rows {
            row.active_claims.sort_by(|left, right| {
                left.priority
                    .cmp(&right.priority)
                    .then_with(|| left.updated_at_ms.cmp(&right.updated_at_ms))
                    .then_with(|| left.card_id.cmp(&right.card_id))
            });
        }
        rows.sort_by(|left, right| {
            roster_row_rank(left, now_ms)
                .cmp(&roster_row_rank(right, now_ms))
                .then_with(|| {
                    left.availability
                        .as_ref()
                        .map(|record| record.report.repo.to_string())
                        .cmp(
                            &right
                                .availability
                                .as_ref()
                                .map(|record| record.report.repo.to_string()),
                        )
                })
                .then_with(|| left.peer.to_string().cmp(&right.peer.to_string()))
        });
        rows
    }
}

fn is_active_claim(card: &WorkCard, now_ms: u64) -> bool {
    card.owner.is_some()
        && card.claim_id.is_some()
        && card
            .claim_expires_at_ms
            .is_some_and(|expires_at_ms| expires_at_ms > now_ms)
        && matches!(
            card.state,
            CardState::Claimed | CardState::InProgress | CardState::Blocked | CardState::Review
        )
}

fn roster_row_rank(row: &WorkRosterRow, now_ms: u64) -> u8 {
    if row.liveness.is_some()
        && row
            .availability
            .as_ref()
            .is_some_and(|record| record.expires_at_ms > now_ms)
    {
        row.availability
            .as_ref()
            .map(|record| availability_state_rank(record.report.state))
            .unwrap_or(3)
    } else if row.liveness.is_some() {
        3
    } else if row.availability.is_some() {
        4
    } else {
        5
    }
}

fn availability_state_rank(state: AgentAvailabilityState) -> u8 {
    match state {
        AgentAvailabilityState::Ready => 0,
        AgentAvailabilityState::Busy => 1,
        AgentAvailabilityState::Away => 2,
    }
}
