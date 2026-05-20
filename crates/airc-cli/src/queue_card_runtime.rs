use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::{Map, Value};

use crate::queue_card_commands::{json_obj, parse_card, read_json, string_field, value_text};

pub fn run_pongs(
    repo: &str,
    sweep_id: &str,
    since: &str,
    output_json: bool,
    cards_file: &Path,
    messages_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let issues = read_json(cards_file)?;
    let messages = read_messages(messages_file)?;
    let since_dt = parse_since(since, "queue pongs")?;
    let owners = owner_cards(&issues, repo)?;
    let responders = responder_map(repo, sweep_id, since_dt, &messages, Utc::now());
    let missing = owners
        .keys()
        .filter(|owner| !responders.contains_key(*owner))
        .cloned()
        .collect::<Vec<_>>();
    let payload = json_obj([
        ("repo", Value::String(repo.to_string())),
        ("sweep_id", Value::String(sweep_id.to_string())),
        ("since", Value::String(since.to_string())),
        (
            "responders",
            Value::Array(responders.values().map(PongReply::json).collect()),
        ),
        (
            "missing_owners",
            Value::Array(missing.iter().cloned().map(Value::String).collect()),
        ),
        (
            "open_owner_cards",
            Value::Object(
                owners
                    .iter()
                    .map(|(owner, refs)| {
                        (
                            owner.clone(),
                            Value::Array(refs.iter().cloned().map(Value::String).collect()),
                        )
                    })
                    .collect(),
            ),
        ),
        (
            "open_cards",
            Value::Array(
                open_cards(&issues)?
                    .into_iter()
                    .map(Value::Object)
                    .collect(),
            ),
        ),
    ]);
    if output_json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        render_pongs(repo, sweep_id, since, responders.values(), &missing);
    }
    Ok(())
}

