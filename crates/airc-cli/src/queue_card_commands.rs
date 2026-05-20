use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

const CARD_KIND: &str = "airc-queue-card-v1";
const CARD_FENCE_START: &str = "```json";
const CARD_FENCE_END: &str = "```";
const STATUS_LOG_HEADER: &str = "## Status log";

#[derive(Debug)]
pub struct QueueCardInput {
    pub id: String,
    pub branch: String,
    pub owner: String,
    pub status: String,
    pub blockers: String,
    pub environment: String,
    pub evidence: String,
    pub next_action: String,
    pub last_heartbeat: String,
}

pub fn run_body(input: QueueCardInput) -> Result<(), Box<dyn Error>> {
    print!("{}", build_body(input)?);
    Ok(())
}

pub fn run_mutate_body(
    body_file: &Path,
    mutations_file: &Path,
    log_msg: &str,
    timestamp: &str,
) -> Result<(), Box<dyn Error>> {
    let body = fs::read_to_string(body_file)?;
    let mutations = fs::read_to_string(mutations_file)?;
    print!("{}", mutate_body(&body, &mutations, log_msg, timestamp)?);
    Ok(())
}

pub fn run_claim_fields(body_file: &Path) -> Result<(), Box<dyn Error>> {
    let body = fs::read_to_string(body_file)?;
    let card = parse_card(&body)
        .ok_or("queue claim: no kind=airc-queue-card-v1 envelope found in body")?;
    let card_value = Value::Object(card);
    let mut owner = string_field(&card_value, "owner");
    if owner == "unclaimed" {
        owner.clear();
    }
    println!("{owner}");
    println!("{}", string_field(&card_value, "status"));
    Ok(())
}

pub fn run_dispatch_message(
    target_agent: &str,
    extra_message: &str,
    next_json_file: &Path,
) -> Result<(), Box<dyn Error>> {
    let raw = fs::read_to_string(next_json_file)?;
    println!("{}", dispatch_message(target_agent, extra_message, &raw)?);
    Ok(())
}

pub fn run_adopt_body(
    issue_json_file: &Path,
    queue_body_file: &Path,
    force: bool,
) -> Result<(), Box<dyn Error>> {
    let issue = read_json(issue_json_file)?;
    let queue_body = fs::read_to_string(queue_body_file)?;
    print!("{}", adopt_body(&issue, &queue_body, force)?);
    Ok(())
}

pub fn run_nudge_summary(raw_json_file: &Path) -> Result<(), Box<dyn Error>> {
    let issues = read_json(raw_json_file)?;
    println!("{}", nudge_summary(&issues)?);
    Ok(())
}

pub fn run_nudge_card_meta(issue_file: &Path) -> Result<(), Box<dyn Error>> {
    let issue = read_json(issue_file)?;
    let (title, status, owner, branch) = nudge_card_meta(&issue)?;
    println!("{title}");
    println!("{status}");
    println!("{owner}");
    println!("{branch}");
    Ok(())
}

