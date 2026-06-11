use airc_core::{Body, PeerId};
use airc_protocol::{FrameKind, HEADER_FORGE_BODY_HINT};

use super::*;
use crate::goal::{ExitCondition, GoalId};
use crate::goal_event::{
    CardOrigin, GoalAbandoned, GoalAchieved, GoalCreated, GoalDryTickRecorded,
};
use crate::recipe::RecipeRef;
use crate::{
    AgentAvailabilityReported, AgentAvailabilityState, BranchName, CardCreated, ClaimId,
    DrainCandidate, DrainCandidateCategory, DrainOutcome, GitBranchMoved, GitObjectId, LaneId,
    PrCheckState, PressureLevel, Priority, PullRequestCheckSuiteChanged, PullRequestRef, RepoId,
    WorkCardId, WorkEvent, WorkspaceDrainCompleted, WorkspaceDrainRequested, WorkspaceId,
    WorkspacePressureReported, WorkspaceRequested,
};

fn card_created() -> WorkEvent {
    WorkEvent::CardCreated(CardCreated {
        card_id: WorkCardId::from_u128(1),
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "make work events routable".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: Some(LaneId::from_u128(2)),
        created_by: PeerId::from_u128(3),
        created_at_ms: 4,
        reviews: None,
        origin: None,
    })
}

#[test]
fn work_event_roundtrips_through_headers_and_body() {
    let event = card_created();

    let (headers, body) = encode_work_event(&event).unwrap();
    let decoded = decode_work_event(&headers, Some(&body)).unwrap();

    assert_eq!(decoded, event);
    assert_eq!(
        headers.get(HEADER_FORGE_BODY_HINT).map(String::as_str),
        Some(BODY_HINT_FORGE_WORK_EVENT)
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("card_created")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CARD_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-000000000001")
    );
}

#[test]
fn subscription_matches_work_events_without_parsing_body() {
    let event = card_created();
    let (headers, _) = encode_work_event(&event).unwrap();
    let sub = work_event_subscription();

    assert!(sub.headers_filter.matches(&headers));
    assert!(sub.kinds.contains(&FrameKind::Event));
}

#[test]
fn decode_rejects_wrong_hint_and_non_json_body() {
    let event = card_created();
    let (mut headers, _) = encode_work_event(&event).unwrap();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        "forge.chat.text".to_string(),
    );

    assert!(matches!(
        decode_work_event(&headers, Some(&Body::text("not work"))),
        Err(WorkEventCodecError::BodyHintMismatch { .. })
    ));

    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_WORK_EVENT.to_string(),
    );
    assert!(matches!(
        decode_work_event(&headers, Some(&Body::Binary(vec![1, 2, 3]))),
        Err(WorkEventCodecError::NonJsonBody)
    ));
}

#[test]
fn drain_events_roundtrip_and_carry_workspace_repo_and_policy_rule_headers() {
    let workspace_id = WorkspaceId::from_u128(0xa1);
    let repo = RepoId::new("CambrianTech/airc").unwrap();
    let reporter = PeerId::from_u128(0xb2);
    let policy_rule_id = "default.rebuildable".to_string();

    let pressure = WorkEvent::WorkspacePressureReported(WorkspacePressureReported {
        workspace_id,
        repo: repo.clone(),
        reporter,
        total_bytes: 1_000_000,
        available_bytes: 50_000,
        level: PressureLevel::High,
        reported_at_ms: 1,
    });
    let (headers, body) = encode_work_event(&pressure).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), pressure);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("workspace_pressure_reported")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_WORKSPACE_ID)
            .map(String::as_str),
        Some("00000000-0000-0000-0000-0000000000a1")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );

    let request = WorkEvent::WorkspaceDrainRequested(WorkspaceDrainRequested {
        workspace_id,
        repo: repo.clone(),
        requester: reporter,
        policy_rule_id: policy_rule_id.clone(),
        dry_run: true,
        candidates: vec![DrainCandidate {
            path: "/tmp/work/target".to_string(),
            category: DrainCandidateCategory::RebuildableCache,
            est_bytes: 500_000,
        }],
        requested_at_ms: 2,
    });
    let (headers, body) = encode_work_event(&request).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), request);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("workspace_drain_requested")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_POLICY_RULE_ID)
            .map(String::as_str),
        Some("default.rebuildable")
    );

    let completed = WorkEvent::WorkspaceDrainCompleted(WorkspaceDrainCompleted {
        workspace_id,
        repo,
        performer: reporter,
        policy_rule_id: policy_rule_id.clone(),
        dry_run: false,
        outcome: DrainOutcome {
            bytes_reclaimed: 450_000,
            paths_touched: vec!["/tmp/work/target".to_string()],
            paths_skipped: vec![],
            errors: vec![],
        },
        completed_at_ms: 3,
    });
    let (headers, body) = encode_work_event(&completed).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), completed);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("workspace_drain_completed")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_POLICY_RULE_ID)
            .map(String::as_str),
        Some("default.rebuildable")
    );
}