pub fn run_availability(
    repo: &str,
    sweep_id: &str,
    since: &str,
    stale_after: &str,
    output_json: bool,
    cards_file: &Path,
    messages_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let issues = read_json(cards_file)?;
    let messages = read_messages(messages_file)?;
    let now = Utc::now();
    let since_dt = parse_since(since, "queue availability")?;
    let stale_threshold = parse_duration(stale_after, "queue availability")?;
    let cards = availability_cards(&issues, repo, now, stale_threshold)?;
    let owner_cards = owner_cards(&issues, repo)?;
    let responders = responder_map(repo, "", since_dt, &messages, now);
    let recent_activity = recent_activity(since_dt, &messages, now);
    let mut recent = recent_activity.values().cloned().collect::<Vec<_>>();
    recent.sort_by_key(|item| Reverse(item.ts_dt));
    let missing_owners = owner_cards
        .keys()
        .filter(|owner| !responders.contains_key(*owner) && !recent_activity.contains_key(*owner))
        .cloned()
        .collect::<Vec<_>>();
    let stale_cards = cards
        .iter()
        .filter(|card| !card.availability_reason.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    let payload = json_obj([
        ("repo", Value::String(repo.to_string())),
        (
            "now",
            Value::String(now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
        ),
        ("since", Value::String(since.to_string())),
        ("stale_after", Value::String(stale_after.to_string())),
        ("sweep_id", Value::String(sweep_id.to_string())),
        (
            "cards",
            Value::Array(cards.iter().map(AvailabilityCard::json).collect()),
        ),
        (
            "stale_cards",
            Value::Array(stale_cards.iter().map(AvailabilityCard::json).collect()),
        ),
        (
            "responders",
            Value::Array(responders.values().map(PongReply::json).collect()),
        ),
        (
            "recent_activity",
            Value::Array(recent.iter().map(RecentActivity::json).collect()),
        ),
        (
            "missing_owners",
            Value::Array(missing_owners.iter().cloned().map(Value::String).collect()),
        ),
        (
            "owner_cards",
            Value::Object(
                owner_cards
                    .iter()
                    .map(|(owner, refs)| {
                        (
                            owner.clone(),
                            Value::Array(refs.iter().cloned().map(Value::String).collect()),
                        )
                    })
                    .collect(),
            ),
        ),
        (
            "suggested_nudge",
            Value::String(format!("airc queue nudge {repo} --sweep-id {sweep_id}")),
        ),
        (
            "suggested_pongs",
            Value::String(format!(
                "airc queue pongs {repo} --sweep-id {sweep_id} --since {since}"
            )),
        ),
    ]);
    if output_json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        render_availability(
            repo,
            now,
            since,
            stale_after,
            cards.len(),
            responders.values(),
            recent.iter(),
            &stale_cards,
            &missing_owners,
            sweep_id,
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PongReply {
    nick: String,
    sender: String,
    ts: String,
    card: String,
    state: String,
    blocker: String,
    next: String,
    claim: String,
    sweep: String,
}

impl PongReply {
    fn json(&self) -> Value {
        json_obj([
            ("nick", Value::String(self.nick.clone())),
            ("sender", Value::String(self.sender.clone())),
            ("ts", Value::String(self.ts.clone())),
            ("card", Value::String(self.card.clone())),
            ("state", Value::String(self.state.clone())),
            ("blocker", Value::String(self.blocker.clone())),
            ("next", Value::String(self.next.clone())),
            ("claim", Value::String(self.claim.clone())),
            ("sweep", Value::String(self.sweep.clone())),
        ])
    }
}

#[derive(Debug, Clone)]
struct AvailabilityCard {
    number: Value,
    title: String,
    url: String,
    status: String,
    owner: String,
    last_heartbeat: String,
    heartbeat_age_seconds: Option<i64>,
    heartbeat_age: String,
    availability_reason: String,
    next_action: String,
}

impl AvailabilityCard {
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
            "heartbeat_age_seconds".to_string(),
            self.heartbeat_age_seconds
                .map(|value| Value::Number(value.into()))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "heartbeat_age".to_string(),
            Value::String(self.heartbeat_age.clone()),
        );
        object.insert(
            "availability_reason".to_string(),
            Value::String(self.availability_reason.clone()),
        );
        object.insert(
            "next_action".to_string(),
            Value::String(self.next_action.clone()),
        );
        Value::Object(object)
    }
}

#[derive(Debug, Clone)]
struct RecentActivity {
    peer: String,
    ts: String,
    age: String,
    ts_dt: DateTime<Utc>,
}

impl RecentActivity {
    fn json(&self) -> Value {
        json_obj([
            ("peer", Value::String(self.peer.clone())),
            ("ts", Value::String(self.ts.clone())),
            ("age", Value::String(self.age.clone())),
        ])
    }
}

fn owner_cards(
    issues: &Value,
    repo: &str,
) -> Result<BTreeMap<String, Vec<String>>, Box<dyn Error>> {
    let mut owners = BTreeMap::new();
    for issue in issue_array(issues, "queue pongs")? {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let card_value = Value::Object(card);
        let owner = normalized_owner(&string_field(&card_value, "owner"));
        let status = string_field(&card_value, "status");
        if owner.is_empty() || !active_status(&status) {
            continue;
        }
        let reference = format!(
            "{repo}#{}",
            issue
                .get("number")
                .map_or_else(|| "?".to_string(), value_text)
        );
        owners.entry(owner).or_insert_with(Vec::new).push(reference);
    }
    Ok(owners)
}

fn open_cards(issues: &Value) -> Result<Vec<Map<String, Value>>, Box<dyn Error>> {
    let mut cards = Vec::new();
    for issue in issue_array(issues, "queue pongs")? {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let card_value = Value::Object(card);
        let mut row = Map::new();
        row.insert(
            "number".to_string(),
            issue.get("number").cloned().unwrap_or(Value::Null),
        );
        row.insert(
            "owner".to_string(),
            Value::String(normalized_owner(&string_field(&card_value, "owner"))),
        );
        row.insert(
            "status".to_string(),
            Value::String(string_field(&card_value, "status")),
        );
        cards.push(row);
    }
    Ok(cards)
}

fn availability_cards(
    issues: &Value,
    repo: &str,
    now: DateTime<Utc>,
    stale_after: Duration,
) -> Result<Vec<AvailabilityCard>, Box<dyn Error>> {
    let mut cards = Vec::new();
    for issue in issue_array(issues, "queue availability")? {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let card_value = Value::Object(card);
        let status = nonempty(&string_field(&card_value, "status"), "unknown");
        let owner = normalized_owner(&string_field(&card_value, "owner"));
        let heartbeat = string_field(&card_value, "last_heartbeat");
        let heartbeat_dt = parse_embedded_utc(&heartbeat);
        let heartbeat_age_seconds = heartbeat_dt.map(|dt| (now - dt).num_seconds());
        let availability_reason = if active_status(&status) {
            if owner.is_empty() {
                "missing-owner".to_string()
            } else if heartbeat_dt.is_none() {
                "missing-heartbeat".to_string()
            } else if now - heartbeat_dt.unwrap() > stale_after {
                "stale-heartbeat".to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        cards.push(AvailabilityCard {
            number: issue.get("number").cloned().unwrap_or(Value::Null),
            title: clean_title(&string_field(issue, "title")),
            url: string_field(issue, "url"),
            status,
            owner,
            last_heartbeat: heartbeat,
            heartbeat_age_seconds,
            heartbeat_age: age_label(heartbeat_age_seconds),
            availability_reason,
            next_action: string_field(&card_value, "next_action"),
        });
    }
    let _ = repo;
    Ok(cards)
}

fn responder_map(
    repo: &str,
    sweep_id: &str,
    since: DateTime<Utc>,
    messages: &[Value],
    _now: DateTime<Utc>,
) -> BTreeMap<String, PongReply> {
    let mut responders = BTreeMap::new();
    for msg in messages {
        let ts = string_field(msg, "ts");
        let Some(ts_dt) = parse_rfc3339_utc(&ts) else {
            continue;
        };
        if ts_dt <= since {
            continue;
        }
        let text = string_field(msg, "msg");
        if text.contains("repo-nudge:") && text.contains("pong with:") {
            continue;
        }
        if !contains_pong_for_repo(&text, repo) {
            continue;
        }
        let fields = scan_fields(&text);
        if !sweep_id.is_empty() && fields.get("sweep").map(String::as_str) != Some(sweep_id) {
            continue;
        }
        let sender = nonempty(&string_field(msg, "from"), "?");
        let nick = pong_nick(&text, &sender);
        responders.insert(
            nick.clone(),
            PongReply {
                nick,
                sender,
                ts,
                card: field(&fields, "card"),
                state: field(&fields, "state"),
                blocker: field(&fields, "blocker"),
                next: field(&fields, "next"),
                claim: field(&fields, "claim"),
                sweep: field(&fields, "sweep"),
            },
        );
    }
    responders
}

fn recent_activity(
    since: DateTime<Utc>,
    messages: &[Value],
    now: DateTime<Utc>,
) -> BTreeMap<String, RecentActivity> {
    let mut recent = BTreeMap::new();
    for msg in messages {
        let ts = string_field(msg, "ts");
        let Some(ts_dt) = parse_rfc3339_utc(&ts) else {
            continue;
        };
        if ts_dt <= since {
            continue;
        }
        let sender = string_field(msg, "from");
        if sender.is_empty() || sender == "airc" {
            continue;
        }
        let should_replace = recent
            .get(&sender)
            .map(|current: &RecentActivity| ts_dt > current.ts_dt)
            .unwrap_or(true);
        if should_replace {
            recent.insert(
                sender.clone(),
                RecentActivity {
                    peer: sender,
                    ts,
                    age: age_label(Some((now - ts_dt).num_seconds())),
                    ts_dt,
                },
            );
        }
    }
    recent
}

fn render_pongs<'a>(
    repo: &str,
    sweep_id: &str,
    since: &str,
    responders: impl Iterator<Item = &'a PongReply>,
    missing: &[String],
) {
    let responders = responders.collect::<Vec<_>>();
    let label = if sweep_id.is_empty() {
        String::new()
    } else {
        format!(" sweep={sweep_id}")
    };
    println!("# airc-queue pongs — {repo}{label}");
    println!("since: {since}");
    if responders.is_empty() {
        println!("responders: none");
    } else {
        println!("responders ({}):", responders.len());
        for item in responders {
            println!(
                "  - {}: card={} state={} blocker={} next={} claim={}",
                item.nick,
                nonempty(&item.card, "?"),
                nonempty(&item.state, "?"),
                nonempty(&item.blocker, "?"),
                nonempty(&item.next, "?"),
                nonempty(&item.claim, "?")
            );
        }
    }
    if missing.is_empty() {
        println!("missing owners: none");
    } else {
        println!("missing owners ({}): {}", missing.len(), missing.join(", "));
    }
}

#[allow(clippy::too_many_arguments)]
fn render_availability<'a>(
    repo: &str,
    now: DateTime<Utc>,
    since: &str,
    stale_after: &str,
    card_count: usize,
    responders: impl Iterator<Item = &'a PongReply>,
    recent: impl Iterator<Item = &'a RecentActivity>,
    stale_cards: &[AvailabilityCard],
    missing_owners: &[String],
    sweep_id: &str,
) {
    let responders = responders.collect::<Vec<_>>();
    let recent = recent.collect::<Vec<_>>();
    println!("# airc-queue availability — {repo}");
    println!(
        "now_utc: {}",
        now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    );
    println!("since: {since}");
    println!("stale_after: {stale_after}");
    println!("open_cards: {card_count}");
    if responders.is_empty() {
        println!("repo-nudge responders: none");
    } else {
        println!("repo-nudge responders ({}):", responders.len());
        for item in responders {
            println!(
                "  - {}: card={} state={} blocker={} next={}",
                item.nick,
                nonempty(&item.card, "?"),
                nonempty(&item.state, "?"),
                nonempty(&item.blocker, "?"),
                nonempty(&item.next, "?")
            );
        }
    }
    if recent.is_empty() {
        println!("recent room activity: none");
    } else {
        println!("recent room activity ({}):", recent.len());
        for item in recent.iter().take(10) {
            println!("  - {}: last seen {} ago", item.peer, item.age);
        }
    }
    if stale_cards.is_empty() {
        println!("attention needed: none");
    } else {
        println!("attention needed ({}):", stale_cards.len());
        for card in stale_cards {
            let owner = nonempty(&card.owner, "(unowned)");
            println!(
                "  - {repo}#{} {} owner={} reason={} heartbeat={}",
                value_text(&card.number),
                card.status,
                owner,
                card.availability_reason,
                card.heartbeat_age
            );
        }
    }
    if missing_owners.is_empty() {
        println!("missing owners: none");
    } else {
        println!(
            "missing owners ({}): {}",
            missing_owners.len(),
            missing_owners.join(", ")
        );
    }
    println!("next:");
    println!("  airc queue nudge {repo} --sweep-id {sweep_id}");
    println!("  airc queue pongs {repo} --sweep-id {sweep_id} --since {since}");
}

fn read_messages(path: &Path) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut messages = Vec::new();
    for line in fs::read_to_string(path)?.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        messages.push(value);
    }
    Ok(messages)
}

