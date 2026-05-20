use std::error::Error;
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::{Map, Value};

use crate::queue_card_commands::{
    json_obj, nonempty_or, parse_card, read_json, string_field, value_text,
};

pub fn run_list(
    repo: &str,
    filter_owner: &str,
    filter_status: &str,
    output_json: bool,
    raw_json_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let issues = read_json(raw_json_file)?;
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let cards = list_cards(&issues, filter_owner, filter_status)?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_obj([
                ("now_utc", Value::String(now)),
                ("repo", Value::String(repo.to_string())),
                (
                    "cards",
                    Value::Array(cards.iter().map(QueueCardListEntry::json).collect())
                ),
            ]))?
        );
    } else {
        render_list(repo, &now, filter_owner, filter_status, &cards);
    }
    Ok(())
}

pub fn run_stale(
    repo: &str,
    stale_after: &str,
    output_json: bool,
    raw_json_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let issues = read_json(raw_json_file)?;
    let threshold = parse_duration(stale_after)?;
    let now = Utc::now();
    let rows = stale_cards(&issues, now, threshold)?;
    let now_text = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_obj([
                ("repo", Value::String(repo.to_string())),
                ("now", Value::String(now_text)),
                ("stale_after", Value::String(stale_after.to_string())),
                (
                    "cards",
                    Value::Array(rows.iter().map(StaleCard::json).collect())
                ),
            ]))?
        );
    } else {
        render_stale(repo, &now_text, stale_after, &rows);
    }
    Ok(())
}

pub fn run_next(
    repo: &str,
    owner: &str,
    base: &str,
    repo_root: &str,
    output_json: bool,
    raw_json_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let issues = read_json(raw_json_file)?;
    let rows = next_cards(&issues, repo, owner, base, repo_root)?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_obj([
                ("repo", Value::String(repo.to_string())),
                ("owner", Value::String(owner.to_string())),
                (
                    "candidates",
                    Value::Array(rows.iter().map(NextCard::json).collect())
                ),
            ]))?
        );
    } else {
        render_next(repo, owner, &rows);
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct QueueCardListEntry {
    number: Value,
    title: String,
    url: String,
    created_at: String,
    updated_at: String,
    card: Map<String, Value>,
}

impl QueueCardListEntry {
    fn json(&self) -> Value {
        json_obj([
            ("number", self.number.clone()),
            ("title", Value::String(self.title.clone())),
            ("url", Value::String(self.url.clone())),
            ("createdAt", Value::String(self.created_at.clone())),
            ("updatedAt", Value::String(self.updated_at.clone())),
            ("card", Value::Object(self.card.clone())),
        ])
    }
}

fn list_cards(
    issues: &Value,
    filter_owner: &str,
    filter_status: &str,
) -> Result<Vec<QueueCardListEntry>, Box<dyn Error>> {
    let issues = issues
        .as_array()
        .ok_or("queue list: issue JSON must be an array")?;
    let mut cards = Vec::new();
    for issue in issues {
        let card = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
            .unwrap_or_default();
        if !filter_owner.is_empty()
            && card.get("owner").and_then(Value::as_str).unwrap_or("") != filter_owner
        {
            continue;
        }
        if !filter_status.is_empty()
            && card.get("status").and_then(Value::as_str).unwrap_or("") != filter_status
        {
            continue;
        }
        cards.push(QueueCardListEntry {
            number: issue.get("number").cloned().unwrap_or(Value::Null),
            title: clean_title(&string_field(issue, "title")),
            url: string_field(issue, "url"),
            created_at: string_field(issue, "createdAt"),
            updated_at: string_field(issue, "updatedAt"),
            card,
        });
    }
    Ok(cards)
}

fn render_list(
    repo: &str,
    now: &str,
    filter_owner: &str,
    filter_status: &str,
    cards: &[QueueCardListEntry],
) {
    if cards.is_empty() {
        let mut suffix = String::new();
        if !filter_owner.is_empty() {
            suffix.push_str(&format!(" owner={filter_owner}"));
        }
        if !filter_status.is_empty() {
            suffix.push_str(&format!(" status={filter_status}"));
        }
        println!("# airc-queue — {repo}");
        println!("now_utc: {now}");
        println!("No open airc-queue cards on {repo}{suffix}.");
        return;
    }
    println!("# airc-queue — {repo} ({} open)", cards.len());
    println!("now_utc: {now}");
    for entry in cards {
        println!();
        println!("## #{} — {}", value_text(&entry.number), entry.title);
        println!("  url:           {}", entry.url);
        print_card_field(&entry.card, "id", "id");
        print_card_field(&entry.card, "branch", "branch");
        print_card_field(&entry.card, "owner", "owner");
        print_card_field(&entry.card, "status", "status");
        print_card_field(&entry.card, "blockers", "blockers");
        print_card_field(&entry.card, "env", "env");
        print_card_field(&entry.card, "evidence", "evidence");
        print_card_field(&entry.card, "next_action", "next");
        print_card_field(&entry.card, "last_heartbeat", "last heartbeat");
    }
}

#[derive(Debug, Clone)]
struct StaleCard {
    number: Value,
    title: String,
    url: String,
    status: String,
    owner: String,
    last_heartbeat: String,
    age_seconds: Option<i64>,
    reason: String,
    next_action: String,
}

impl StaleCard {
    fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("number".to_string(), self.number.clone());
        object.insert("title".to_string(), Value::String(self.title.clone()));
        object.insert("url".to_string(), Value::String(self.url.clone()));
        object.insert("status".to_string(), Value::String(self.status.clone()));
        object.insert("owner".to_string(), Value::String(self.owner.clone()));
        object.insert(
            "last_heartbeat".to_string(),
            Value::String(self.last_heartbeat.clone()),
        );
        object.insert(
            "age_seconds".to_string(),
            self.age_seconds
                .map(|value| Value::Number(value.into()))
                .unwrap_or(Value::Null),
        );
        object.insert("reason".to_string(), Value::String(self.reason.clone()));
        object.insert(
            "next_action".to_string(),
            Value::String(self.next_action.clone()),
        );
        Value::Object(object)
    }
}

