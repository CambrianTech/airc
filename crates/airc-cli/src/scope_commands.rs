use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub fn run_repair_config(
    home: &Path,
    config: &Path,
    default_name: &str,
    host: &str,
) -> Result<(), Box<dyn Error>> {
    if !home.exists() || !has_scope_state(home) {
        return Err("scope has no durable state to repair".into());
    }
    let existing = read_json_object(config).unwrap_or_default();
    let repaired = infer_config(home, default_name, host, &existing);
    if repaired == existing && config.exists() {
        return Ok(());
    }
    write_json(config, &repaired)?;
    if existing.is_empty() {
        println!("repaired missing config: {}", config.display());
    } else {
        println!("repaired incomplete config: {}", config.display());
    }
    Ok(())
}

fn infer_config(
    home: &Path,
    default_name: &str,
    host: &str,
    existing: &Map<String, Value>,
) -> Map<String, Value> {
    let room_name = read_trimmed(home.join("room_name"));
    let room_gist = read_trimmed(home.join("room_gist_id"));
    let host_gist = read_trimmed(home.join("host_gist_id"));
    let parted = string_set(existing.get("parted_rooms"));
    let mut channels = string_vec(existing.get("subscribed_channels"));
    for channel in bearer_state_channels(home) {
        if !channels.contains(&channel) && !parted.contains(&channel) {
            channels.push(channel);
        }
    }
    if !room_name.is_empty() && !channels.contains(&room_name) {
        channels.push(room_name.clone());
    }
    promote_channel(&mut channels, "cambriantech");
    if channels.first().is_none_or(|value| value != "cambriantech") && !room_name.is_empty() {
        promote_channel(&mut channels, &room_name);
    }

    let mut channel_gists = object_string_map(existing.get("channel_gists"));
    if !room_name.is_empty()
        && is_gist_id(&room_gist)
        && gone_channel_gist(home, &room_name) != room_gist
    {
        channel_gists.insert(room_name.clone(), room_gist);
    }
    for channel in &channels {
        let gone = gone_channel_gist(home, channel);
        if !gone.is_empty() {
            if channel_gists.get(channel) == Some(&gone) {
                channel_gists.remove(channel);
            }
            continue;
        }
        let log_gist = gist_from_bearer_log(home, channel);
        if is_gist_id(&log_gist) {
            channel_gists.insert(channel.clone(), log_gist);
        } else if !channel_gists.contains_key(channel)
            && channel != "general"
            && is_gist_id(&host_gist)
        {
            channel_gists.insert(channel.clone(), host_gist.clone());
        }
    }

    let mut data = existing.clone();
    data.entry("created".to_string()).or_insert_with(|| {
        Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    });
    data.entry("host".to_string())
        .or_insert_with(|| Value::String(host.to_string()));
    data.entry("name".to_string()).or_insert_with(|| {
        Value::String(
            name_from_messages(home)
                .or_else(|| name_from_ssh_comment(home))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    if default_name.is_empty() {
                        "airc".to_string()
                    } else {
                        default_name.to_string()
                    }
                }),
        )
    });
    if !channels.is_empty() {
        data.insert(
            "subscribed_channels".to_string(),
            Value::Array(channels.into_iter().map(Value::String).collect()),
        );
    }
    if !channel_gists.is_empty() {
        data.insert(
            "channel_gists".to_string(),
            Value::Object(
                channel_gists
                    .into_iter()
                    .map(|(key, value)| (key, Value::String(value)))
                    .collect(),
            ),
        );
    }
    data
}

fn has_scope_state(home: &Path) -> bool {
    ["identity", "messages.jsonl", "room_gist_id", "host_gist_id"]
        .iter()
        .any(|name| home.join(name).exists())
        || bearer_state_channels(home).next().is_some()
}