fn issue_array<'a>(issues: &'a Value, command: &str) -> Result<&'a [Value], Box<dyn Error>> {
    issues
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{command}: issue JSON must be an array").into())
}

fn parse_duration(value: &str, command: &str) -> Result<Duration, Box<dyn Error>> {
    let value = value.trim();
    if value.len() < 2 {
        return Err(format!("{command}: cannot parse duration '{value}' (use 30m, 2h, 1d)").into());
    }
    let (amount, unit) = value.split_at(value.len() - 1);
    let amount: i64 = amount
        .trim()
        .parse()
        .map_err(|_| format!("{command}: cannot parse duration '{value}' (use 30m, 2h, 1d)"))?;
    match unit.trim() {
        "s" => Ok(Duration::seconds(amount)),
        "m" => Ok(Duration::minutes(amount)),
        "h" => Ok(Duration::hours(amount)),
        "d" => Ok(Duration::days(amount)),
        _ => Err(format!("{command}: cannot parse duration '{value}' (use 30m, 2h, 1d)").into()),
    }
}

fn parse_since(value: &str, command: &str) -> Result<DateTime<Utc>, Box<dyn Error>> {
    if value
        .trim()
        .chars()
        .last()
        .map(|unit| matches!(unit, 's' | 'm' | 'h' | 'd'))
        .unwrap_or(false)
    {
        return Ok(Utc::now() - parse_duration(value, command)?);
    }
    parse_rfc3339_utc(value)
        .ok_or_else(|| format!("{command}: cannot parse --since '{value}'").into())
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn parse_embedded_utc(value: &str) -> Option<DateTime<Utc>> {
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

fn contains_pong_for_repo(text: &str, repo: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&format!("pong: {}", repo.to_ascii_lowercase()))
}

fn scan_fields(text: &str) -> BTreeMap<String, String> {
    let bytes = text.as_bytes();
    let mut fields = BTreeMap::new();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] != b'=' {
            idx += 1;
            continue;
        }
        let mut key_start = idx;
        while key_start > 0 && is_key_byte(bytes[key_start - 1]) {
            key_start -= 1;
        }
        if key_start == idx {
            idx += 1;
            continue;
        }
        let key = &text[key_start..idx];
        let value_start = idx + 1;
        if value_start >= bytes.len() {
            break;
        }
        let (value, next_idx) = if bytes[value_start] == b'<' {
            match text[value_start + 1..].find('>') {
                Some(end) => (
                    &text[value_start + 1..value_start + 1 + end],
                    value_start + 2 + end,
                ),
                None => ("", value_start + 1),
            }
        } else {
            let mut end = value_start;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            (&text[value_start..end], end)
        };
        fields.insert(key.to_string(), value.to_string());
        idx = next_idx;
    }
    fields
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn pong_nick(text: &str, sender: &str) -> String {
    for part in text.split('—').map(str::trim) {
        if !part.is_empty() && !part.starts_with("pong:") && !part.contains('=') {
            return part.to_string();
        }
    }
    sender.to_string()
}

