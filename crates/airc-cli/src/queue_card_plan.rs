use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::{Map, Value};

use crate::queue_card_commands::{
    json_obj, nonempty_or, parse_card, read_json, string_field, value_text,
};

const ACTIVE_STATUSES: &[&str] = &["claimed", "in-progress", "review", "blocked"];
const LANES: &[(&str, &[&str])] = &[
    (
        "alpha-gap/rust-runtime",
        &[
            "rust",
            "persona",
            "cognition",
            "ts-rs",
            "runtime",
            "node",
            "engram",
            "prompt",
            "evaluator",
            "provider",
            "model",
            "ai throughput",
        ],
    ),
    (
        "perf/resource-control",
        &[
            "perf",
            "speed",
            "cpu",
            "memory",
            "gpu",
            "docker",
            "resource",
            "throughput",
            "latency",
            "capacity",
            "qwen",
            "cuda",
            "metal",
            "vulkan",
            "livekit",
            "webrtc",
            "framebuffer",
            "texture",
        ],
    ),
    (
        "flywheel/automation",
        &[
            "airc",
            "queue",
            "kanban",
            "automation",
            "metronome",
            "nudge",
            "close-merged",
            "issue",
            "pr",
            "canary",
            "workflow",
            "precommit",
            "collaboration",
            "owner",
            "claim",
        ],
    ),
    (
        "quality/tests-vdd",
        &[
            "test",
            "vdd",
            "tdd",
            "lint",
            "clippy",
            "baseline",
            "validation",
            "smoke",
            "roundtrip",
            "hygiene",
            "ratchet",
            "determinism",
        ],
    ),
    (
        "ui/configurator",
        &[
            "ui",
            "browser",
            "widget",
            "configurator",
            "render",
            "web",
            "tsx",
            "react",
            "adapter",
        ],
    ),
    (
        "integration/canary",
        &[
            "canary", "merge", "release", "install", "image", "main", "pre-push", "prepush", "ci",
        ],
    ),
];
const P0_WORDS: &[&str] = &[
    "broken",
    "failing",
    "fail",
    "regression",
    "panic",
    "crash",
    "blocked",
    "idle",
    "flywheel",
    "canary",
    "main",
    "merge",
    "stale",
    "wrong",
];
const P1_WORDS: &[&str] = &[
    "rust", "perf", "speed", "memory", "cpu", "gpu", "resource", "ts-rs", "test", "vdd", "tdd",
    "cleanup", "refactor", "docker", "qwen",
];

pub fn run_plan(
    repo: &str,
    owner: &str,
    stale_after: &str,
    output_json: bool,
    raw_json_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let board = QueueBoard::load(repo, owner, stale_after, raw_json_file, "queue plan")?;
    if output_json {
        println!("{}", serde_json::to_string_pretty(&board.plan_json())?);
    } else {
        render_plan(&board);
    }
    Ok(())
}

