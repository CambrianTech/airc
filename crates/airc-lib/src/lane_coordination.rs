//! Typed lane-coordination events.
//!
//! Closes GRID-SUBSTRATE-AUDIT flaw #3 (#964): "prose-only lane
//! claims." Before this module, when one agent took a lane the only
//! way another agent learned about it was a free-text chat message
//! like "taking AIRC flaw #3 from #964" that the receiving agent had
//! to grep through inbox prose to discover.
//!
//! After this module, an agent calls
//! [`Airc::claim_lane`]/[`Airc::release_lane`]/[`Airc::complete_lane`]/
//! [`Airc::block_on_lane`] and the substrate emits a signed event
//! with stable headers any subscriber can filter on:
//!
//! - `airc.coord.kind` = `"claim" | "release" | "complete" | "block_on"`
//! - `airc.coord.lane_id` = caller-supplied stable id
//! - `airc.coord.pr` = optional PR number this lane became / blocks on
//!
//! Body is JSON-encoded [`LaneCoordinationEvent`]. Frame kind is
//! `Event` so the route policy treats this as control-class traffic
//! (the same class WebRTC signaling and other coordination metadata
//! use).
//!
//! Scope cut: this module ships the *contract* — typed events + a
//! query that returns the latest event per lane_id by replaying the
//! transcript. It does **not** ship automatic enforcement
//! ("can't double-claim", "must release before complete"), a
//! scheduler ("auto-assign me an unclaimed lane"), or a CLI surface
//! (`airc lane claim ...`). Each of those is its own follow-up
//! discussed in the PR description.

use std::sync::Arc;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, PeerId, TranscriptEvent};
use airc_protocol::FrameKind;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::Airc;

/// Header carrying the lane-action kind. Stable string so subscribers
/// can filter without parsing the body.
pub const HEADER_COORD_KIND: &str = "airc.coord.kind";
/// Header carrying the caller-supplied lane identifier.
pub const HEADER_COORD_LANE_ID: &str = "airc.coord.lane_id";
/// Header carrying an associated PR number (optional). Useful when a
/// claim resolves into a real PR — observers can join claim →
/// completion events to a concrete artifact.
pub const HEADER_COORD_PR: &str = "airc.coord.pr";

/// What an agent is asserting about a coordination lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneAction {
    /// "I am taking this lane and intend to ship it." Other agents
    /// should avoid duplicating effort.
    Claim,
    /// "I am abandoning this lane (handing back)." Cleans up after a
    /// claim if the original owner can no longer ship it.
    Release,
    /// "Lane is shipped." Emit alongside the resulting PR number so
    /// observers can close it off the active list.
    Complete,
    /// "I am blocked on another lane / PR." Useful when an agent
    /// can't progress until something else lands; surfaces the
    /// dependency to scheduling logic later.
    BlockOn,
}

impl LaneAction {
    pub fn header_value(self) -> &'static str {
        match self {
            LaneAction::Claim => "claim",
            LaneAction::Release => "release",
            LaneAction::Complete => "complete",
            LaneAction::BlockOn => "block_on",
        }
    }
}

/// One typed coordination message. Body of the event; the substrate
/// header `airc.coord.lane_id` carries the same `lane_id` so
/// subscribers can filter without decoding the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCoordinationEvent {
    pub action: LaneAction,
    pub lane_id: String,
    pub owner: PeerId,
    /// Free-text reason. Kept short — this is metadata, not a chat
    /// transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// PR this lane will become or is blocked on. Optional because
    /// claims are typically emitted before the PR exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// Lane this claim depends on. Only meaningful for
    /// `LaneAction::BlockOn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_lane_id: Option<String>,
}

impl LaneCoordinationEvent {
    pub fn matches_lane(&self, lane_id: &str) -> bool {
        self.lane_id == lane_id
    }
}

/// What the substrate knows about a lane based on the transcript so
/// far. Returned by [`Airc::lane_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneStatus {
    /// The latest event seen for this lane, by lamport. `None` if the
    /// lane has never appeared.
    pub latest: Option<LaneCoordinationEvent>,
    /// Full history (oldest → newest) for the lane in this query
    /// window.
    pub history: Vec<LaneCoordinationEvent>,
}