fn stale_cards(
    issues: &Value,
    now: DateTime<Utc>,
    threshold: Duration,
) -> Result<Vec<StaleCard>, Box<dyn Error>> {
    let issues = issues
        .as_array()
        .ok_or("queue stale: issue JSON must be an array")?;
    let mut rows = Vec::new();
    for issue in issues {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let card_value = Value::Object(card);
        let status = nonempty_or(&string_field(&card_value, "status"), "unknown");
        let mut owner = string_field(&card_value, "owner");
        if owner == "unclaimed" {
            owner.clear();
        }
        if !matches!(status.as_str(), "claimed" | "in-progress" | "review") {
            continue;
        }
        let heartbeat = string_field(&card_value, "last_heartbeat");
        let hb_dt = parse_heartbeat(&heartbeat);
        let mut age_seconds = None;
        let reason = if owner.is_empty() {
            "missing-owner".to_string()
        } else if hb_dt.is_none() {
            "missing-heartbeat".to_string()
        } else if let Some(heartbeat_at) = hb_dt {
            let age = now - heartbeat_at;
            age_seconds = Some(age.num_seconds());
            if age > threshold {
                "stale-heartbeat".to_string()
            } else {
                String::new()
            }
        } else {
            "missing-heartbeat".to_string()
        };
        if reason.is_empty() {
            continue;
        }
        rows.push(StaleCard {
            number: issue.get("number").cloned().unwrap_or(Value::Null),
            title: string_field(issue, "title"),
            url: string_field(issue, "url"),
            status,
            owner,
            last_heartbeat: heartbeat,
            age_seconds,
            reason,
            next_action: string_field(&card_value, "next_action"),
        });
    }
    Ok(rows)
}

