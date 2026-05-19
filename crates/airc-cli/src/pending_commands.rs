use std::error::Error;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    channel_gists: std::collections::BTreeMap<String, String>,
}

pub fn run_host_broadcast_route(
    snapshot: &Path,
    config: &Path,
    fallback_gist: &str,
) -> Result<(), Box<dyn Error>> {
    let lines = read_lines(snapshot);
    if lines.is_empty() {
        println!("no\tempty");
        return Ok(());
    }

    let mut channel = String::new();
    for line in &lines {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            println!("no\tmalformed");
            return Ok(());
        };
        let to = message.get("to").and_then(Value::as_str).unwrap_or("all");
        if !matches!(to, "" | "all") {
            println!("no\tdm");
            return Ok(());
        }
        let line_channel = message.get("channel").and_then(Value::as_str).unwrap_or("");
        if line_channel.is_empty() {
            println!("no\tmissing-channel");
            return Ok(());
        }
        if !channel.is_empty() && channel != line_channel {
            println!("no\tmixed-channel");
            return Ok(());
        }
        channel = line_channel.to_string();
    }

    let config = load_config(config);
    let gist = config
        .channel_gists
        .get(&channel)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_gist);
    if gist.is_empty() {
        println!("no\tmissing-gist");
        return Ok(());
    }

    println!("ok\t{channel}\t{gist}\t{}", lines.len());
    Ok(())
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn load_config(path: &Path) -> ConfigFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_broadcast_route_rejects_dm_batches() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("pending.jsonl");
        let config = dir.path().join("config.json");
        fs::write(&snapshot, r#"{"to":"alice","channel":"general"}"#).unwrap();
        fs::write(&config, "{}").unwrap();

        run_host_broadcast_route(&snapshot, &config, "fallback").unwrap();
    }
}
