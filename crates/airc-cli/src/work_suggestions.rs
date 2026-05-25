use airc_core::TranscriptEvent;
use airc_lib::{
    Airc, RepoId, WorkQueueStatus, WorkQueueStatusQuery, HEADER_FORGE_WORK_EVENT_KIND,
    HEADER_FORGE_WORK_REPO,
};

const DEFAULT_LIMIT: usize = 5;

pub(crate) fn is_work_queue_event(event: &TranscriptEvent) -> bool {
    matches!(
        event
            .headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some(
            "card_created"
                | "card_claimed"
                | "claim_released"
                | "card_state_changed"
                | "agent_availability_reported",
        )
    )
}

pub(crate) async fn render_claimable_work(
    airc: &Airc,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let status = airc
        .work_queue_status(WorkQueueStatusQuery {
            limit: DEFAULT_LIMIT,
            ..WorkQueueStatusQuery::default()
        })
        .await?;
    Ok(render_status(&status))
}

pub(crate) async fn render_claimable_work_for_event(
    airc: &Airc,
    event: &TranscriptEvent,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !is_work_queue_event(event) {
        return Ok(None);
    }
    let repo = event
        .headers
        .get(HEADER_FORGE_WORK_REPO)
        .and_then(|value| RepoId::try_from(value.as_str()).ok());
    let status = airc
        .work_queue_status(WorkQueueStatusQuery {
            repo,
            limit: DEFAULT_LIMIT,
            ..WorkQueueStatusQuery::default()
        })
        .await?;
    Ok(render_status(&status))
}

fn render_status(status: &WorkQueueStatus) -> Option<String> {
    if status.claimable.is_empty() && status.agent_availability.is_empty() {
        return None;
    }
    let now_ms = now_ms();
    let mut lines = vec![format!(
        "AIRC work: {} claimable P0/P1; availability ready={} busy={} away={} stale={}",
        status.claimable.len(),
        status.ready_count(now_ms),
        status.busy_count(now_ms),
        status.away_count(now_ms),
        status.stale_availability_count(now_ms)
    )];
    for item in &status.claimable {
        let card = &item.card;
        let prefix = if item.is_stale_claim() {
            "recover"
        } else {
            "claim"
        };
        lines.push(format!(
            "- {priority:?} {card_id} {repo}: {title} ({prefix}: airc work claim {card_id})",
            priority = card.priority,
            card_id = card.card_id,
            repo = card.repo,
            title = card.title
        ));
    }
    if status.claimable.is_empty() {
        lines.push(
            "no claimable cards; publish ready/busy/away with airc work availability".to_string(),
        );
    } else if status.active_claims_for_peer.is_empty() {
        lines.push(
            "if idle: claim one, or publish busy/away via airc work availability".to_string(),
        );
    }
    Some(lines.join("\n"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
    };
    use airc_lib::{
        AgentAvailabilityRecord, AgentAvailabilityReported, AgentAvailabilityState, CardState,
        ClaimableWorkItem, Priority, WorkCard, WorkCardId,
    };

    use super::*;

    #[test]
    fn detects_queue_events_only() {
        let mut event = transcript();
        event.headers.insert(
            HEADER_FORGE_WORK_EVENT_KIND.to_string(),
            "card_created".to_string(),
        );
        assert!(is_work_queue_event(&event));

        event.headers.insert(
            HEADER_FORGE_WORK_EVENT_KIND.to_string(),
            "claim_heartbeat".to_string(),
        );
        assert!(!is_work_queue_event(&event));

        event.headers.insert(
            HEADER_FORGE_WORK_EVENT_KIND.to_string(),
            "agent_availability_reported".to_string(),
        );
        assert!(is_work_queue_event(&event));
    }

    #[test]
    fn renders_claimable_items_with_claim_command_hint() {
        let card_id = WorkCardId::new();
        let repo = RepoId::try_from("CambrianTech/airc").unwrap();
        let text = render_status(&WorkQueueStatus {
            claimable: vec![ClaimableWorkItem {
                card: work_card(card_id, repo),
                stale_claim: None,
            }],
            agent_availability: Vec::new(),
            active_claims_for_peer: Vec::new(),
        })
        .unwrap();

        assert!(text.contains("AIRC work: 1 claimable P0/P1"));
        assert!(text.contains("airc work claim"));
        assert!(text.contains("wire work suggestions into feed"));
        assert!(text.contains("publish busy/away"));
    }

    #[test]
    fn renders_availability_without_claimable_work() {
        let repo = RepoId::try_from("CambrianTech/airc").unwrap();
        let text = render_status(&WorkQueueStatus {
            claimable: Vec::new(),
            agent_availability: vec![AgentAvailabilityRecord {
                report: AgentAvailabilityReported {
                    repo,
                    peer: PeerId::new(),
                    state: AgentAvailabilityState::Ready,
                    note: Some("available for review".to_string()),
                    ttl_ms: 60_000,
                    reported_at_ms: now_ms(),
                },
                expires_at_ms: now_ms() + 60_000,
            }],
            active_claims_for_peer: Vec::new(),
        })
        .unwrap();

        assert!(text.contains("availability ready=1 busy=0 away=0 stale=0"));
        assert!(text.contains("publish ready/busy/away"));
    }

    fn work_card(card_id: WorkCardId, repo: RepoId) -> WorkCard {
        WorkCard {
            card_id,
            repo,
            title: "wire work suggestions into feed".to_string(),
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
        }
    }

    fn transcript() -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::System,
            occurred_at_ms: 1,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::Json(serde_json::json!({}))),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }
}
