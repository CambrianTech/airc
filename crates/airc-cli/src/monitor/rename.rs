use std::fs;
use std::path::Path;

use serde_json::Value;

use super::scope::load_json;

pub(crate) fn handle_rename(peers_dir: &Path, msg: &str) -> bool {
    let Some(rest) = msg.strip_prefix("[rename] ") else {
        return false;
    };
    let mut old = "";
    let mut new = "";
    let mut host = "";
    for part in rest.split_whitespace() {
        if let Some(value) = part.strip_prefix("old=") {
            old = value;
        } else if let Some(value) = part.strip_prefix("new=") {
            new = value;
        } else if let Some(value) = part.strip_prefix("host=") {
            host = value;
        }
    }
    if old.is_empty() || new.is_empty() {
        return false;
    }
    if rename_files(peers_dir, old, new) {
        println!("airc: nick {old} -> {new}");
        return true;
    }
    if !host.is_empty() {
        if let Some(current) = find_peer_by_host(peers_dir, host) {
            if current != new && rename_files(peers_dir, &current, new) {
                println!("airc: nick (chain-repair) {current} -> {new}");
                return true;
            }
        }
    }
    false
}

fn rename_files(peers_dir: &Path, old: &str, new: &str) -> bool {
    let old_json = peers_dir.join(format!("{old}.json"));
    let new_json = peers_dir.join(format!("{new}.json"));
    if !old_json.is_file() {
        return false;
    }
    if fs::rename(&old_json, &new_json).is_ok() {
        if let Some(mut value) = load_json(&new_json) {
            value["name"] = Value::String(new.to_string());
            if let Ok(raw) = serde_json::to_string_pretty(&value) {
                let _ = fs::write(&new_json, raw);
            }
        }
    }
    let old_pub = peers_dir.join(format!("{old}.pub"));
    if old_pub.is_file() {
        let _ = fs::rename(old_pub, peers_dir.join(format!("{new}.pub")));
    }
    true
}

fn find_peer_by_host(peers_dir: &Path, host: &str) -> Option<String> {
    let mut matches = Vec::new();
    let entries = fs::read_dir(peers_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(value) = load_json(&path) else {
            continue;
        };
        if value.get("host").and_then(Value::as_str) == Some(host) {
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| path.file_stem()?.to_str().map(ToOwned::to_owned));
            if let Some(name) = name {
                matches.push(name);
            }
        }
    }
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}