pub fn run_steward(
    repo: &str,
    owner: &str,
    stale_after: &str,
    output_json: bool,
    raw_json_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let board = QueueBoard::load(repo, owner, stale_after, raw_json_file, "queue steward")?;
    let actions = steward_actions(&board);
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&board.steward_json(&actions))?
        );
    } else {
        render_steward(&board, &actions);
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct QueueBoard {
    repo: String,
    owner: String,
    now: DateTime<Utc>,
    stale_after: String,
    cards: Vec<PlanCard>,
}

impl QueueBoard {
    fn load(
        repo: &str,
        owner: &str,
        stale_after: &str,
        raw_json_file: &Path,
        context: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let issues = read_json(raw_json_file)?;
        let issues = issues
            .as_array()
            .ok_or_else(|| format!("{context}: issue JSON must be an array"))?;
        let now = Utc::now();
        let threshold = parse_duration(stale_after, context)?;
        let mut cards = Vec::new();
        for issue in issues {
            let Some(card) = issue
                .get("body")
                .and_then(Value::as_str)
                .and_then(parse_card)
            else {
                continue;
            };
            cards.push(PlanCard::from_issue(
                repo, owner, issue, card, now, threshold,
            ));
        }
        cards.sort_by_key(|card| {
            (
                priority_rank(&card.priority),
                status_rank(&card.status),
                if card.owner.is_empty() { 0 } else { 1 },
                card.number.as_i64().unwrap_or(0),
            )
        });
        Ok(Self {
            repo: repo.to_string(),
            owner: owner.to_string(),
            now,
            stale_after: stale_after.to_string(),
            cards,
        })
    }

    fn now_text(&self) -> String {
        self.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn summary(&self) -> BoardSummary {
        BoardSummary::from_cards(&self.cards)
    }

    fn lanes(&self) -> BTreeMap<String, Vec<&PlanCard>> {
        let mut lanes: BTreeMap<String, Vec<&PlanCard>> = BTreeMap::new();
        for card in &self.cards {
            lanes.entry(card.lane.clone()).or_default().push(card);
        }
        lanes
    }

    fn owners(&self) -> BTreeMap<String, Vec<&PlanCard>> {
        let mut owners: BTreeMap<String, Vec<&PlanCard>> = BTreeMap::new();
        for card in &self.cards {
            if !card.owner.is_empty() {
                owners.entry(card.owner.clone()).or_default().push(card);
            }
        }
        owners
    }

    fn plan_json(&self) -> Value {
        let summary = self.summary();
        json_obj([
            ("repo", Value::String(self.repo.clone())),
            ("owner", Value::String(self.owner.clone())),
            ("now_utc", Value::String(self.now_text())),
            ("stale_after", Value::String(self.stale_after.clone())),
            ("summary", summary.json()),
            (
                "cards",
                Value::Array(self.cards.iter().map(PlanCard::json).collect()),
            ),
            (
                "lanes",
                refs_object(self.lanes().into_iter().map(|(lane, cards)| {
                    (
                        lane,
                        cards.into_iter().map(|card| card.ref_id.clone()).collect(),
                    )
                })),
            ),
            (
                "owners",
                refs_object(self.owners().into_iter().map(|(owner, cards)| {
                    (
                        owner,
                        cards.into_iter().map(|card| card.ref_id.clone()).collect(),
                    )
                })),
            ),
        ])
    }

    fn steward_json(&self, actions: &[StewardAction]) -> Value {
        let summary = StewardSummary::from_board(self, actions);
        json_obj([
            ("repo", Value::String(self.repo.clone())),
            ("owner", Value::String(self.owner.clone())),
            ("now_utc", Value::String(self.now_text())),
            ("stale_after", Value::String(self.stale_after.clone())),
            ("summary", summary.json()),
            (
                "actions",
                Value::Array(actions.iter().map(StewardAction::json).collect()),
            ),
        ])
    }
}

#[derive(Debug, Clone)]
struct PlanCard {
    number: Value,
    ref_id: String,
    title: String,
    url: String,
    status: String,
    owner: String,
    lane: String,
    priority: String,
    explicit_priority: String,
    branch: String,
    environment: String,
    blockers: String,
    evidence: String,
    next_action: String,
    last_heartbeat: String,
    stale_reason: String,
    updated_at: String,
    claim_command: String,
}

impl PlanCard {
    fn from_issue(
        repo: &str,
        owner: &str,
        issue: &Value,
        card: Map<String, Value>,
        now: DateTime<Utc>,
        threshold: Duration,
    ) -> Self {
        let card_value = Value::Object(card.clone());
        let status = nonempty_or(&string_field(&card_value, "status"), "claimed");
        let mut card_owner = string_field(&card_value, "owner");
        if card_owner == "unclaimed" {
            card_owner.clear();
        }
        let heartbeat = string_field(&card_value, "last_heartbeat");
        let heartbeat_at = parse_time(&heartbeat);
        let stale_reason = if matches!(status.as_str(), "claimed" | "in-progress" | "review") {
            if !card_owner.is_empty() && heartbeat_at.is_none() {
                "missing-heartbeat"
            } else if !card_owner.is_empty()
                && heartbeat_at.map(|at| now - at > threshold).unwrap_or(false)
            {
                "stale-heartbeat"
            } else if card_owner.is_empty() && matches!(status.as_str(), "in-progress" | "review") {
                "missing-owner"
            } else {
                ""
            }
        } else {
            ""
        }
        .to_string();
        let title = compact_title(&string_field(issue, "title"));
        let text = card_text(issue, &card_value, &title);
        let lane = lane_for(&text);
        let priority = priority_for(&status, &text, &card_value, !stale_reason.is_empty());
        let number = issue.get("number").cloned().unwrap_or(Value::Null);
        let ref_id = format!("{repo}#{}", value_text(&number));
        Self {
            number,
            ref_id: ref_id.clone(),
            title,
            url: string_field(issue, "url"),
            status,
            owner: card_owner,
            lane,
            explicit_priority: explicit_priority(&card_value),
            priority,
            branch: string_field(&card_value, "branch"),
            environment: string_field(&card_value, "env"),
            blockers: string_field(&card_value, "blockers"),
            evidence: string_field(&card_value, "evidence"),
            next_action: string_field(&card_value, "next_action"),
            last_heartbeat: heartbeat,
            stale_reason,
            updated_at: string_field(issue, "updatedAt"),
            claim_command: format!(
                "airc queue claim {} --owner {}",
                shquote(&ref_id),
                shquote(owner)
            ),
        }
    }

    fn json(&self) -> Value {
        json_obj([
            ("number", self.number.clone()),
            ("ref", Value::String(self.ref_id.clone())),
            ("title", Value::String(self.title.clone())),
            ("url", Value::String(self.url.clone())),
            ("status", Value::String(self.status.clone())),
            ("owner", Value::String(self.owner.clone())),
            ("lane", Value::String(self.lane.clone())),
            ("priority", Value::String(self.priority.clone())),
            ("branch", Value::String(self.branch.clone())),
            ("env", Value::String(self.environment.clone())),
            ("blockers", Value::String(self.blockers.clone())),
            ("evidence", Value::String(self.evidence.clone())),
            ("next_action", Value::String(self.next_action.clone())),
            ("last_heartbeat", Value::String(self.last_heartbeat.clone())),
            ("stale_reason", Value::String(self.stale_reason.clone())),
            ("updatedAt", Value::String(self.updated_at.clone())),
            ("claim_command", Value::String(self.claim_command.clone())),
        ])
    }
}

#[derive(Debug)]
struct BoardSummary {
    open: usize,
    stale: usize,
    owned: usize,
    unowned: usize,
    priorities: BTreeMap<String, usize>,
    statuses: BTreeMap<String, usize>,
}

impl BoardSummary {
    fn from_cards(cards: &[PlanCard]) -> Self {
        let mut priorities = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        for card in cards {
            *priorities.entry(card.priority.clone()).or_insert(0) += 1;
            *statuses.entry(card.status.clone()).or_insert(0) += 1;
        }
        Self {
            open: cards.len(),
            stale: cards
                .iter()
                .filter(|card| !card.stale_reason.is_empty())
                .count(),
            owned: cards.iter().filter(|card| !card.owner.is_empty()).count(),
            unowned: cards.iter().filter(|card| card.owner.is_empty()).count(),
            priorities,
            statuses,
        }
    }

    fn json(&self) -> Value {
        json_obj([
            ("open", Value::Number((self.open as u64).into())),
            ("priorities", count_object(&self.priorities)),
            ("statuses", count_object(&self.statuses)),
            ("stale", Value::Number((self.stale as u64).into())),
            ("owned", Value::Number((self.owned as u64).into())),
            ("unowned", Value::Number((self.unowned as u64).into())),
        ])
    }
}

#[derive(Debug)]
struct StewardSummary {
    open: usize,
    active: usize,
    actions: usize,
    stale: usize,
    claimable: usize,
    priorities: BTreeMap<String, usize>,
}

impl StewardSummary {
    fn from_board(board: &QueueBoard, actions: &[StewardAction]) -> Self {
        let active = active_cards(&board.cards);
        let mut priorities = BTreeMap::new();
        for card in &active {
            *priorities.entry(card.priority.clone()).or_insert(0) += 1;
        }
        Self {
            open: board.cards.len(),
            active: active.len(),
            actions: actions.len(),
            stale: active
                .iter()
                .filter(|card| !card.stale_reason.is_empty())
                .count(),
            claimable: active
                .iter()
                .filter(|card| {
                    matches!(card.status.as_str(), "claimed" | "blocked") && card.owner.is_empty()
                })
                .count(),
            priorities,
        }
    }

    fn json(&self) -> Value {
        json_obj([
            ("open", Value::Number((self.open as u64).into())),
            ("active", Value::Number((self.active as u64).into())),
            ("actions", Value::Number((self.actions as u64).into())),
            ("stale", Value::Number((self.stale as u64).into())),
            ("claimable", Value::Number((self.claimable as u64).into())),
            ("priorities", count_object(&self.priorities)),
        ])
    }
}

#[derive(Debug, Clone)]
struct StewardAction {
    kind: String,
    severity: String,
    ref_id: String,
    title: String,
    reason: String,
    command: String,
}

impl StewardAction {
    fn card(kind: &str, card: &PlanCard, reason: &str, command: String, severity: &str) -> Self {
        Self {
            kind: kind.to_string(),
            severity: severity.to_string(),
            ref_id: card.ref_id.clone(),
            title: card.title.clone(),
            reason: reason.to_string(),
            command,
        }
    }

    fn global(kind: &str, reason: String, command: String, severity: &str) -> Self {
        Self {
            kind: kind.to_string(),
            severity: severity.to_string(),
            ref_id: String::new(),
            title: String::new(),
            reason,
            command,
        }
    }

    fn json(&self) -> Value {
        json_obj([
            ("kind", Value::String(self.kind.clone())),
            ("severity", Value::String(self.severity.clone())),
            ("ref", Value::String(self.ref_id.clone())),
            ("title", Value::String(self.title.clone())),
            ("reason", Value::String(self.reason.clone())),
            ("command", Value::String(self.command.clone())),
        ])
    }
}

fn steward_actions(board: &QueueBoard) -> Vec<StewardAction> {
    let mut actions = Vec::new();
    for card in &board.cards {
        if card.status == "merged" {
            actions.push(StewardAction::card(
                "close-merged-card",
                card,
                "queue card is marked merged but the GitHub issue is still open",
                format!("gh issue close {}", card.ref_id),
                "info",
            ));
            continue;
        }
        if !card.stale_reason.is_empty() {
            actions.push(StewardAction::card(
                "nudge-stale-claim",
                card,
                &card.stale_reason,
                format!(
                    "airc queue nudge {} --reason stale-claim --message \"Please heartbeat, release, or update next_action.\"",
                    card.ref_id
                ),
                "warn",
            ));
        }
        if matches!(card.status.as_str(), "claimed" | "blocked") && card.owner.is_empty() {
            actions.push(StewardAction::card(
                "claim-ready-card",
                card,
                "claimable card has no active owner",
                format!(
                    "airc queue claim {} --owner {}",
                    shquote(&card.ref_id),
                    shquote(&board.owner)
                ),
                "info",
            ));
        }
        if card.next_action.is_empty() {
            actions.push(StewardAction::card(
                "fill-next-action",
                card,
                "missing next_action makes the card hard to dispatch",
                format!(
                    "airc queue heartbeat {} --note \"Set next_action before dispatch.\"",
                    card.ref_id
                ),
                "warn",
            ));
        }
        if card.priority == "P0"
            && card.explicit_priority.is_empty()
            && card.stale_reason.is_empty()
            && !matches!(card.status.as_str(), "blocked" | "review")
        {
            actions.push(StewardAction::card(
                "review-priority",
                card,
                "implicit P0 inferred from keywords; steward should confirm or demote",
                format!(
                    "airc queue heartbeat {} --note \"Review priority: confirm P0 or set explicit P1/P2.\"",
                    card.ref_id
                ),
                "info",
            ));
        }
    }

    let active = active_cards(&board.cards);
    let mut active_by_owner: BTreeMap<String, Vec<&PlanCard>> = BTreeMap::new();
    for card in &active {
        if !card.owner.is_empty() && ACTIVE_STATUSES.contains(&card.status.as_str()) {
            active_by_owner
                .entry(card.owner.clone())
                .or_default()
                .push(card);
        }
    }
    for (owner, cards) in active_by_owner {
        if cards.len() > 3 {
            let refs = cards
                .iter()
                .take(8)
                .map(|card| card.ref_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            actions.push(StewardAction::global(
                "owner-overloaded",
                format!("{owner} owns {} active cards: {refs}", cards.len()),
                format!(
                    "airc queue availability {} --stale-after {}",
                    board.repo, board.stale_after
                ),
                "warn",
            ));
        }
    }

    let mut priority_counts = BTreeMap::new();
    for card in &active {
        *priority_counts
            .entry(card.priority.clone())
            .or_insert(0usize) += 1;
    }
    let p0_count = priority_counts.get("P0").copied().unwrap_or(0);
    if active.len() >= 4 && p0_count * 4 >= active.len() * 3 {
        actions.push(StewardAction::global(
            "priority-collapse",
            format!(
                "{}/{} active cards are P0; priority has lost meaning",
                p0_count,
                active.len()
            ),
            format!("airc queue steward {} --json", board.repo),
            "warn",
        ));
    }
    actions
}

fn render_plan(board: &QueueBoard) {
    let summary = board.summary();
    let lanes = board.lanes();
    let owners = board.owners();
    println!("# airc queue plan — {}", board.repo);
    println!("now_utc: {}", board.now_text());
    println!("owner: {}", board.owner);
    println!(
        "summary: open={} P0={} P1={} review={} blocked={} stale={} unowned={}",
        summary.open,
        summary.priorities.get("P0").copied().unwrap_or(0),
        summary.priorities.get("P1").copied().unwrap_or(0),
        summary.statuses.get("review").copied().unwrap_or(0),
        summary.statuses.get("blocked").copied().unwrap_or(0),
        summary.stale,
        summary.unowned
    );
    if board.cards.is_empty() {
        println!("No open airc-queue cards.");
        println!(
            "next: airc queue add {} --title \"Describe the next concrete task\" --status claimed --owner unclaimed",
            board.repo
        );
        return;
    }

    println!("\n## P0 now");
    print_section_cards(
        &board
            .cards
            .iter()
            .filter(|card| card.priority == "P0")
            .collect::<Vec<_>>(),
    );
    println!("\n## Review / merge candidates");
    let review_cards = board
        .cards
        .iter()
        .filter(|card| card.status == "review")
        .collect::<Vec<_>>();
    print_section_cards(&review_cards);
    println!("\n## Active ownership");
    if owners.is_empty() {
        println!("- none");
    } else {
        for (owner, cards) in &owners {
            let refs = cards
                .iter()
                .take(8)
                .map(|card| format!("{}({}/{})", card.ref_id, card.status, card.priority))
                .collect::<Vec<_>>()
                .join(", ");
            println!("- {owner}: {refs}");
        }
    }
    println!("\n## Stale / needs nudge");
    print_section_cards(
        &board
            .cards
            .iter()
            .filter(|card| !card.stale_reason.is_empty())
            .collect::<Vec<_>>(),
    );
    println!("\n## Strategic lanes");
    for lane in LANES
        .iter()
        .map(|(lane, _)| *lane)
        .chain(std::iter::once("backlog/general"))
    {
        let Some(rows) = lanes.get(lane) else {
            continue;
        };
        if rows.is_empty() {
            continue;
        }
        let examples = rows
            .iter()
            .take(6)
            .map(|card| format!("{}({}/{})", card.ref_id, card.status, card.priority))
            .collect::<Vec<_>>()
            .join(", ");
        println!("- {lane}: {} open — {examples}", rows.len());
    }
    println!("\n## Next actions");
    let mut actions = Vec::new();
    if let Some(card) = review_cards.first() {
        actions.push(format!(
            "Review/merge {}: {}",
            card.ref_id,
            short(&card.title, 72)
        ));
    }
    if let Some(card) = board
        .cards
        .iter()
        .find(|card| !card.stale_reason.is_empty())
    {
        actions.push(format!(
            "Nudge or release stale claim {}: {}",
            card.ref_id, card.stale_reason
        ));
    }
    if let Some(card) = board
        .cards
        .iter()
        .find(|card| matches!(card.status.as_str(), "claimed" | "blocked") && card.owner.is_empty())
    {
        actions.push(format!("Claim top ready card: {}", card.claim_command));
    }
    if actions.is_empty() {
        actions.push(
            "No obvious queue move; create a missing P1/P2 card from the next code smell or test gap."
                .to_string(),
        );
    }
    for (index, action) in actions.iter().take(5).enumerate() {
        println!("{}. {action}", index + 1);
    }
}

fn render_steward(board: &QueueBoard, actions: &[StewardAction]) {
    let summary = StewardSummary::from_board(board, actions);
    println!("# airc queue steward — {}", board.repo);
    println!("now_utc: {}", board.now_text());
    println!("owner: {}", board.owner);
    println!(
        "summary: open={} active={} actions={} stale={} claimable={} P0={} P1={}",
        summary.open,
        summary.active,
        summary.actions,
        summary.stale,
        summary.claimable,
        summary.priorities.get("P0").copied().unwrap_or(0),
        summary.priorities.get("P1").copied().unwrap_or(0)
    );
    println!("\n## Proposed Steward Actions");
    if actions.is_empty() {
        println!("- none");
    } else {
        for action in actions.iter().take(20) {
            let target = if action.ref_id.is_empty() {
                String::new()
            } else {
                format!(" {}", action.ref_id)
            };
            println!("- [{}] {}{}", action.severity, action.kind, target);
            if !action.title.is_empty() {
                println!("  title: {}", short(&action.title, 120));
            }
            println!("  reason: {}", short(&action.reason, 120));
            println!("  command: {}", action.command);
        }
    }
    println!("\nmode: dry-run/read-only; commands are recommendations, not automatic mutations");
}

fn print_section_cards(cards: &[&PlanCard]) {
    if cards.is_empty() {
        println!("- none");
        return;
    }
    for card in cards.iter().take(8) {
        print_card(card);
    }
}

fn print_card(card: &PlanCard) {
    let owner = if card.owner.is_empty() {
        "unowned"
    } else {
        &card.owner
    };
    let mut bits = vec![
        card.ref_id.clone(),
        format!("[{}]", card.priority),
        format!("[{}]", card.status),
        card.lane.clone(),
        format!("owner={owner}"),
    ];
    if !card.stale_reason.is_empty() {
        bits.push(format!("stale={}", card.stale_reason));
    }
    println!("- {}", bits.join(" "));
    println!("  title: {}", short(&card.title, 96));
    if !card.next_action.is_empty() {
        println!("  next:  {}", short(&card.next_action, 96));
    }
    if !card.blockers.is_empty() {
        println!("  blockers: {}", short(&card.blockers, 96));
    }
}

fn active_cards(cards: &[PlanCard]) -> Vec<&PlanCard> {
    cards
        .iter()
        .filter(|card| card.status != "merged")
        .collect()
}

fn card_text(issue: &Value, card: &Value, title: &str) -> String {
    [
        title.to_string(),
        string_field(issue, "body"),
        string_field(card, "next_action"),
        string_field(card, "evidence"),
        string_field(card, "env"),
        string_field(card, "branch"),
    ]
    .join("\n")
    .to_ascii_lowercase()
}

fn lane_for(text: &str) -> String {
    LANES
        .iter()
        .find_map(|(lane, words)| {
            words
                .iter()
                .any(|word| text.contains(word))
                .then(|| lane.to_string())
        })
        .unwrap_or_else(|| "backlog/general".to_string())
}

fn priority_for(status: &str, text: &str, card: &Value, stale: bool) -> String {
    let explicit = explicit_priority(card);
    if !explicit.is_empty() {
        return explicit;
    }
    if matches!(status, "blocked" | "review")
        || stale
        || P0_WORDS.iter().any(|word| text.contains(word))
    {
        "P0".to_string()
    } else if P1_WORDS.iter().any(|word| text.contains(word)) {
        "P1".to_string()
    } else {
        "P2".to_string()
    }
}

fn explicit_priority(card: &Value) -> String {
    let priority = string_field(card, "priority").to_ascii_uppercase();
    let priority = if priority.is_empty() {
        string_field(card, "prio").to_ascii_uppercase()
    } else {
        priority
    };
    if matches!(priority.as_str(), "P0" | "P1" | "P2" | "P3") {
        priority
    } else {
        String::new()
    }
}

fn compact_title(title: &str) -> String {
    title
        .strip_prefix("airc-queue: ")
        .unwrap_or(title)
        .trim()
        .to_string()
}

fn parse_duration(value: &str, context: &str) -> Result<Duration, Box<dyn Error>> {
    let value = value.trim();
    if value.len() < 2 {
        return Err(
            format!("{context}: cannot parse --stale-after '{value}' (use 30m, 2h, 1d)").into(),
        );
    }
    let (amount, unit) = value.split_at(value.len() - 1);
    let amount: i64 = amount.trim().parse().map_err(|_| {
        format!("{context}: cannot parse --stale-after '{value}' (use 30m, 2h, 1d)")
    })?;
    match unit {
        "s" => Ok(Duration::seconds(amount)),
        "m" => Ok(Duration::minutes(amount)),
        "h" => Ok(Duration::hours(amount)),
        "d" => Ok(Duration::days(amount)),
        _ => {
            Err(format!("{context}: cannot parse --stale-after '{value}' (use 30m, 2h, 1d)").into())
        }
    }
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    let start = value.find(|ch: char| ch.is_ascii_digit())?;
    let tail = &value[start..];
    let end = tail
        .find('Z')
        .map(|index| start + index + 1)
        .unwrap_or(value.len());
    let raw = &value[start..end];
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%MZ")
                .ok()
                .map(|dt| dt.and_utc())
        })
}

fn shquote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn short(value: &str, limit: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        compact
    } else {
        compact
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>()
            .trim_end()
            .to_string()
            + "..."
    }
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "P0" => 0,
        "P1" => 1,
        "P2" => 2,
        "P3" => 3,
        _ => 9,
    }
}