#[test]
fn workspace_headers_include_workspace_claim_card_and_repo() {
    let event = WorkEvent::WorkspaceRequested(WorkspaceRequested {
        workspace_id: WorkspaceId::from_u128(10),
        card_id: WorkCardId::from_u128(11),
        claim_id: ClaimId::from_u128(12),
        owner: PeerId::from_u128(13),
        repo: RepoId::new("CambrianTech/continuum").unwrap(),
        branch: BranchName::new("feat/rust-work").unwrap(),
        base: BranchName::new("rust-rewrite").unwrap(),
        requested_at_ms: 14,
    });

    let headers = work_event_headers(&event);

    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_WORKSPACE_ID)
            .map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000a")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CARD_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000b")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CLAIM_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000c")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/continuum")
    );
}

#[test]
fn git_and_pr_events_roundtrip_with_routeable_headers() {
    let repo = RepoId::new("CambrianTech/airc").unwrap();
    let branch = BranchName::new("rust-rewrite").unwrap();
    let commit = GitObjectId::new("abc123").unwrap();

    let git_event = WorkEvent::GitBranchMoved(GitBranchMoved {
        repo: repo.clone(),
        branch: branch.clone(),
        old_head: None,
        new_head: commit.clone(),
        moved_by: PeerId::from_u128(1),
        moved_at_ms: 2,
    });
    let (headers, body) = encode_work_event(&git_event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), git_event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("git_branch_moved")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_GIT_BRANCH)
            .map(String::as_str),
        Some("rust-rewrite")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_GIT_COMMIT)
            .map(String::as_str),
        Some("abc123")
    );

    let pr_event = WorkEvent::PullRequestCheckSuiteChanged(PullRequestCheckSuiteChanged {
        pull_request: PullRequestRef {
            repo,
            number: 914,
            head: BranchName::new("feat/lifecycle-events").unwrap(),
            base: branch,
        },
        state: PrCheckState::Passed,
        changed_by: PeerId::from_u128(3),
        changed_at_ms: 4,
    });
    let (headers, body) = encode_work_event(&pr_event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), pr_event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("pull_request_check_suite_changed")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_PR_NUMBER).map(String::as_str),
        Some("914")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_GIT_BRANCH)
            .map(String::as_str),
        Some("feat/lifecycle-events")
    );
}

#[test]
fn availability_event_roundtrips_with_repo_and_state_headers() {
    let event = WorkEvent::AgentAvailabilityReported(AgentAvailabilityReported {
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        peer: PeerId::from_u128(42),
        state: AgentAvailabilityState::Ready,
        note: Some("can take review".to_string()),
        ttl_ms: 60_000,
        reported_at_ms: 7,
    });

    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("agent_availability_reported")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_STATE).map(String::as_str),
        Some("ready")
    );
}