fn render_stale(repo: &str, now: &str, stale_after: &str, rows: &[StaleCard]) {
    println!("# airc-queue stale — {repo}");
    println!("now_utc: {now}");
    println!("stale_after: {stale_after}");
    if rows.is_empty() {
        println!("No stale owned cards found.");
    }
    for row in rows {
        println!();
        println!("## #{} — {}", value_text(&row.number), row.title);
        println!("  url:            {}", row.url);
        println!("  status:         {}", row.status);
        if !row.owner.is_empty() {
            println!("  owner:          {}", row.owner);
        }
        if !row.last_heartbeat.is_empty() {
            println!("  last heartbeat: {}", row.last_heartbeat);
        }
        if let Some(age) = row.age_seconds {
            println!("  heartbeat age:  {age}s");
        }
        println!("  reason:         {}", row.reason);
        if !row.next_action.is_empty() {
            println!("  next:           {}", row.next_action);
        }
    }
}

#[derive(Debug, Clone)]
struct NextCard {
    rank: i64,
    number: Value,
    title: String,
    url: String,
    reference: String,
    status: String,
    owner: String,
    branch: String,
    environment: String,
    next_action: String,
    claim_command: String,
    lane_command: String,
}

impl NextCard {
    fn json(&self) -> Value {
        json_obj([
            ("rank", Value::Number(self.rank.into())),
            ("number", self.number.clone()),
            ("title", Value::String(self.title.clone())),
            ("url", Value::String(self.url.clone())),
            ("ref", Value::String(self.reference.clone())),
            ("status", Value::String(self.status.clone())),
            ("owner", Value::String(self.owner.clone())),
            ("branch", Value::String(self.branch.clone())),
            ("env", Value::String(self.environment.clone())),
            ("next_action", Value::String(self.next_action.clone())),
            ("claim_command", Value::String(self.claim_command.clone())),
            ("lane_command", Value::String(self.lane_command.clone())),
        ])
    }
}

fn next_cards(
    issues: &Value,
    repo: &str,
    owner: &str,
    base: &str,
    repo_root: &str,
) -> Result<Vec<NextCard>, Box<dyn Error>> {
    let issues = issues
        .as_array()
        .ok_or("queue next: issue JSON must be an array")?;
    let mut rows = Vec::new();
    for issue in issues {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let card_value = Value::Object(card);
        let status = nonempty_or(&string_field(&card_value, "status"), "claimed");
        let mut card_owner = string_field(&card_value, "owner");
        if card_owner == "unclaimed" {
            card_owner.clear();
        }
        let rank = next_score(&status, &card_owner, owner);
        if rank >= 9 {
            continue;
        }
        let number = issue.get("number").cloned().unwrap_or(Value::Null);
        let reference = format!("{repo}#{}", value_text(&number));
        let branch = string_field(&card_value, "branch");
        let mut lane_cmd = format!(
            "airc lane create {} --base {}",
            shquote(&reference),
            shquote(base)
        );
        if !branch.is_empty() {
            lane_cmd.push_str(&format!(" --branch {}", shquote(&branch)));
        }
        if !repo_root.is_empty() {
            lane_cmd.push_str(&format!(" --repo {}", shquote(repo_root)));
        }
        let claim_command = format!(
            "airc queue claim {} --owner {}",
            shquote(&reference),
            shquote(owner)
        );
        rows.push(NextCard {
            rank,
            number,
            title: clean_title(&string_field(issue, "title")),
            url: string_field(issue, "url"),
            reference,
            status,
            owner: card_owner,
            branch,
            environment: string_field(&card_value, "env"),
            next_action: string_field(&card_value, "next_action"),
            claim_command,
            lane_command: lane_cmd,
        });
    }
    rows.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then(value_text(&left.number).cmp(&value_text(&right.number)))
    });
    Ok(rows)
}