fn bearer_state_channels(home: &Path) -> impl Iterator<Item = String> {
    fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.strip_prefix("bearer_state.")
                .and_then(|value| value.strip_suffix(".json"))
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn gist_from_bearer_log(home: &Path, channel: &str) -> String {
    let raw =
        fs::read_to_string(home.join(format!("bearer_recv.{channel}.log"))).unwrap_or_default();
    raw.lines()
        .rev()
        .take(200)
        .find_map(first_gist_after_gh_get)
        .unwrap_or_default()
}

fn first_gist_after_gh_get(line: &str) -> Option<String> {
    let start = line.find("_gh_api_get(")? + "_gh_api_get(".len();
    let end = line[start..].find(')')? + start;
    let value = &line[start..end];
    is_gist_id(value).then(|| value.to_string())
}

fn gone_channel_gist(home: &Path, channel: &str) -> String {
    let value = read_trimmed(home.join(format!("gone_channel_gist.{channel}")));
    if is_gist_id(&value) {
        value
    } else {
        String::new()
    }
}

fn name_from_messages(home: &Path) -> Option<String> {
    let raw = fs::read_to_string(home.join("messages.jsonl")).ok()?;
    let mut counts = BTreeMap::<String, usize>::new();
    for line in raw.lines().rev().take(500) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(sender) = value.get("from").and_then(Value::as_str) else {
            continue;
        };
        if !sender.is_empty() && !matches!(sender, "airc" | "unknown") {
            *counts.entry(sender.to_string()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(sender, _)| sender)
}

fn name_from_ssh_comment(home: &Path) -> Option<String> {
    let raw = fs::read_to_string(home.join("identity").join("ssh_key.pub")).ok()?;
    let comment = raw.split_whitespace().nth(2)?;
    comment.starts_with("airc-").then(|| comment.to_string())
}

fn promote_channel(channels: &mut Vec<String>, target: &str) {
    if let Some(index) = channels.iter().position(|value| value == target) {
        let value = channels.remove(index);
        channels.insert(0, value);
    }
}

fn is_gist_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_trimmed(path: PathBuf) -> String {
    fs::read_to_string(path)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    let Value::Object(object) = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()? else {
        return None;
    };
    Some(object)
}

fn write_json(path: &Path, data: &Map<String, Value>) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(data)? + "\n")?;
    Ok(())
}

fn string_vec(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn string_set(value: Option<&Value>) -> BTreeSet<String> {
    string_vec(value).into_iter().collect()
}

fn object_string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_missing_config_from_scope_state() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::create_dir(home.join("identity")).unwrap();
        fs::write(
            home.join("identity").join("ssh_key.pub"),
            "ssh-ed25519 AAAA airc-fallback\n",
        )
        .unwrap();
        fs::write(home.join("room_name"), "general\n").unwrap();
        fs::write(
            home.join("room_gist_id"),
            "c68640ec0144b422c16b2d8c83ad5ee5\n",
        )
        .unwrap();
        fs::write(home.join("bearer_state.cambriantech.json"), "{}\n").unwrap();
        fs::write(home.join("bearer_state.general.json"), "{}\n").unwrap();
        fs::write(
            home.join("bearer_recv.cambriantech.log"),
            "_gh_api_get(df40c8ae6c90f8e14009426fd6e16e22): ok\n",
        )
        .unwrap();
        fs::write(
            home.join("messages.jsonl"),
            r#"{"from":"airc-8a5e","msg":"local"}"#,
        )
        .unwrap();

        run_repair_config(home, &home.join("config.json"), "default", "127.0.0.1").unwrap();
        let repaired = read_json_object(&home.join("config.json")).unwrap();

        assert_eq!(repaired["name"], "airc-8a5e");
        assert_eq!(repaired["host"], "127.0.0.1");
        assert_eq!(repaired["subscribed_channels"][0], "cambriantech");
        assert_eq!(
            repaired["channel_gists"]["cambriantech"],
            "df40c8ae6c90f8e14009426fd6e16e22"
        );
    }
}