// Slice C2a coverage — wire-shape + header-projection tests for the
// four new Goal-lifecycle WorkEvent variants. Projection logic (apply
// arms in WorkBoardProjection::apply) lands in C2b; here we pin the
// codec round-trip + the routing headers the wire-side relies on.

fn goal_id(n: u128) -> GoalId {
    GoalId::from_u128(n)
}

fn peer(n: u128) -> PeerId {
    PeerId::from_u128(n)
}

#[test]
fn card_created_origin_field_round_trips_when_synthesized() {
    // what this catches: regression where `CardCreated.origin` drops
    // from the serde wire shape or its `Synthesized` variant fails to
    // round-trip. C2a's central wire claim is that provenance rides
    // the primary object — losing this round-trip means downstream
    // replayers can't attribute synthesizer-minted cards.
    let event = WorkEvent::CardCreated(CardCreated {
        card_id: WorkCardId::from_u128(10),
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "synthesized follow-up".to_string(),
        body: None,
        priority: Priority::P2,
        lane_id: None,
        created_by: peer(11),
        created_at_ms: 12,
        reviews: None,
        origin: Some(CardOrigin::Synthesized {
            goal_id: goal_id(13),
            recipe_id: RecipeRef::new("follow-up-extraction"),
            synthesizer_peer: peer(14),
            dedup_key: "goal-13::follow-up-#15".to_string(),
        }),
    });

    let (headers, body) = encode_work_event(&event).unwrap();
    let decoded = decode_work_event(&headers, Some(&body)).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn card_created_legacy_payload_without_origin_decodes_with_none() {
    // what this catches: regression where removing `#[serde(default)]`
    // from `CardCreated.origin` breaks legacy-event decode. Every card
    // filed before C2a lands has no `origin` field on the wire; the
    // projection (C2b) interprets `None` as Operator { peer_id:
    // created_by }, so silently failing to decode would silently break
    // every pre-C2a transcript. Verdict-anchored back-compat.
    let json = serde_json::to_value(CardCreated {
        card_id: WorkCardId::from_u128(20),
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "legacy operator-filed card".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        created_by: peer(21),
        created_at_ms: 22,
        reviews: None,
        origin: None,
    })
    .unwrap();

    // Sanity: `origin: None` is skipped from the wire under
    // `skip_serializing_if = "Option::is_none"`, so the encoded shape
    // is exactly a legacy payload. Decoding it back must succeed and
    // produce `origin: None`.
    assert!(
        json.get("origin").is_none(),
        "origin must be omitted from wire when None: {json}"
    );
    let decoded: CardCreated = serde_json::from_value(json).unwrap();
    assert!(decoded.origin.is_none());
}

#[test]
fn goal_created_event_projects_goal_id_and_repo_headers() {
    // what this catches: regression where `GoalCreated` either fails to
    // round-trip or loses the goal_id / default_repo header projection
    // the routing layer keys on. Subscribers filtering by goal_id
    // depend on this header landing structurally.
    let event = WorkEvent::GoalCreated(GoalCreated {
        goal_id: goal_id(30),
        title: "ship cross-grid inference".into(),
        default_repo: RepoId::new("CambrianTech/airc").unwrap(),
        exit_condition: ExitCondition::OperatorOnly,
        recipe_refs: vec![],
        created_by: peer(31),
        created_at_ms: 32,
    });

    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("goal_created")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_GOAL_ID).map(String::as_str),
        Some(goal_id(30).to_string().as_str())
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );
}

#[test]
fn goal_achieved_event_projects_goal_id_header() {
    // what this catches: regression where the operator-path goal
    // achievement event loses its goal_id routing header. The auto
    // path is silent (pure derived state per v2 residual 4); the
    // operator path is the ONLY wire emission for goal achievement.
    let event = WorkEvent::GoalAchieved(GoalAchieved {
        goal_id: goal_id(40),
        condition: ExitCondition::OperatorOnly,
        achieved_by: peer(41),
        achieved_at_ms: 42,
    });

    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("goal_achieved")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_GOAL_ID).map(String::as_str),
        Some(goal_id(40).to_string().as_str())
    );
}