fn render_next(repo: &str, owner: &str, rows: &[NextCard]) {
    println!("# airc-queue next — {repo}");
    println!("owner: {owner}");
    if rows.is_empty() {
        println!("No claimable queue cards found.");
        println!("Try: airc queue nudge {repo} --message \"idle agent looking for work\"");
    }
    for (idx, row) in rows.iter().take(10).enumerate() {
        let owner_label = if row.owner.is_empty() {
            "(unowned)"
        } else {
            &row.owner
        };
        println!();
        println!("## {}. {} — {}", idx + 1, row.reference, row.title);
        println!("  status: {} owner={owner_label}", row.status);
        if !row.environment.is_empty() {
            println!("  env:    {}", row.environment);
        }
        if !row.branch.is_empty() {
            println!("  branch: {}", row.branch);
        }
        if !row.next_action.is_empty() {
            println!("  next:   {}", row.next_action);
        }
        println!("  claim:  {}", row.claim_command);
        println!("  lane:   {}", row.lane_command);
    }
}

fn clean_title(title: &str) -> String {
    title
        .strip_prefix("airc-queue: ")
        .unwrap_or(title)
        .to_string()
}

fn print_card_field(card: &Map<String, Value>, key: &str, label: &str) {
    let value = card
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let Some(value) = value {
        let label = format!("{label}:");
        println!("  {label:<15}{value}");
    }
}

fn parse_duration(value: &str) -> Result<Duration, Box<dyn Error>> {
    let value = value.trim();
    if value.len() < 2 {
        return Err(format!("cannot parse duration '{value}' (use 30m, 2h, 1d)").into());
    }
    let (amount, unit) = value.split_at(value.len() - 1);
    let amount: i64 = amount
        .trim()
        .parse()
        .map_err(|_| format!("cannot parse duration '{value}' (use 30m, 2h, 1d)"))?;
    match unit {
        "s" => Ok(Duration::seconds(amount)),
        "m" => Ok(Duration::minutes(amount)),
        "h" => Ok(Duration::hours(amount)),
        "d" => Ok(Duration::days(amount)),
        _ => Err(format!("cannot parse duration '{value}' (use 30m, 2h, 1d)").into()),
    }
}

fn parse_heartbeat(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    let start = value.find(|ch: char| ch.is_ascii_digit())?;
    let tail = &value[start..];
    for len in [20usize, 17usize] {
        if tail.len() < len {
            continue;
        }
        let raw = &tail[..len];
        if len == 20 {
            if let Ok(dt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%SZ") {
                return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
            }
        } else if let Ok(dt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%MZ") {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
        }
    }
    None
}

fn next_score(status: &str, card_owner: &str, owner: &str) -> i64 {
    match (status, card_owner.is_empty(), card_owner == owner) {
        ("claimed", true, _) => 0,
        ("claimed", false, _) => 1,
        ("blocked", true, _) => 2,
        ("review", _, _) => 3,
        ("in-progress", _, true) => 4,
        _ => 9,
    }
}

fn shquote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_cards_filters_owner_and_status() {
        let issues = json!([
            {"number":1,"title":"airc-queue: A","url":"u","body":"```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"codex\",\"status\":\"claimed\"}\n```"},
            {"number":2,"title":"B","url":"u","body":"```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"other\",\"status\":\"claimed\"}\n```"}
        ]);

        let cards = list_cards(&issues, "codex", "claimed").unwrap();

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "A");
    }

    #[test]
    fn stale_cards_detects_missing_heartbeat() {
        let issues = json!([{
            "number": 1,
            "title": "A",
            "url": "u",
            "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"codex\",\"status\":\"in-progress\"}\n```"
        }]);

        let rows = stale_cards(&issues, Utc::now(), Duration::minutes(30)).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, "missing-heartbeat");
    }

    #[test]
    fn next_cards_ranks_claimable_cards() {
        let issues = json!([
            {"number":2,"title":"B","url":"u","body":"```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"codex\",\"status\":\"in-progress\"}\n```"},
            {"number":1,"title":"A","url":"u","body":"```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"\",\"status\":\"claimed\",\"branch\":\"feat/a\"}\n```"}
        ]);

        let rows = next_cards(
            &issues,
            "CambrianTech/airc",
            "codex",
            "rust-rewrite",
            "/repo",
        )
        .unwrap();

        assert_eq!(rows[0].reference, "CambrianTech/airc#1");
        assert!(rows[0].lane_command.contains("--branch 'feat/a'"));
        assert!(rows[0].claim_command.contains("--owner 'codex'"));
    }
}