pub fn run_close_merged_meta(pr_file: &Path) -> Result<(), Box<dyn Error>> {
    let pr = read_json(pr_file)?;
    let merge_commit = pr.get("mergeCommit").and_then(Value::as_object);
    let sha = merge_commit
        .and_then(|commit| commit.get("oid"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = string_field(&pr, "title");
    let body = string_field(&pr, "body");
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        string_field(&pr, "mergedAt"),
        string_field(&pr, "baseRefName"),
        sha,
        string_field(&pr, "url"),
        title.chars().count(),
        body.chars().count()
    );
    Ok(())
}

pub fn run_close_merged_refs(pr_file: &Path, default_repo: &str) -> Result<(), Box<dyn Error>> {
    let pr = read_json(pr_file)?;
    let title = string_field(&pr, "title");
    let body = string_field(&pr, "body");
    for reference in close_merged_refs(&format!("{title}\n{body}"), default_repo) {
        println!("{reference}");
    }
    Ok(())
}

pub fn run_card_status(body_file: &Path) -> Result<(), Box<dyn Error>> {
    let body = fs::read_to_string(body_file)?;
    match parse_card(&body) {
        Some(card) => {
            let status = string_field(&Value::Object(card), "status");
            if status.is_empty() {
                println!("unknown");
            } else {
                println!("{status}");
            }
        }
        None => println!("not-a-card"),
    }
    Ok(())
}

fn build_body(input: QueueCardInput) -> Result<String, Box<dyn Error>> {
    let mut card = Map::new();
    card.insert("kind".to_string(), Value::String(CARD_KIND.to_string()));
    insert_nonempty(&mut card, "id", &input.id);
    insert_nonempty(&mut card, "branch", &input.branch);
    insert_nonempty(&mut card, "owner", &input.owner);
    insert_nonempty(&mut card, "status", &input.status);
    insert_nonempty(&mut card, "blockers", &input.blockers);
    insert_nonempty(&mut card, "env", &input.environment);
    insert_nonempty(&mut card, "evidence", &input.evidence);
    insert_nonempty(&mut card, "next_action", &input.next_action);
    insert_nonempty(&mut card, "last_heartbeat", &input.last_heartbeat);
    let card_json = serde_json::to_string_pretty(&Value::Object(card))?;
    Ok(format!(
        "**airc-queue card**\n\n{}\n\n```json\n{}\n```\n\n{}\n",
        "Coordinates work via the AIRC queue substrate (airc#562). Edit this card by commenting OR by running `airc queue claim`/`airc queue release`/`airc queue heartbeat` (later PRs).",
        card_json,
        "Close this issue when the work is done (status=merged/abandoned).",
    ))
}

fn mutate_body(
    body: &str,
    mutations: &str,
    log_msg: &str,
    timestamp: &str,
) -> Result<String, Box<dyn Error>> {
    let found = find_card_block(body)
        .ok_or("queue mutate: no kind=airc-queue-card-v1 envelope found in body")?;
    let mut card = found.card;
    for raw in mutations.lines() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        if let Some(keyval) = raw.strip_prefix("set:") {
            let (key, value) = keyval
                .split_once('=')
                .ok_or_else(|| format!("queue mutate: malformed --set: {keyval}"))?;
            card.insert(
                key.trim().to_string(),
                Value::String(value.trim().to_string()),
            );
        } else if let Some(key) = raw.strip_prefix("clear:") {
            card.remove(key.trim());
        } else {
            return Err(format!("queue mutate: malformed mutation: {raw}").into());
        }
    }

    let new_block = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&Value::Object(card))?
    );
    let body_with_card = format!(
        "{}{}{}",
        &body[..found.start],
        new_block,
        &body[found.end..]
    );
    let log_line = format!("- {timestamp} — {log_msg}");
    if body_with_card.contains(STATUS_LOG_HEADER) {
        Ok(body_with_card.replacen(
            STATUS_LOG_HEADER,
            &format!("{STATUS_LOG_HEADER}\n\n{log_line}"),
            1,
        ))
    } else {
        Ok(format!(
            "{}\n\n{STATUS_LOG_HEADER}\n\n{log_line}\n",
            body_with_card.trim_end()
        ))
    }
}

fn dispatch_message(
    target_agent: &str,
    extra_message: &str,
    raw: &str,
) -> Result<String, Box<dyn Error>> {
    let data: Value = serde_json::from_str(raw)?;
    let items: &[Value] = if let Some(items) = data.get("candidates").and_then(Value::as_array) {
        items.as_slice()
    } else if let Some(items) = data.get("items").and_then(Value::as_array) {
        items.as_slice()
    } else if let Some(items) = data.get("results").and_then(Value::as_array) {
        items.as_slice()
    } else if let Some(items) = data.as_array() {
        items.as_slice()
    } else {
        &[]
    };
    let top = items.first().ok_or("ERR:no-items")?;
    let number = top
        .get("number")
        .map_or_else(|| "?".to_string(), value_text);
    let title = truncate_chars(&string_field(top, "title"), 80);
    let url = string_field(top, "url");
    let claim = string_field(top, "claim_command");
    let claim = if claim.is_empty() {
        string_field(top, "claim")
    } else {
        claim
    };
    let lane = string_field(top, "lane_command");
    let lane = if lane.is_empty() {
        string_field(top, "lane")
    } else {
        lane
    };

    let mut parts = vec![
        format!("📋 hand-out for @{target_agent}: #{number} — {title}"),
        nonempty_prefix("   ", &url),
        nonempty_prefix("   claim: ", &claim),
        nonempty_prefix("   lane:  ", &lane),
    ];
    if !extra_message.trim().is_empty() {
        parts.push(format!("   note: {}", extra_message.trim()));
    }
    parts.retain(|item| !item.is_empty());
    Ok(parts.join("\n"))
}

