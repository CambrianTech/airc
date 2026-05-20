use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::json_path::{emit_value, navigate};

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    parted_rooms: Vec<String>,
    #[serde(default)]
    subscribed_channels: Vec<String>,
    #[serde(default)]
    channel_gists: std::collections::BTreeMap<String, String>,
}

pub fn run_get(
    home: &Path,
    config: Option<PathBuf>,
    key: &str,
    default: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    println!("{}", config_value(&config, key, default));
    Ok(())
}

pub fn run_get_path(
    home: &Path,
    config: Option<PathBuf>,
    path: &str,
    default: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let root = load_value(&config);
    let value = navigate(&root, path).unwrap_or(&Value::Null);
    emit_value(value, default)
}

pub fn run_has_key(
    home: &Path,
    config: Option<PathBuf>,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    println!("{}", config_has_key(&config, key));
    Ok(())
}

pub fn run_get_name(
    home: &Path,
    config: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    println!("{}", config_value(&config, "name", "unknown"));
    Ok(())
}

pub fn run_set(
    home: &Path,
    config: Option<PathBuf>,
    key: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    object_mut(&mut root).insert(key.to_string(), Value::String(value.to_string()));
    save_value(&config, &root)
}

pub fn run_set_name(
    home: &Path,
    config: Option<PathBuf>,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    run_set(home, config, "name", name)
}

pub fn run_unset_keys(
    home: &Path,
    config: Option<PathBuf>,
    keys: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    let object = object_mut(&mut root);
    for key in keys {
        object.remove(key);
    }
    save_value(&config, &root)
}

pub fn run_read_parted(
    home: &Path,
    config: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let file = load_config(&config);
    for room in file.parted_rooms {
        println!("{room}");
    }
    Ok(())
}

pub fn run_record_parted(
    home: &Path,
    config: Option<PathBuf>,
    room: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    let rooms = parted_rooms_mut(&mut root);
    if !rooms.iter().any(|value| value.as_str() == Some(room)) {
        rooms.push(Value::String(room.to_string()));
    }
    save_value(&config, &root)
}