impl LaneStatus {
    pub fn is_claimed(&self) -> bool {
        matches!(
            self.latest.as_ref().map(|event| event.action),
            Some(LaneAction::Claim) | Some(LaneAction::BlockOn)
        )
    }

    pub fn is_complete(&self) -> bool {
        matches!(
            self.latest.as_ref().map(|event| event.action),
            Some(LaneAction::Complete)
        )
    }

    pub fn current_owner(&self) -> Option<PeerId> {
        self.latest.as_ref().map(|event| event.owner)
    }
}

impl Airc {
    /// Publish a "claim" coordination event for `lane_id`. Other
    /// agents subscribed to the substrate stream see the typed
    /// event; their scheduling logic (or the human watching the
    /// inbox) avoids double-taking the lane.
    pub async fn claim_lane(
        &self,
        lane_id: impl Into<String>,
        rationale: Option<String>,
    ) -> Result<(), AircError> {
        self.publish_lane_event(LaneCoordinationEvent {
            action: LaneAction::Claim,
            lane_id: lane_id.into(),
            owner: self.peer_id(),
            rationale,
            pr_number: None,
            blocked_on_lane_id: None,
        })
        .await
    }

    pub async fn release_lane(
        &self,
        lane_id: impl Into<String>,
        rationale: Option<String>,
    ) -> Result<(), AircError> {
        self.publish_lane_event(LaneCoordinationEvent {
            action: LaneAction::Release,
            lane_id: lane_id.into(),
            owner: self.peer_id(),
            rationale,
            pr_number: None,
            blocked_on_lane_id: None,
        })
        .await
    }

    pub async fn complete_lane(
        &self,
        lane_id: impl Into<String>,
        pr_number: u64,
    ) -> Result<(), AircError> {
        self.publish_lane_event(LaneCoordinationEvent {
            action: LaneAction::Complete,
            lane_id: lane_id.into(),
            owner: self.peer_id(),
            rationale: None,
            pr_number: Some(pr_number),
            blocked_on_lane_id: None,
        })
        .await
    }

    pub async fn block_on_lane(
        &self,
        lane_id: impl Into<String>,
        blocked_on_lane_id: impl Into<String>,
        rationale: Option<String>,
    ) -> Result<(), AircError> {
        self.publish_lane_event(LaneCoordinationEvent {
            action: LaneAction::BlockOn,
            lane_id: lane_id.into(),
            owner: self.peer_id(),
            rationale,
            pr_number: None,
            blocked_on_lane_id: Some(blocked_on_lane_id.into()),
        })
        .await
    }

    /// Walk recent transcript history for coordination events on
    /// `lane_id` and return the latest plus the full ordered history.
    ///
    /// `window` is the number of recent events to scan; pass a
    /// generous value (e.g. 512) for lanes that may have been
    /// claimed early in a session. Callers wanting the absolute full
    /// history should page through the store directly — this is the
    /// convenience query for "what's the current state of this
    /// lane?"
    pub async fn lane_status(&self, lane_id: &str, window: usize) -> Result<LaneStatus, AircError> {
        let recent = self.page_recent(window).await?;
        let mut history: Vec<(u64, LaneCoordinationEvent)> = Vec::new();
        for event in &recent {
            let Some(parsed) = parse_lane_event(event) else {
                continue;
            };
            if parsed.lane_id != lane_id {
                continue;
            }
            history.push((event.lamport, parsed));
        }
        history.sort_by_key(|(lamport, _)| *lamport);
        let latest = history.last().map(|(_, event)| event.clone());
        let history = history.into_iter().map(|(_, event)| event).collect();
        Ok(LaneStatus { latest, history })
    }

    /// Subscribe to coordination events. Wraps the substrate stream
    /// with a filter that only yields `LaneCoordinationEvent`s, so
    /// schedulers and dashboards don't need to know the header /
    /// body format.
    pub async fn subscribe_lane_coordination(
        &self,
    ) -> Result<
        impl futures::stream::Stream<Item = (Arc<TranscriptEvent>, LaneCoordinationEvent)>,
        AircError,
    > {
        let inner = self.subscribe().await?;
        Ok(inner.filter_map(|item| async move {
            let event = item.ok()?;
            let parsed = parse_lane_event(&event)?;
            Some((event, parsed))
        }))
    }