fn status_rank(status: &str) -> u8 {
    match status {
        "review" => 0,
        "blocked" => 1,
        "claimed" => 2,
        "in-progress" => 3,
        "merged" => 4,
        _ => 9,
    }
}

fn count_object(counts: &BTreeMap<String, usize>) -> Value {
    Value::Object(
        counts
            .iter()
            .map(|(key, value)| (key.clone(), Value::Number((*value as u64).into())))
            .collect(),
    )
}

fn refs_object(items: impl Iterator<Item = (String, Vec<String>)>) -> Value {
    Value::Object(
        items
            .map(|(key, refs)| {
                (
                    key,
                    Value::Array(refs.into_iter().map(Value::String).collect()),
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use serde_json::json;

    #[test]
    fn lane_priority_and_stale_projection_match_queue_plan_shape() {
        let issues = json!([
            {
                "number": 11,
                "title": "airc-queue: Rust persona cognition",
                "url": "https://example/11",
                "updatedAt": "2026-05-20T00:00:00Z",
                "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"claimed\",\"owner\":\"codex\",\"branch\":\"feat/rust-persona\",\"last_heartbeat\":\"2020-01-01T00:00Z\",\"next_action\":\"Port runtime\"}\n```"
            },
            {
                "number": 12,
                "title": "airc-queue: Review queue automation",
                "url": "https://example/12",
                "updatedAt": "2026-05-20T00:00:00Z",
                "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"review\",\"owner\":\"claude\",\"next_action\":\"Merge\"}\n```"
            }
        ]);
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), issues.to_string()).unwrap();

        let board =
            QueueBoard::load("owner/repo", "codex", "30m", temp.path(), "queue plan").unwrap();

        assert_eq!(board.cards.len(), 2);
        let rust_card = board
            .cards
            .iter()
            .find(|card| card.ref_id == "owner/repo#11")
            .unwrap();
        assert_eq!(rust_card.priority, "P0");
        assert_eq!(rust_card.lane, "alpha-gap/rust-runtime");
        assert_eq!(rust_card.stale_reason, "stale-heartbeat");
        assert!(board.plan_json()["lanes"]["alpha-gap/rust-runtime"]
            .as_array()
            .unwrap()
            .contains(&Value::String("owner/repo#11".to_string())));
    }

    #[test]
    fn steward_recommends_claim_and_next_action_hygiene() {
        let issues = json!([
            {
                "number": 7,
                "title": "airc-queue: Fix tests",
                "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"claimed\",\"owner\":\"unclaimed\"}\n```"
            }
        ]);
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), issues.to_string()).unwrap();
        let board =
            QueueBoard::load("owner/repo", "codex", "30m", temp.path(), "queue steward").unwrap();
        let actions = steward_actions(&board);
        let kinds = actions
            .iter()
            .map(|action| action.kind.as_str())
            .collect::<BTreeSet<_>>();

        assert!(kinds.contains("claim-ready-card"));
        assert!(kinds.contains("fill-next-action"));
    }
}
