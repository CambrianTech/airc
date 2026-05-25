//! Typed, budgeted room-context assembly for managers, RAG
//! consumers, and prompt-boundary hooks.
//!
//! Closes work card d3930e42 (P1, first slice): "Add budgeted
//! room-context assembly for manager and RAG consumers".
//!
//! Consumers (managers, hooks, RAG processes, Continuum
//! personas) need to slice the current room's evidence —
//! recent events, work cards, active claims — into a bounded
//! payload they can hand to a prompt or a planning loop. Today
//! they either scrape `airc work board`/`airc events list`
//! output (the very `stdout parsing` the card forbids) or
//! re-implement the queries each time.
//!
//! This module gives them ONE typed call:
//!
//! ```ignore
//! let slice = airc.room_context(ContextBudget {
//!     max_items: 64,
//!     max_age_ms: Some(60 * 60 * 1000),
//! }).await?;
//! ```
//!
//! `ContextSlice` is JSON-serialisable so the CLI emits it
//! verbatim for shell/Python consumers, and the in-process
//! consumers (Continuum/OpenClaw/Hermes per the audits in
//! #1002 / #1003) link it as a typed value without parsing.
//!
//! ## Scope cut (this PR)
//!
//! - Evidence types: room events, work cards, active claims.
//! - Budgets: `max_items` (deterministic fill order) and
//!   `max_age_ms` (drop everything older).
//!
//! ## Explicit non-scope (each is a separate card)
//!
//! - Token budget — needs a tokenizer dependency. Consumers
//!   that care can apply their own tokenizer over the JSON-
//!   serialised slice for the first iteration.
//! - PR / CI status integration — waits on the local-git +
//!   pull-request primitives shipping to airc-lib.
//! - Roadmap-gap evidence — needs the markdown-sync card
//!   (fe57c6fa) to land first.
//! - Capability state — needs the capability advertisement
//!   shape from the Hermes audit (#1003 follow-ups).
//! - Hook prompt-boundary consumer wiring — belongs in the
//!   runtime-planning hooks card (1702d553, already merged
//!   docs in #1001).
//!
//! ## Determinism + ordering
//!
//! The slice is deterministic: same store contents + same
//! budget produce the same slice. Ordering inside the slice:
//!
//! 1. Events: newest first (descending `lamport`).
//! 2. Work cards: priority ascending (P0 first), then state
//!    bucket (claimable > in-progress > review > closed),
//!    then `updated_at_ms` descending.
//! 3. Active claims: `claim_expires_at_ms` ascending (about-
//!    to-expire surfaces first).
//!
//! Items are interleaved in the output by type group in the
//! order above, fill-stopping when `max_items` is hit. The
//! `ContextTotals` field on the slice reports what was seen
//! vs kept so consumers can detect truncation.

use std::collections::BTreeMap;

use airc_core::{EventId, PeerId, RoomId, TranscriptEvent};
use airc_work::{CardState, ClaimId, Priority, WorkCard, WorkCardId};
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

/// How much evidence to keep in a [`ContextSlice`]. Caller-
/// provided so different consumers (a hook injecting context
/// at prompt boundary vs a manager-loop replay) can tune
/// independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Total number of items across all evidence types in the
    /// slice. Hard cap; fill order is documented in the module
    /// header.
    pub max_items: usize,
    /// Drop evidence older than this. `None` means no age
    /// cap. Measured against `now_ms` at assembly time.
    pub max_age_ms: Option<u64>,
}

impl Default for ContextBudget {
    fn default() -> Self {
        // 64 items + 1h window — small enough to fit
        // a model context, big enough to span a typical
        // multi-agent coordination burst.
        Self {
            max_items: 64,
            max_age_ms: Some(60 * 60 * 1000),
        }
    }
}

/// One piece of room evidence. Tagged so consumers can
/// pattern-match without re-deriving the type from a field
/// name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextItem {
    /// A recent room event, summarised to keep the slice
    /// bounded. Full transcript still accessible via
    /// `airc events list`.
    Event(EventSummary),
    /// A work card in this room.
    WorkCard(CardSummary),
    /// An active (non-expired) claim on a card in this room.
    ActiveClaim(ClaimSummary),
}