    async fn publish_lane_event(&self, event: LaneCoordinationEvent) -> Result<(), AircError> {
        let kind = event.action.header_value();
        let lane_id = event.lane_id.clone();
        let pr_number = event.pr_number;
        let body = serde_json::to_value(&event)
            .map_err(|error| AircError::Crypto(format!("lane coordination encode: {error}")))?;

        let mut headers = Headers::new();
        headers.insert(HEADER_COORD_KIND.into(), kind.to_string());
        headers.insert(HEADER_COORD_LANE_ID.into(), lane_id);
        if let Some(pr) = pr_number {
            headers.insert(HEADER_COORD_PR.into(), pr.to_string());
        }

        self.send_frame_to(
            FrameKind::Event,
            MentionTarget::All,
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }
}

fn parse_lane_event(event: &TranscriptEvent) -> Option<LaneCoordinationEvent> {
    // The header filter is the cheap path. Bodies are only decoded
    // when the header indicates this is a coordination event.
    let _ = event.headers.get(HEADER_COORD_KIND)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_claim() -> LaneCoordinationEvent {
        LaneCoordinationEvent {
            action: LaneAction::Claim,
            lane_id: "audit:#964:flaw-3".to_string(),
            owner: PeerId::new(),
            rationale: Some("test claim".to_string()),
            pr_number: None,
            blocked_on_lane_id: None,
        }
    }

    fn sample_complete(pr: u64) -> LaneCoordinationEvent {
        LaneCoordinationEvent {
            action: LaneAction::Complete,
            lane_id: "audit:#964:flaw-3".to_string(),
            owner: PeerId::new(),
            rationale: None,
            pr_number: Some(pr),
            blocked_on_lane_id: None,
        }
    }

    #[test]
    fn claim_event_round_trips_through_json() {
        let event = sample_claim();
        let json = serde_json::to_string(&event).expect("encode");
        let decoded: LaneCoordinationEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, event);
    }

    #[test]
    fn release_event_round_trips_through_json() {
        let event = LaneCoordinationEvent {
            action: LaneAction::Release,
            lane_id: "lane-x".to_string(),
            owner: PeerId::new(),
            rationale: Some("handing back".to_string()),
            pr_number: None,
            blocked_on_lane_id: None,
        };
        let json = serde_json::to_string(&event).expect("encode");
        let decoded: LaneCoordinationEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, event);
    }

    #[test]
    fn complete_event_round_trips_through_json() {
        let event = sample_complete(957);
        let json = serde_json::to_string(&event).expect("encode");
        let decoded: LaneCoordinationEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, event);
        assert_eq!(decoded.pr_number, Some(957));
    }

    #[test]
    fn block_on_event_round_trips_with_dependency() {
        let event = LaneCoordinationEvent {
            action: LaneAction::BlockOn,
            lane_id: "lane-y".to_string(),
            owner: PeerId::new(),
            rationale: Some("needs upstream change".to_string()),
            pr_number: None,
            blocked_on_lane_id: Some("lane-z".to_string()),
        };
        let json = serde_json::to_string(&event).expect("encode");
        let decoded: LaneCoordinationEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, event);
        assert_eq!(decoded.blocked_on_lane_id.as_deref(), Some("lane-z"));
    }

    #[test]
    fn header_values_are_stable() {
        assert_eq!(LaneAction::Claim.header_value(), "claim");
        assert_eq!(LaneAction::Release.header_value(), "release");
        assert_eq!(LaneAction::Complete.header_value(), "complete");
        assert_eq!(LaneAction::BlockOn.header_value(), "block_on");
    }

    #[test]
    fn lane_status_is_claimed_after_claim_only() {
        let status = LaneStatus {
            latest: Some(sample_claim()),
            history: vec![sample_claim()],
        };
        assert!(status.is_claimed());
        assert!(!status.is_complete());
    }

    #[test]
    fn lane_status_is_complete_after_complete_supersedes_claim() {
        let claim = sample_claim();
        let complete = sample_complete(957);
        let status = LaneStatus {
            latest: Some(complete.clone()),
            history: vec![claim, complete],
        };
        assert!(!status.is_claimed());
        assert!(status.is_complete());
    }

    #[test]
    fn lane_status_empty_history_reads_unclaimed() {
        let status = LaneStatus {
            latest: None,
            history: Vec::new(),
        };
        assert!(!status.is_claimed());
        assert!(!status.is_complete());
        assert!(status.current_owner().is_none());
    }
}
