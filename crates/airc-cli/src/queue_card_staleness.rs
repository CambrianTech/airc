use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value};

use crate::queue_card_commands::{json_obj, parse_card, read_json, string_field};

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

pub struct StalenessAnalyzeInput<'a> {
    pub repo_root: &'a Path,
    pub pr_repo: &'a str,
    pub pr_num: &'a str,
    pub base_ref: &'a str,
    pub head_ref: &'a str,
    pub base_git_ref: &'a str,
    pub head_git_ref: &'a str,
    pub merge_base: &'a str,
    pub pr_url: &'a str,
    pub limit: usize,
    pub output_json: bool,
    pub files_file: &'a Path,
    pub diff_file: &'a Path,
    pub base_new_file: &'a Path,
}

pub fn run_staleness_analyze(input: StalenessAnalyzeInput<'_>) -> Result<(), Box<dyn Error>> {
    let touched_files = read_nonempty_lines(input.files_file)?;
    let diff_lines = read_lines_lossy(input.diff_file)?;
    let base_new_lines = read_lines_lossy(input.base_new_file)?;
    let base_added = plus_lines_by_file(&base_new_lines);
    let warnings = stale_warnings(
        input.repo_root,
        input.base_git_ref,
        &diff_lines,
        &base_added,
        input.limit,
    );
    let payload = json_obj([
        ("repo", Value::String(input.pr_repo.to_string())),
        ("pr", Value::String(input.pr_num.to_string())),
        ("url", Value::String(input.pr_url.to_string())),
        ("base", Value::String(input.base_ref.to_string())),
        ("head", Value::String(input.head_ref.to_string())),
        (
            "base_git_ref",
            Value::String(input.base_git_ref.to_string()),
        ),
        (
            "head_git_ref",
            Value::String(input.head_git_ref.to_string()),
        ),
        ("merge_base", Value::String(input.merge_base.to_string())),
        (
            "touched_files",
            Value::Array(touched_files.iter().cloned().map(Value::String).collect()),
        ),
        (
            "warning_count",
            Value::Number((warnings.len() as u64).into()),
        ),
        (
            "warnings",
            Value::Array(warnings.iter().map(StaleWarning::json).collect()),
        ),
    ]);
    if input.output_json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else if warnings.is_empty() {
        let label = if !input.pr_repo.is_empty() && !input.pr_num.is_empty() {
            format!("{}#{}", input.pr_repo, input.pr_num)
        } else {
            input.head_ref.to_string()
        };
        println!("OK: no stale conflicts detected for {label}.");
        println!(
            "base={} head={} files_touched={}",
            input.base_ref,
            input.head_ref,
            touched_files.len()
        );
    } else {
        let label = if !input.pr_repo.is_empty() && !input.pr_num.is_empty() {
            format!("{}#{}", input.pr_repo, input.pr_num)
        } else {
            input.head_ref.to_string()
        };
        println!("WARN: {label} branch may erase current-base work.");
        println!(
            "base={} head={} files_touched={} missing_base_lines_sample={}",
            input.base_ref,
            input.head_ref,
            touched_files.len(),
            warnings.len()
        );
        println!(
            "Rebase the PR branch onto the current base before merge, then rerun this command."
        );
        for warning in &warnings {
            let origin = if warning.origin.is_empty() {
                String::new()
            } else {
                format!(" ({})", warning.origin)
            };
            println!("  - {}: {}{}", warning.file, warning.line, origin);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaleWarning {
    file: String,
    line: String,
    origin: String,
}

impl StaleWarning {
    fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("file".to_string(), Value::String(self.file.clone()));
        object.insert("line".to_string(), Value::String(self.line.clone()));
        object.insert("origin".to_string(), Value::String(self.origin.clone()));
        Value::Object(object)
    }
}

fn read_nonempty_lines(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn read_lines_lossy(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(String::from_utf8_lossy(&fs::read(path)?)
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}

fn plus_lines_by_file(lines: &[String]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current = String::new();
    for raw in lines {
        if let Some(file) = raw.strip_prefix("+++ b/") {
            current = file.to_string();
            continue;
        }
        if !raw.starts_with('+') || raw.starts_with("+++") {
            continue;
        }
        let content = raw[1..].to_string();
        if content.trim().is_empty() {
            continue;
        }
        out.entry(current.clone()).or_default().insert(content);
    }
    out
}

fn stale_warnings(
    repo_root: &Path,
    base_git_ref: &str,
    diff_lines: &[String],
    base_added: &BTreeMap<String, BTreeSet<String>>,
    limit: usize,
) -> Vec<StaleWarning> {
    let mut warnings = Vec::new();
    let mut current_file = String::new();
    for raw in diff_lines {
        if let Some(file) = raw.strip_prefix("+++ b/") {
            current_file = file.to_string();
            continue;
        }
        if !raw.starts_with('+') || raw.starts_with("+++") {
            continue;
        }
        let content = &raw[1..];
        if content.trim().is_empty() {
            continue;
        }
        if !base_added
            .get(&current_file)
            .map(|lines| lines.contains(content))
            .unwrap_or(false)
        {
            continue;
        }
        let line = truncate_chars(content, 240);
        let origin = git_origin(repo_root, base_git_ref, content, &current_file);
        warnings.push(StaleWarning {
            file: current_file.clone(),
            line,
            origin,
        });
        if warnings.len() >= limit {
            break;
        }
    }
    warnings
}

fn git_origin(repo_root: &Path, base_git_ref: &str, content: &str, file: &str) -> String {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("log")
        .arg("--format=%h %s")
        .arg("-n")
        .arg("1")
        .arg("-S")
        .arg(content)
        .arg(base_git_ref)
        .arg("--")
        .arg(file)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|stdout| stdout.lines().next().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!("{}...", value.chars().take(max).collect::<String>())
    }
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

    #[test]
    fn plus_lines_by_file_tracks_nonempty_additions() {
        let lines = vec![
            "+++ b/src/lib.rs".to_string(),
            "+one".to_string(),
            "+".to_string(),
            " context".to_string(),
            "+++ b/src/main.rs".to_string(),
            "+two".to_string(),
        ];

        let by_file = plus_lines_by_file(&lines);

        assert!(by_file["src/lib.rs"].contains("one"));
        assert!(by_file["src/main.rs"].contains("two"));
        assert!(!by_file["src/lib.rs"].contains(""));
    }

    #[test]
    fn stale_warnings_match_base_added_lines() {
        let diff_lines = vec![
            "+++ b/src/lib.rs".to_string(),
            "+keep".to_string(),
            "+ignore".to_string(),
        ];
        let mut base_added = BTreeMap::new();
        base_added.insert(
            "src/lib.rs".to_string(),
            BTreeSet::from(["keep".to_string()]),
        );

        let warnings = stale_warnings(Path::new("."), "HEAD", &diff_lines, &base_added, 10);

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].file, "src/lib.rs");
        assert_eq!(warnings[0].line, "keep");
    }
}