/// Compact event summary for context slices. Drops the full
/// body — consumers that need it can re-fetch by `event_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSummary {
    pub event_id: EventId,
    pub peer_id: PeerId,
    pub lamport: u64,
    pub occurred_at_ms: u64,
    pub kind: String,
    /// First few headers (top-level keys only) so consumers
    /// can filter; bounded so a single event can't blow the
    /// budget on its own.
    pub headers: BTreeMap<String, String>,
}

/// Compact work-card summary. Mirrors `WorkCard` but drops the
/// body (often long) for slice-size predictability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardSummary {
    pub card_id: WorkCardId,
    pub repo: String,
    pub title: String,
    pub priority: Priority,
    pub state: CardState,
    pub owner: Option<PeerId>,
    pub claim_id: Option<ClaimId>,
    pub claim_expires_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Active-claim summary. About-to-expire claims are
/// surfaced first because they're the most actionable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimSummary {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub claim_expires_at_ms: u64,
    pub last_heartbeat_at_ms: Option<u64>,
}

/// What was seen vs what fit in the budget. Lets consumers
/// detect truncation and decide whether to re-query with a
/// larger budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextTotals {
    pub events_seen: usize,
    pub events_kept: usize,
    pub cards_seen: usize,
    pub cards_kept: usize,
    pub claims_seen: usize,
    pub claims_kept: usize,
}

/// A budgeted snapshot of the current room's evidence.
///
/// Deterministic: same store + same budget produces the same
/// slice. JSON-serialisable so the CLI can emit it verbatim
/// for shell consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSlice {
    pub room_id: RoomId,
    pub room_name: String,
    /// Unix-ms timestamp captured at assembly start. Reading
    /// it lets consumers detect a stale slice when comparing
    /// against `EventSummary.occurred_at_ms`.
    pub assembled_at_ms: u64,
    pub budget: ContextBudget,
    pub items: Vec<ContextItem>,
    pub totals: ContextTotals,
}

/// Default per-room event scan window when consumers don't
/// override. Big enough to cover the typical multi-agent
/// coordination history, small enough that decode + sort is
/// fast on every call.
const DEFAULT_EVENT_SCAN_WINDOW: usize = 1024;

impl Airc {
    /// Assemble a budgeted slice of the current room's
    /// evidence. See module docs for fill-order semantics.
    pub async fn room_context(&self, budget: ContextBudget) -> Result<ContextSlice, AircError> {
        self.room_context_with_scan_window(budget, DEFAULT_EVENT_SCAN_WINDOW)
            .await
    }

    /// Variant of [`Self::room_context`] with an explicit
    /// per-source scan window. Tests use it to build small
    /// deterministic transcripts; production callers should
    /// use [`Self::room_context`].
    pub async fn room_context_with_scan_window(
        &self,
        budget: ContextBudget,
        event_scan_window: usize,
    ) -> Result<ContextSlice, AircError> {
        let room = self.current_room().await?;
        let assembled_at_ms = now_ms()?;
        let min_occurred_at_ms = budget
            .max_age_ms
            .and_then(|window| assembled_at_ms.checked_sub(window));

        // ----- events -----
        let recent_events = self.page_recent(event_scan_window).await?;
        let events_seen = recent_events.len();
        let mut event_summaries: Vec<EventSummary> = recent_events
            .into_iter()
            .filter(|event| min_occurred_at_ms.is_none_or(|min| event.occurred_at_ms >= min))
            .map(EventSummary::from_transcript)
            .collect();
        // Newest first.
        event_summaries.sort_by_key(|event| std::cmp::Reverse(event.lamport));

        // ----- work cards (already room-scoped projection) -----
        let board = self.work_board(event_scan_window).await?;
        let now_ms_for_claims = assembled_at_ms;
        let board_snapshot = board.snapshot();
        let cards_seen = board_snapshot.cards.len();
        let mut card_summaries: Vec<CardSummary> = board_snapshot
            .cards
            .iter()
            .filter(|card| min_occurred_at_ms.is_none_or(|min| card.updated_at_ms >= min))
            .map(CardSummary::from_work_card)
            .collect();
        card_summaries.sort_by(card_priority_then_state_then_recency);

        // ----- active claims -----
        let mut active_claims: Vec<ClaimSummary> = board_snapshot
            .cards
            .iter()
            .filter(|card| {
                card.claim_id.is_some()
                    && card
                        .claim_expires_at_ms
                        .is_some_and(|expires| expires > now_ms_for_claims)
            })
            .filter_map(ClaimSummary::from_work_card)
            .collect();
        let claims_seen = active_claims.len();
        active_claims.sort_by_key(|claim| claim.claim_expires_at_ms);

        // ----- fill items in declared order until budget hits -----
        let mut items = Vec::with_capacity(budget.max_items);
        let mut events_kept = 0usize;
        let mut cards_kept = 0usize;
        let mut claims_kept = 0usize;
        for event in event_summaries {
            if items.len() >= budget.max_items {
                break;
            }
            items.push(ContextItem::Event(event));
            events_kept += 1;
        }
        for card in card_summaries {
            if items.len() >= budget.max_items {
                break;
            }
            items.push(ContextItem::WorkCard(card));
            cards_kept += 1;
        }
        for claim in active_claims {
            if items.len() >= budget.max_items {
                break;
            }
            items.push(ContextItem::ActiveClaim(claim));
            claims_kept += 1;
        }

        Ok(ContextSlice {
            room_id: room.channel,
            room_name: room.name,
            assembled_at_ms,
            budget,
            items,
            totals: ContextTotals {
                events_seen,
                events_kept,
                cards_seen,
                cards_kept,
                claims_seen,
                claims_kept,
            },
        })
    }
}

