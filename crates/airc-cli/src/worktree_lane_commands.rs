use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct LaneRecord {
    ts: String,
    issue: String,
    repo: String,
    dir: String,
    branch: String,
    base: String,
    owner: String,
}

pub fn run_abs_path(path: &str) -> Result<(), Box<dyn Error>> {
    println!("{}", abs_path(path)?.display());
    Ok(())
}

pub fn run_slug(value: &str) {
    println!("{}", slug(value));
}

pub fn run_record(
    registry: &Path,
    issue: String,
    repo: String,
    dir: String,
    branch: String,
    base: String,
    owner: String,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = registry.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = LaneRecord {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        issue,
        repo,
        dir,
        branch,
        base,
        owner,
    };
    let mut line = serde_json::to_string(&record)?;
    line.push('\n');
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(registry)?
        .write_all(line.as_bytes())?;
    Ok(())
}

pub fn run_list(registry: &Path, as_json: bool) -> Result<(), Box<dyn Error>> {
    let lanes = read_records(registry);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "lanes": lanes }))?
        );
        return Ok(());
    }

    println!("# airc lanes");
    if lanes.is_empty() {
        println!("No recorded lanes yet.");
        return Ok(());
    }
    for lane in lanes {
        println!(
            "- {issue} owner={owner} branch={branch} dir={dir} base={base}",
            issue = lane.issue,
            owner = lane.owner,
            branch = lane.branch,
            dir = lane.dir,
            base = lane.base,
        );
    }
    Ok(())
}

pub fn run_find(registry: &Path, target: &str, field: &str) -> Result<(), Box<dyn Error>> {
    let Some(lane) = find_record(registry, target) else {
        return Err(format!("no recorded lane matches: {target}").into());
    };
    match field {
        "json" => println!("{}", serde_json::to_string(&lane)?),
        "repo" => println!("{}", lane.repo),
        "dir" => println!("{}", lane.dir),
        "issue" => println!("{}", lane.issue),
        "branch" => println!("{}", lane.branch),
        "base" => println!("{}", lane.base),
        "owner" => println!("{}", lane.owner),
        other => return Err(format!("unknown lane field: {other}").into()),
    }
    Ok(())
}

fn abs_path(path: &str) -> Result<PathBuf, Box<dyn Error>> {
    let expanded = if path == "~" {
        home_dir().ok_or("HOME/USERPROFILE is not set for ~ expansion")?
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir()
            .ok_or("HOME/USERPROFILE is not set for ~/ expansion")?
            .join(rest)
    } else {
        PathBuf::from(path)
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(std::env::current_dir()?.join(expanded))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_was_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "lane".to_string()
    } else {
        trimmed.to_string()
    }
}

fn read_records(registry: &Path) -> Vec<LaneRecord> {
    let Ok(raw) = fs::read_to_string(registry) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<LaneRecord>(line).ok())
        .collect()
}

fn find_record(registry: &Path, target: &str) -> Option<LaneRecord> {
    read_records(registry).into_iter().rfind(|lane| {
        lane.issue == target
            || lane.dir == target
            || Path::new(&lane.dir)
                .file_name()
                .and_then(|value| value.to_str())
                == Some(target)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_legacy_shape() {
        assert_eq!(slug("#1234: Rust Lane!"), "1234-rust-lane");
        assert_eq!(slug(""), "lane");
        assert_eq!(slug("A_B.C-D"), "a_b.c-d");
    }

    #[test]
    fn record_list_and_find_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("lanes.jsonl");

        run_record(
            &registry,
            "#1".to_string(),
            "/repo".to_string(),
            "/tmp/lane-one".to_string(),
            "feat/one".to_string(),
            "origin/canary".to_string(),
            "codex".to_string(),
        )
        .unwrap();

        let found = find_record(&registry, "lane-one").unwrap();

        assert_eq!(found.issue, "#1");
        assert_eq!(found.repo, "/repo");
        assert_eq!(found.dir, "/tmp/lane-one");
        assert_eq!(found.branch, "feat/one");
    }
}