fn adopt_body(issue: &Value, queue_body: &str, force: bool) -> Result<String, Box<dyn Error>> {
    let old_body = issue.get("body").and_then(Value::as_str).unwrap_or("");
    if parse_card(old_body).is_some() && !force {
        return Err("queue adopt: issue already has an airc-queue-card-v1 envelope; pass --force to rewrite".into());
    }
    let original = if old_body.trim().is_empty() {
        "\n\n## Original issue body\n\n_No pre-adoption body._\n".to_string()
    } else {
        format!(
            "\n\n## Original issue body\n\n<details>\n<summary>Pre-adoption body</summary>\n\n{}\n\n</details>\n",
            old_body.trim_end()
        )
    };
    Ok(format!("{}{}", queue_body.trim_end(), original))
}

fn nudge_summary(issues: &Value) -> Result<String, Box<dyn Error>> {
    let issues = issues
        .as_array()
        .ok_or("queue nudge: issue JSON must be an array")?;
    let mut items = Vec::new();
    for issue in issues {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let title = string_field(issue, "title");
        let title = title
            .strip_prefix("airc-queue: ")
            .unwrap_or(&title)
            .to_string();
        let status = nonempty_or(
            &string_field(&Value::Object(card.clone()), "status"),
            "unknown",
        );
        let mut owner = string_field(&Value::Object(card.clone()), "owner");
        if owner == "unclaimed" {
            owner.clear();
        }
        let branch = string_field(&Value::Object(card), "branch");
        let mut bit = format!(
            "#{} {status}",
            issue
                .get("number")
                .map_or_else(|| "?".to_string(), value_text)
        );
        if !owner.is_empty() {
            bit.push_str(&format!(" owner={owner}"));
        }
        if !branch.is_empty() {
            bit.push_str(&format!(" branch={branch}"));
        }
        if !title.is_empty() {
            bit.push_str(&format!(" '{}'", truncate_chars(&title, 60)));
        }
        items.push(bit);
    }
    if items.is_empty() {
        Ok("no open queue cards".to_string())
    } else {
        Ok(items.into_iter().take(10).collect::<Vec<_>>().join("; "))
    }
}

fn nudge_card_meta(issue: &Value) -> Result<(String, String, String, String), Box<dyn Error>> {
    let title = nonempty_or(&string_field(issue, "title"), "(no title)");
    let body = string_field(issue, "body");
    let card = parse_card(&body).ok_or("queue nudge: issue has no airc-queue-card-v1 envelope")?;
    let card_value = Value::Object(card);
    Ok((
        title,
        nonempty_or(&string_field(&card_value, "status"), "unknown"),
        string_field(&card_value, "owner"),
        string_field(&card_value, "branch"),
    ))
}

fn close_merged_refs(text: &str, default_repo: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((keyword_start, keyword_end)) = next_closing_keyword(text, cursor) else {
            break;
        };
        let mut pos = skip_spaces_and_colon(text, keyword_end);
        let mut first = true;
        loop {
            if !first {
                let Some(next_pos) = skip_ref_separator(text, pos) else {
                    break;
                };
                pos = next_pos;
            }
            let Some((reference, end)) = parse_close_ref(text, pos, default_repo) else {
                break;
            };
            if seen.insert(reference.clone()) {
                refs.push(reference);
            }
            pos = end;
            first = false;
        }
        if pos == skip_spaces_and_colon(text, keyword_end) {
            cursor = keyword_start
                + text[keyword_start..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
        } else {
            cursor = pos;
        }
    }
    refs
}

fn next_closing_keyword(text: &str, start: usize) -> Option<(usize, usize)> {
    let lower = text[start..].to_ascii_lowercase();
    let keywords = [
        "closes", "closed", "close", "fixes", "fixed", "fix", "resolves", "resolved", "resolve",
    ];
    let mut best: Option<(usize, usize)> = None;
    for keyword in keywords {
        let mut search_from = 0;
        while let Some(relative) = lower[search_from..].find(keyword) {
            let begin = start + search_from + relative;
            let end = begin + keyword.len();
            if is_word_start_boundary(text, begin) && is_word_end_boundary(text, end) {
                match best {
                    Some((best_begin, _)) if begin >= best_begin => {}
                    _ => best = Some((begin, end)),
                }
                break;
            }
            search_from += relative + keyword.len();
        }
    }
    best
}

fn is_word_start_boundary(text: &str, index: usize) -> bool {
    !matches!(
        text[..index].chars().next_back(),
        Some(ch) if ch.is_ascii_alphanumeric() || ch == '_'
    )
}