#[test]
fn goal_abandoned_event_projects_goal_id_header() {
    // what this catches: regression where the operator-path goal
    // abandonment event loses its goal_id routing header. Distinct
    // from GoalAchieved in the audit trail; both kill the synthesizer.
    let event = WorkEvent::GoalAbandoned(GoalAbandoned {
        goal_id: goal_id(50),
        abandoned_by: peer(51),
        reason: "scope cut".into(),
        abandoned_at_ms: 52,
    });

    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("goal_abandoned")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_GOAL_ID).map(String::as_str),
        Some(goal_id(50).to_string().as_str())
    );
}

#[test]
fn goal_dry_tick_recorded_event_projects_goal_id_header() {
    // what this catches: regression where the dry-tick event loses its
    // goal_id header. The projection (C2b) counts consecutive instances
    // per goal_id to fire `ExitCondition::DryForTicks { n }`; a missing
    // routing header would break per-goal-scoped filtering on subscribe.
    let event = WorkEvent::GoalDryTickRecorded(GoalDryTickRecorded {
        goal_id: goal_id(60),
        synthesizer_peer: peer(61),
        recorded_at_ms: 62,
    });

    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("goal_dry_tick_recorded")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_GOAL_ID).map(String::as_str),
        Some(goal_id(60).to_string().as_str())
    );
}

// Verdict 4679774882 blocker 2 — envelope-tag pins for the 4 goal
// WorkEvent variants. The codec tests above pin only the HEADER kind
// (which comes from `event_kind()`'s independent string literal);
// `decode_work_event` never reads the kind header, so the body's
// serde tag IS the decode contract — and it was unpinned. Mutation
// proof: `#[serde(rename = "goal_minted")]` on `GoalCreated` passes
// every existing test. These tests assert the literal `kind` value
// in the encoded JSON body so a variant rename either updates the
// assertion deliberately or fails loudly. Same pattern as the
// `card_origin_operator_variant_has_stable_wire_tag` test in
// goal_event.rs.

/// Helper: extract the body JSON from an encoded WorkEvent. The codec
/// emits `Body::Json(serde_json::Value)`; this helper unwraps that to
/// the `Value` so per-tag assertions stay terse.
fn body_value(body: &Body) -> &serde_json::Value {
    match body {
        Body::Json(v) => v,
        other => panic!("expected Body::Json, got {other:?}"),
    }
}

#[test]
fn goal_created_body_serde_tag_is_pinned_literal() {
    // what this catches: mutation of the `GoalCreated` variant name
    // (e.g. `#[serde(rename = "goal_minted")]`) that drifts the body
    // wire shape away from `"goal_created"`. Verdict 4679774882
    // blocker 2: round-trip tests are tag-blind (encode + decode
    // shift together); header-kind assertions ride a separate code
    // path (`event_kind()`); ONLY a literal-JSON assertion on the
    // body's `kind` tag catches the regression. Header + body MUST
    // agree.
    let event = WorkEvent::GoalCreated(GoalCreated {
        goal_id: goal_id(70),
        title: "x".into(),
        default_repo: RepoId::new("CambrianTech/airc").unwrap(),
        exit_condition: ExitCondition::OperatorOnly,
        recipe_refs: vec![],
        created_by: peer(71),
        created_at_ms: 72,
    });
    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(
        body_value(&body).get("kind").and_then(|v| v.as_str()),
        Some("goal_created"),
        "WorkEvent::GoalCreated body MUST encode `\"kind\":\"goal_created\"` — \
         any rename is a deliberate wire break and must update this assertion"
    );
    // Header/body consistency: the routing header's kind value MUST
    // match the body's serde tag. Divergence between them is the
    // exact tag-blindness failure mode this slice's review uncovered.
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        body_value(&body).get("kind").and_then(|v| v.as_str()),
        "envelope kind header must equal body serde tag"
    );
}

