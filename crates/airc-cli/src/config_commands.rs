use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    subscribed_channels: Vec<String>,
    #[serde(default)]
    channel_gists: std::collections::BTreeMap<String, String>,
}

pub fn run_read_channels(
    home: &Path,
    config: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let file = load_config(&config);
    for channel in file.subscribed_channels {
        println!("{channel}");
    }
    Ok(())
}

pub fn run_default_channel(
    home: &Path,
    config: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let file = load_config(&config);
    if let Some(channel) = file.subscribed_channels.first() {
        println!("{channel}");
    }
    Ok(())
}

pub fn run_get_channel_gist(
    home: &Path,
    config: Option<PathBuf>,
    channel: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let file = load_config(&config);
    if let Some(gist) = file
        .channel_gists
        .get(channel)
        .filter(|gist| !gist.is_empty())
    {
        println!("{gist}");
    }
    Ok(())
}

pub fn run_list_channel_gists(
    home: &Path,
    config: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let file = load_config(&config);
    for (channel, gist) in file
        .channel_gists
        .into_iter()
        .filter(|(channel, gist)| !channel.is_empty() && !gist.is_empty())
    {
        println!("{channel}\t{gist}");
    }
    Ok(())
}

pub fn run_subscribe(
    home: &Path,
    config: Option<PathBuf>,
    channel: &str,
    first: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    let channels = subscribed_channels_mut(&mut root);
    channels.retain(|value| value.as_str() != Some(channel));
    let value = Value::String(channel.to_string());
    if first {
        channels.insert(0, value);
    } else {
        channels.push(value);
    }
    save_value(&config, &root)
}

pub fn run_unsubscribe(
    home: &Path,
    config: Option<PathBuf>,
    channel: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    subscribed_channels_mut(&mut root).retain(|value| value.as_str() != Some(channel));
    save_value(&config, &root)
}

pub fn run_set_channel_gist(
    home: &Path,
    config: Option<PathBuf>,
    channel: &str,
    gist_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    let gists = channel_gists_mut(&mut root);
    if gist_id.is_empty() {
        gists.remove(channel);
    } else {
        gists.insert(channel.to_string(), Value::String(gist_id.to_string()));
    }
    save_value(&config, &root)
}

fn load_config(path: &Path) -> ConfigFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| ConfigFile {
            subscribed_channels: Vec::new(),
            channel_gists: std::collections::BTreeMap::new(),
        })
}

fn load_value(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| Value::Object(Map::new()))
}

fn save_value(path: &Path, value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(value)?;
    fs::write(path, raw)?;
    Ok(())
}

fn object_mut(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .expect("value was converted to object")
}

fn subscribed_channels_mut(value: &mut Value) -> &mut Vec<Value> {
    let object = object_mut(value);
    let entry = object
        .entry("subscribed_channels")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().expect("entry was converted to array")
}

fn channel_gists_mut(value: &mut Value) -> &mut Map<String, Value> {
    let object = object_mut(value);
    let entry = object
        .entry("channel_gists")
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    entry
        .as_object_mut()
        .expect("entry was converted to object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        let config = load_config(&dir.path().join("missing.json"));

        assert!(config.subscribed_channels.is_empty());
        assert!(config.channel_gists.is_empty());
    }

    #[test]
    fn read_channels_preserves_config_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"subscribed_channels":["general","airc","continuum"]}"#,
        )
        .unwrap();

        let config = load_config(&path);

        assert_eq!(config.subscribed_channels, ["general", "airc", "continuum"]);
    }

    #[test]
    fn reads_channel_gist_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"channel_gists":{"general":"gist-general","airc":"gist-airc"}}"#,
        )
        .unwrap();

        let config = load_config(&path);

        assert_eq!(config.channel_gists.get("general").unwrap(), "gist-general");
        assert_eq!(config.channel_gists.get("airc").unwrap(), "gist-airc");
    }

    #[test]
    fn subscribe_appends_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"subscribed_channels":["general"]}"#).unwrap();

        run_subscribe(dir.path(), Some(path.clone()), "airc", false).unwrap();
        run_subscribe(dir.path(), Some(path.clone()), "airc", false).unwrap();

        let config = load_config(&path);
        assert_eq!(config.subscribed_channels, ["general", "airc"]);
    }

    #[test]
    fn subscribe_first_promotes_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"subscribed_channels":["general","airc"]}"#).unwrap();

        run_subscribe(dir.path(), Some(path.clone()), "airc", true).unwrap();

        let config = load_config(&path);
        assert_eq!(config.subscribed_channels, ["airc", "general"]);
    }

    #[test]
    fn unsubscribe_removes_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"subscribed_channels":["general","airc"]}"#).unwrap();

        run_unsubscribe(dir.path(), Some(path.clone()), "airc").unwrap();

        let config = load_config(&path);
        assert_eq!(config.subscribed_channels, ["general"]);
    }

    #[test]
    fn set_channel_gist_sets_and_clears_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        run_set_channel_gist(dir.path(), Some(path.clone()), "general", "gist-general").unwrap();
        assert_eq!(
            load_config(&path).channel_gists.get("general").unwrap(),
            "gist-general"
        );

        run_set_channel_gist(dir.path(), Some(path.clone()), "general", "").unwrap();
        assert!(!load_config(&path).channel_gists.contains_key("general"));
    }
}