fn is_word_end_boundary(text: &str, index: usize) -> bool {
    !matches!(
        text[index..].chars().next(),
        Some(ch) if ch.is_ascii_alphanumeric() || ch == '_'
    )
}

fn skip_spaces_and_colon(text: &str, mut pos: usize) -> usize {
    while let Some(ch) = text[pos..].chars().next() {
        if ch.is_whitespace() || ch == ':' {
            pos += ch.len_utf8();
        } else {
            break;
        }
    }
    pos
}

fn skip_ref_separator(text: &str, mut pos: usize) -> Option<usize> {
    while let Some(ch) = text[pos..].chars().next() {
        if ch.is_whitespace() {
            pos += ch.len_utf8();
        } else {
            break;
        }
    }
    if text[pos..].starts_with(',') {
        pos += 1;
        while let Some(ch) = text[pos..].chars().next() {
            if ch.is_whitespace() {
                pos += ch.len_utf8();
            } else {
                break;
            }
        }
        return Some(pos);
    }
    let rest = &text[pos..];
    if rest.len() >= 3
        && rest[..3].eq_ignore_ascii_case("and")
        && is_word_end_boundary(text, pos + 3)
    {
        pos += 3;
        while let Some(ch) = text[pos..].chars().next() {
            if ch.is_whitespace() {
                pos += ch.len_utf8();
            } else {
                break;
            }
        }
        return Some(pos);
    }
    None
}

fn parse_close_ref(text: &str, pos: usize, default_repo: &str) -> Option<(String, usize)> {
    if text[pos..].starts_with('#') {
        let (num, end) = parse_digits(text, pos + 1)?;
        return Some((format!("{default_repo}#{num}"), end));
    }
    let (owner, after_owner) = parse_repo_part(text, pos)?;
    if !text[after_owner..].starts_with('/') {
        return None;
    }
    let (repo, after_repo) = parse_repo_part(text, after_owner + 1)?;
    if !text[after_repo..].starts_with('#') {
        return None;
    }
    let (num, end) = parse_digits(text, after_repo + 1)?;
    Some((format!("{owner}/{repo}#{num}"), end))
}

fn parse_repo_part(text: &str, mut pos: usize) -> Option<(String, usize)> {
    let start = pos;
    while let Some(ch) = text[pos..].chars().next() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            pos += ch.len_utf8();
        } else {
            break;
        }
    }
    if pos == start {
        None
    } else {
        Some((text[start..pos].to_string(), pos))
    }
}

fn parse_digits(text: &str, mut pos: usize) -> Option<(String, usize)> {
    let start = pos;
    while let Some(ch) = text[pos..].chars().next() {
        if ch.is_ascii_digit() {
            pos += ch.len_utf8();
        } else {
            break;
        }
    }
    if pos == start {
        None
    } else {
        Some((text[start..pos].to_string(), pos))
    }
}

#[derive(Debug)]
struct FoundCard {
    start: usize,
    end: usize,
    card: Map<String, Value>,
}

pub(crate) fn parse_card(body: &str) -> Option<Map<String, Value>> {
    find_card_block(body).map(|found| found.card)
}

fn find_card_block(body: &str) -> Option<FoundCard> {
    let mut offset = 0;
    while let Some(relative_start) = body[offset..].find(CARD_FENCE_START) {
        let start = offset + relative_start;
        let after_start = start + CARD_FENCE_START.len();
        let content_start = body[after_start..]
            .find('\n')
            .map(|newline| after_start + newline + 1)
            .unwrap_or(after_start);
        let relative_end = body[content_start..].find(CARD_FENCE_END)?;
        let end = content_start + relative_end;
        let content = body[content_start..end].trim();
        if let Ok(Value::Object(card)) = serde_json::from_str::<Value>(content) {
            if card.get("kind").and_then(Value::as_str) == Some(CARD_KIND) {
                return Some(FoundCard {
                    start,
                    end: end + CARD_FENCE_END.len(),
                    card,
                });
            }
        }
        offset = end + CARD_FENCE_END.len();
    }
    None
}

pub(crate) fn read_json(path: &Path) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn insert_nonempty(card: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        card.insert(key.to_string(), Value::String(value.to_string()));
    }
}

pub(crate) fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(crate) fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn nonempty_prefix(prefix: &str, value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        format!("{prefix}{value}")
    }
}

pub(crate) fn nonempty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect()
    }
}