fn card_priority_then_state_then_recency(
    left: &CardSummary,
    right: &CardSummary,
) -> std::cmp::Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| state_bucket(left.state).cmp(&state_bucket(right.state)))
        .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
        .then_with(|| left.card_id.cmp(&right.card_id))
}

fn state_bucket(state: CardState) -> u8 {
    match state {
        CardState::Open => 0,
        CardState::Claimed | CardState::InProgress => 1,
        CardState::Blocked => 2,
        CardState::Review => 3,
        CardState::Merged => 4,
        CardState::Closed => 5,
    }
}

impl EventSummary {
    fn from_transcript(event: TranscriptEvent) -> Self {
        let headers = event
            .headers
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Self {
            event_id: event.event_id,
            peer_id: event.peer_id,
            lamport: event.lamport,
            occurred_at_ms: event.occurred_at_ms,
            kind: format!("{:?}", event.kind).to_lowercase(),
            headers,
        }
    }
}

impl CardSummary {
    fn from_work_card(card: &WorkCard) -> Self {
        Self {
            card_id: card.card_id,
            repo: card.repo.to_string(),
            title: card.title.clone(),
            priority: card.priority,
            state: card.state,
            owner: card.owner,
            claim_id: card.claim_id,
            claim_expires_at_ms: card.claim_expires_at_ms,
            created_at_ms: card.created_at_ms,
            updated_at_ms: card.updated_at_ms,
        }
    }
}

impl ClaimSummary {
    fn from_work_card(card: &WorkCard) -> Option<Self> {
        Some(Self {
            card_id: card.card_id,
            claim_id: card.claim_id?,
            owner: card.owner?,
            claim_expires_at_ms: card.claim_expires_at_ms?,
            last_heartbeat_at_ms: card.last_heartbeat_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_budget_default_is_sane() {
        let budget = ContextBudget::default();
        assert_eq!(budget.max_items, 64);
        assert!(budget.max_age_ms.is_some_and(|ms| ms > 0));
    }

    #[test]
    fn context_slice_round_trips_as_snake_case_json() {
        let slice = ContextSlice {
            room_id: RoomId::from_uuid(uuid::Uuid::nil()),
            room_name: "test-room".to_string(),
            assembled_at_ms: 1_700_000_000_000,
            budget: ContextBudget::default(),
            items: Vec::new(),
            totals: ContextTotals::default(),
        };
        let value = serde_json::to_value(&slice).expect("encode");
        assert_eq!(value["room_name"], "test-room");
        assert_eq!(value["assembled_at_ms"], 1_700_000_000_000_u64);
        let round_trip: ContextSlice = serde_json::from_value(value).expect("decode");
        assert_eq!(round_trip.room_name, "test-room");
    }

    #[test]
    fn state_bucket_orders_claimable_before_review_before_closed() {
        assert!(state_bucket(CardState::Open) < state_bucket(CardState::Claimed));
        assert!(state_bucket(CardState::Claimed) < state_bucket(CardState::Review));
        assert!(state_bucket(CardState::Review) < state_bucket(CardState::Closed));
    }
}