pub fn run_clear_parted(
    home: &Path,
    config: Option<PathBuf>,
    room: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let mut root = load_value(&config);
    parted_rooms_mut(&mut root).retain(|value| value.as_str() != Some(room));
    save_value(&config, &root)
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

#[derive(Debug)]
pub struct HostBlockUpdate {
    pub host_airc_home: String,
    pub host_name: String,
    pub host_port: String,
    pub host_ssh_pub: String,
    pub host_identity_json: String,
}

impl HostBlockUpdate {
    fn parsed_port(&self) -> u16 {
        self.host_port.parse().unwrap_or(7547)
    }

    fn parsed_identity(&self) -> Value {
        serde_json::from_str(&self.host_identity_json).unwrap_or_else(|_| Value::Object(Map::new()))
    }
}

pub fn run_set_host_block(
    home: &Path,
    config: Option<PathBuf>,
    update: HostBlockUpdate,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let host_port = update.parsed_port();
    let host_identity = update.parsed_identity();
    let mut root = load_value(&config);
    let object = object_mut(&mut root);
    object.insert(
        "host_airc_home".to_string(),
        Value::String(update.host_airc_home),
    );
    object.insert("host_name".to_string(), Value::String(update.host_name));
    object.insert("host_port".to_string(), Value::Number(host_port.into()));
    object.insert(
        "host_ssh_pub".to_string(),
        Value::String(update.host_ssh_pub),
    );
    object.insert("host_identity".to_string(), host_identity);
    save_value(&config, &root)
}

fn load_config(path: &Path) -> ConfigFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| ConfigFile {
            parted_rooms: Vec::new(),
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

fn parted_rooms_mut(value: &mut Value) -> &mut Vec<Value> {
    let object = object_mut(value);
    let entry = object
        .entry("parted_rooms")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().expect("entry was converted to array")
}

fn config_value(path: &Path, key: &str, default: &str) -> String {
    let root = load_value(path);
    match root.get(key) {
        Some(Value::Null) | None => default.to_string(),
        Some(Value::String(value)) if value.is_empty() => default.to_string(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => {
            serde_json::to_string(root.get(key).expect("matched value exists"))
                .unwrap_or_else(|_| default.to_string())
        }
    }
}

fn config_has_key(path: &Path, key: &str) -> bool {
    load_value(path)
        .as_object()
        .map(|object| object.contains_key(key))
        .unwrap_or(false)
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
        assert!(config.parted_rooms.is_empty());
    }

    #[test]
    fn get_returns_default_for_missing_or_empty_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"empty":"","name":"alice"}"#).unwrap();

        assert_eq!(config_value(&path, "name", "unknown"), "alice");
        assert_eq!(config_value(&path, "empty", "fallback"), "fallback");
        assert_eq!(config_value(&path, "missing", "fallback"), "fallback");
    }

    #[test]
    fn get_serializes_structured_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"identity":{"role":"agent"},"rooms":["general"]}"#,
        )
        .unwrap();

        assert_eq!(config_value(&path, "identity", "{}"), r#"{"role":"agent"}"#);
        assert_eq!(config_value(&path, "rooms", "[]"), r#"["general"]"#);
    }

    #[test]
    fn get_path_reads_nested_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"identity":{"role":"agent","enabled":true},"rooms":["general"]}"#,
        )
        .unwrap();

        let root = load_value(&path);
        assert_eq!(
            navigate(&root, ".identity.role").and_then(Value::as_str),
            Some("agent")
        );
        assert_eq!(
            navigate(&root, ".identity.enabled").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            navigate(&root, ".rooms[0]").and_then(Value::as_str),
            Some("general")
        );
    }

    #[test]
    fn has_key_reports_top_level_presence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"host_target":"","identity":{"role":"agent"}}"#).unwrap();

        assert!(config_has_key(&path, "host_target"));
        assert!(config_has_key(&path, "identity"));
        assert!(!config_has_key(&path, "missing"));
        assert!(!config_has_key(
            &dir.path().join("missing.json"),
            "host_target"
        ));
    }

    #[test]
    fn set_and_unset_preserve_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"name":"alice","host":"localhost"}"#).unwrap();

        run_set(dir.path(), Some(path.clone()), "name", "bob").unwrap();
        run_unset_keys(dir.path(), Some(path.clone()), &["host".to_string()]).unwrap();

        assert_eq!(config_value(&path, "name", ""), "bob");
        assert_eq!(config_value(&path, "host", "missing"), "missing");
    }

    #[test]
    fn parted_rooms_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        run_record_parted(dir.path(), Some(path.clone()), "general").unwrap();
        run_record_parted(dir.path(), Some(path.clone()), "general").unwrap();
        assert_eq!(load_config(&path).parted_rooms, ["general"]);

        run_clear_parted(dir.path(), Some(path.clone()), "general").unwrap();
        assert!(load_config(&path).parted_rooms.is_empty());
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

    #[test]
    fn set_host_block_writes_typed_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"name":"joiner"}"#).unwrap();

        run_set_host_block(
            dir.path(),
            Some(path.clone()),
            HostBlockUpdate {
                host_airc_home: "/tmp/airc".to_string(),
                host_name: "host".to_string(),
                host_port: "7550".to_string(),
                host_ssh_pub: "ssh-ed25519 AAA".to_string(),
                host_identity_json: r#"{"role":"host"}"#.to_string(),
            },
        )
        .unwrap();

        assert_eq!(config_value(&path, "name", ""), "joiner");
        assert_eq!(config_value(&path, "host_airc_home", ""), "/tmp/airc");
        assert_eq!(config_value(&path, "host_name", ""), "host");
        assert_eq!(config_value(&path, "host_port", ""), "7550");
        assert_eq!(
            config_value(&path, "host_identity", "{}"),
            r#"{"role":"host"}"#
        );
    }

    #[test]
    fn set_host_block_defaults_bad_port_and_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        run_set_host_block(
            dir.path(),
            Some(path.clone()),
            HostBlockUpdate {
                host_airc_home: String::new(),
                host_name: String::new(),
                host_port: "not-a-port".to_string(),
                host_ssh_pub: String::new(),
                host_identity_json: "not-json".to_string(),
            },
        )
        .unwrap();

        assert_eq!(config_value(&path, "host_port", ""), "7547");
        assert_eq!(config_value(&path, "host_identity", ""), "{}");
    }
}