pub(crate) fn json_obj<const N: usize>(items: [(&str, Value); N]) -> Value {
    let mut object = Map::new();
    for (key, value) in items {
        object.insert(key.to_string(), value);
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn body_builds_markdown_and_card_json() {
        let body = build_body(QueueCardInput {
            id: "airc#1".to_string(),
            branch: "feat/x".to_string(),
            owner: "codex".to_string(),
            status: "claimed".to_string(),
            blockers: String::new(),
            environment: "mac".to_string(),
            evidence: "tests pass".to_string(),
            next_action: "merge".to_string(),
            last_heartbeat: "2026-05-19T00:00Z".to_string(),
        })
        .unwrap();

        let card = parse_card(&body).unwrap();
        assert_eq!(card["kind"], CARD_KIND);
        assert_eq!(card["id"], "airc#1");
        assert_eq!(card["env"], "mac");
        assert!(!card.contains_key("blockers"));
    }

    #[test]
    fn mutate_body_replaces_card_and_adds_log() {
        let body = "**x**\n\n```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"old\"}\n```\n";
        let updated =
            mutate_body(body, "set:owner=new\nclear:missing\n", "claimed", "now").unwrap();

        let card = parse_card(&updated).unwrap();
        assert_eq!(card["owner"], "new");
        assert!(updated.contains("## Status log"));
        assert!(updated.contains("- now — claimed"));
    }

    #[test]
    fn claim_fields_treats_unclaimed_as_empty_owner() {
        let body = "```json\n{\"kind\":\"airc-queue-card-v1\",\"owner\":\"unclaimed\",\"status\":\"claimed\"}\n```";
        let card = parse_card(body).unwrap();
        assert_eq!(string_field(&Value::Object(card), "status"), "claimed");
    }

    #[test]
    fn dispatch_message_uses_top_candidate() {
        let raw = json!({
            "candidates": [{
                "number": 7,
                "title": "Fix the queue",
                "url": "https://example",
                "claim_command": "airc queue claim",
                "lane_command": "airc lane"
            }]
        })
        .to_string();

        let text = dispatch_message("claude", "now", &raw).unwrap();

        assert!(text.contains("@claude"));
        assert!(text.contains("#7"));
        assert!(text.contains("claim: airc queue claim"));
        assert!(text.contains("note: now"));
    }

    #[test]
    fn adopt_body_refuses_existing_card_without_force() {
        let issue = json!({"body":"```json\n{\"kind\":\"airc-queue-card-v1\"}\n```"});

        assert!(adopt_body(&issue, "new", false).is_err());
        assert!(adopt_body(&issue, "new", true)
            .unwrap()
            .contains("Pre-adoption body"));
    }

    #[test]
    fn nudge_summary_renders_compact_cards() {
        let issues = json!([{
            "number": 2,
            "title": "airc-queue: Build thing",
            "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"review\",\"owner\":\"codex\",\"branch\":\"feat/x\"}\n```"
        }]);

        assert_eq!(
            nudge_summary(&issues).unwrap(),
            "#2 review owner=codex branch=feat/x 'Build thing'"
        );
    }

    #[test]
    fn nudge_card_meta_requires_queue_card() {
        let issue = json!({
            "title": "A",
            "body": "```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"blocked\",\"owner\":\"codex\",\"branch\":\"b\"}\n```"
        });

        assert_eq!(
            nudge_card_meta(&issue).unwrap(),
            (
                "A".to_string(),
                "blocked".to_string(),
                "codex".to_string(),
                "b".to_string()
            )
        );
    }

    #[test]
    fn close_merged_refs_require_closing_keyword() {
        let refs = close_merged_refs(
            "feat(#576): document queue cards\nBody has unrelated #561.",
            "CambrianTech/airc",
        );

        assert!(refs.is_empty());
    }

    #[test]
    fn close_merged_refs_accept_same_repo_cross_repo_and_continuations() {
        let refs = close_merged_refs(
            "Closes #100, CambrianTech/continuum#1130 and #102.",
            "CambrianTech/airc",
        );

        assert_eq!(
            refs,
            vec![
                "CambrianTech/airc#100",
                "CambrianTech/continuum#1130",
                "CambrianTech/airc#102",
            ]
        );
    }

    #[test]
    fn close_merged_refs_dedupes_and_ignores_prose_after_keyword() {
        let refs = close_merged_refs(
            "Fix the queue docs. See #100.\nFixes #100. Also see #100.",
            "CambrianTech/airc",
        );

        assert_eq!(refs, vec!["CambrianTech/airc#100"]);
    }
}