fn normalized_owner(owner: &str) -> String {
    if owner == "unclaimed" {
        String::new()
    } else {
        owner.trim().to_string()
    }
}

fn active_status(status: &str) -> bool {
    matches!(status, "claimed" | "in-progress" | "review")
}

fn clean_title(title: &str) -> String {
    title
        .strip_prefix("airc-queue: ")
        .unwrap_or(title)
        .to_string()
}

fn nonempty(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn field(fields: &BTreeMap<String, String>, key: &str) -> String {
    fields.get(key).cloned().unwrap_or_default()
}

fn age_label(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return "unknown".to_string();
    };
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scan_fields_handles_angle_values_and_plain_values() {
        let fields = scan_fields(
            "pong: CambrianTech/airc — codex — card=<CambrianTech/airc#1> state=coding blocker=<none> next=<merge it>",
        );

        assert_eq!(fields["card"], "CambrianTech/airc#1");
        assert_eq!(fields["state"], "coding");
        assert_eq!(fields["blocker"], "none");
        assert_eq!(fields["next"], "merge it");
    }

    #[test]
    fn responder_map_filters_repo_nudge_prompts_and_sweep() {
        let messages = vec![
            json!({"ts":"2026-05-20T00:01:00Z","from":"airc","msg":"repo-nudge: x pong with:"}),
            json!({"ts":"2026-05-20T00:02:00Z","from":"codex","msg":"pong: CambrianTech/airc — codex — sweep=s1 card=<CambrianTech/airc#1> state=testing"}),
            json!({"ts":"2026-05-20T00:03:00Z","from":"other","msg":"pong: CambrianTech/other — other — sweep=s1 card=<x> state=idle"}),
        ];

        let responders = responder_map(
            "CambrianTech/airc",
            "s1",
            parse_rfc3339_utc("2026-05-20T00:00:00Z").unwrap(),
            &messages,
            Utc::now(),
        );

        assert_eq!(responders.len(), 1);
        assert_eq!(responders["codex"].state, "testing");
    }

    #[test]
    fn availability_cards_detect_stale_claims() {
        let issues = json!([{
            "number": 1,
            "title": "airc-queue: Runtime",
            "url": "u",
            "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"codex\",\"status\":\"claimed\",\"last_heartbeat\":\"2026-05-19T00:00:00Z\"}\n```"
        }]);

        let cards = availability_cards(
            &issues,
            "CambrianTech/airc",
            parse_rfc3339_utc("2026-05-20T00:00:00Z").unwrap(),
            Duration::minutes(30),
        )
        .unwrap();

        assert_eq!(cards[0].availability_reason, "stale-heartbeat");
        assert_eq!(cards[0].heartbeat_age, "1d");
    }
}
