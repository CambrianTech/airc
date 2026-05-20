use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;

use serde_json::Value;

use crate::queue_card_commands::{parse_card, read_json, string_field};

pub fn run_review_refs(repo: &str, raw_json_file: &Path) -> Result<(), Box<dyn Error>> {
    let issues = read_json(raw_json_file)?;
    let issues = issues
        .as_array()
        .ok_or("queue staleness refs: issue JSON must be an array")?;
    for issue in issues {
        let Some(card) = issue
            .get("body")
            .and_then(Value::as_str)
            .and_then(parse_card)
        else {
            continue;
        };
        let card_value = Value::Object(card);
        if string_field(&card_value, "status") != "review" {
            continue;
        }
        let text = [
            string_field(issue, "title"),
            string_field(issue, "body"),
            string_field(&card_value, "next_action"),
            string_field(&card_value, "evidence"),
        ]
        .join("\n");
        if let Some(reference) = first_ref(repo, &text) {
            println!("{reference}");
        }
    }
    Ok(())
}

pub fn run_pr_meta(pr_file: &Path) -> Result<(), Box<dyn Error>> {
    let pr = read_json(pr_file)?;
    println!(
        "{}\t{}\t{}",
        string_field(&pr, "baseRefName"),
        string_field(&pr, "headRefName"),
        string_field(&pr, "url")
    );
    Ok(())
}

fn first_ref(repo: &str, text: &str) -> Option<String> {
    extract_refs(repo, text).into_iter().next()
}

fn extract_refs(repo: &str, text: &str) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for raw in text.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']')
    }) {
        let token = raw.trim_matches(|ch: char| matches!(ch, ',' | '.' | ':' | ';' | '!' | '?'));
        if token.is_empty() {
            continue;
        }
        if let Some(reference) = github_url_ref(token) {
            refs.insert(reference);
            continue;
        }
        if let Some(reference) = owner_repo_ref(token) {
            refs.insert(reference);
            continue;
        }
        if let Some(number) = local_issue_ref(token) {
            refs.insert(format!("{repo}#{number}"));
        }
    }
    refs
}

fn github_url_ref(token: &str) -> Option<String> {
    let path = token.strip_prefix("https://github.com/")?;
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() < 4 || !matches!(parts[2], "pull" | "pulls") || !is_digits(parts[3]) {
        return None;
    }
    Some(format!("{}/{}#{}", parts[0], parts[1], parts[3]))
}

fn owner_repo_ref(token: &str) -> Option<String> {
    let (repo, number) = token.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || !is_digits(number) {
        return None;
    }
    if !owner.chars().all(is_repo_char) || !name.chars().all(is_repo_char) {
        return None;
    }
    Some(format!("{owner}/{name}#{number}"))
}

fn local_issue_ref(token: &str) -> Option<&str> {
    let number = token.strip_prefix('#')?;
    is_digits(number).then_some(number)
}

fn is_repo_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn refs_extract_github_url_repo_ref_and_local_ref() {
        let refs = extract_refs(
            "CambrianTech/airc",
            "see https://github.com/CambrianTech/airc/pull/755, other/repo#12 and #9",
        );

        assert!(refs.contains("CambrianTech/airc#755"));
        assert!(refs.contains("other/repo#12"));
        assert!(refs.contains("CambrianTech/airc#9"));
    }

    #[test]
    fn review_refs_only_prints_review_cards() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("issues.json");
        fs::write(
            &file,
            json!([
                {"title":"A #1","body":"```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"claimed\"}\n```"},
                {"title":"B #2","body":"```json\n{\"kind\":\"airc-queue-card-v1\",\"status\":\"review\"}\n```"}
            ])
            .to_string(),
        )
        .unwrap();

        let issues = read_json(&file).unwrap();
        let refs = issues
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|issue| {
                let card = issue
                    .get("body")
                    .and_then(Value::as_str)
                    .and_then(parse_card)?;
                let card_value = Value::Object(card);
                (string_field(&card_value, "status") == "review")
                    .then(|| first_ref("CambrianTech/airc", &string_field(issue, "title")))
                    .flatten()
            })
            .collect::<Vec<_>>();

        assert_eq!(refs, vec!["CambrianTech/airc#2"]);
    }
}
