use airc_core::TranscriptEvent;
use airc_lib::{
    Airc, ClaimableWorkItem, ClaimableWorkQuery, RepoId, HEADER_FORGE_WORK_EVENT_KIND,
    HEADER_FORGE_WORK_REPO,
};

const DEFAULT_LIMIT: usize = 5;

pub(crate) fn is_work_queue_event(event: &TranscriptEvent) -> bool {
    matches!(
        event
            .headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("card_created" | "claim_released" | "card_state_changed")
    )
}

pub(crate) async fn render_claimable_work(
    airc: &Airc,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let items = airc
        .claimable_work(ClaimableWorkQuery {
            limit: DEFAULT_LIMIT,
            ..ClaimableWorkQuery::default()
        })
        .await?;
    Ok(render_items(&items))
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
    let items = airc
        .claimable_work(ClaimableWorkQuery {
            repo,
            limit: DEFAULT_LIMIT,
            ..ClaimableWorkQuery::default()
        })
        .await?;
    Ok(render_items(&items))
}

fn render_items(items: &[ClaimableWorkItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut lines = vec![format!("AIRC work: {} claimable P0/P1", items.len())];
    for item in items {
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
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
    };
    use airc_lib::{CardState, Priority, WorkCard, WorkCardId};

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
    }

    #[test]
    fn renders_claimable_items_with_claim_command_hint() {
        let card_id = WorkCardId::new();
        let repo = RepoId::try_from("CambrianTech/airc").unwrap();
        let text = render_items(&[ClaimableWorkItem {
            card: WorkCard {
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
            },
            stale_claim: None,
        }])
        .unwrap();

        assert!(text.contains("AIRC work: 1 claimable P0/P1"));
        assert!(text.contains("airc work claim"));
        assert!(text.contains("wire work suggestions into feed"));
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