#[test]
fn goal_achieved_body_serde_tag_is_pinned_literal() {
    // what this catches: mutation of `GoalAchieved` variant name.
    // See goal_created counterpart for the full rationale.
    let event = WorkEvent::GoalAchieved(GoalAchieved {
        goal_id: goal_id(80),
        condition: ExitCondition::OperatorOnly,
        achieved_by: peer(81),
        achieved_at_ms: 82,
    });
    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(
        body_value(&body).get("kind").and_then(|v| v.as_str()),
        Some("goal_achieved")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        body_value(&body).get("kind").and_then(|v| v.as_str()),
    );
}

#[test]
fn goal_abandoned_body_serde_tag_is_pinned_literal() {
    // what this catches: mutation of `GoalAbandoned` variant name.
    let event = WorkEvent::GoalAbandoned(GoalAbandoned {
        goal_id: goal_id(90),
        abandoned_by: peer(91),
        reason: "scope cut".into(),
        abandoned_at_ms: 92,
    });
    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(
        body_value(&body).get("kind").and_then(|v| v.as_str()),
        Some("goal_abandoned")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        body_value(&body).get("kind").and_then(|v| v.as_str()),
    );
}

#[test]
fn goal_dry_tick_recorded_body_serde_tag_is_pinned_literal() {
    // what this catches: mutation of `GoalDryTickRecorded` variant
    // name. The projection (C2b) counts consecutive instances by
    // event kind; a body-tag drift breaks `ExitCondition::DryForTicks`
    // structurally without any header-side signal.
    let event = WorkEvent::GoalDryTickRecorded(GoalDryTickRecorded {
        goal_id: goal_id(100),
        synthesizer_peer: peer(101),
        recorded_at_ms: 102,
    });
    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(
        body_value(&body).get("kind").and_then(|v| v.as_str()),
        Some("goal_dry_tick_recorded")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        body_value(&body).get("kind").and_then(|v| v.as_str()),
    );
}

#[test]
fn card_created_origin_field_name_is_pinned_literal() {
    // what this catches: mutation of `CardCreated.origin` field name
    // (e.g. `#[serde(rename = "provenance")]`). Verdict 4679774882
    // blocker 3: the round-trip test is field-name-blind (encode +
    // decode shift together) and the legacy test only pins absence.
    // Pinning the literal `"origin"` key on a Synthesized payload
    // kills field-rename mutations structurally.
    let event = WorkEvent::CardCreated(CardCreated {
        card_id: WorkCardId::from_u128(110),
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "synthesized follow-up".to_string(),
        body: None,
        priority: Priority::P2,
        lane_id: None,
        created_by: peer(111),
        created_at_ms: 112,
        reviews: None,
        origin: Some(CardOrigin::Synthesized {
            goal_id: goal_id(113),
            recipe_id: RecipeRef::new("follow-up-extraction"),
            synthesizer_peer: peer(114),
            dedup_key: "goal-113::dedup-test".to_string(),
        }),
    });
    let (_headers, body) = encode_work_event(&event).unwrap();
    let value = body_value(&body);
    let origin = value.get("origin").expect(
        "CardCreated body MUST encode the field name `\"origin\"` — \
                 any rename is a deliberate wire break",
    );
    // Pin the inner tagged shape too: `{"kind":"synthesized", ...}`.
    // This belt-and-suspenders catches the case where someone
    // renames both `origin` AND the variant tag in lockstep, which
    // a single-key check would miss.
    assert_eq!(
        origin.get("kind").and_then(|v| v.as_str()),
        Some("synthesized"),
        "CardOrigin::Synthesized body tag MUST be `\"synthesized\"`"
    );
}
