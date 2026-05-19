use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub(crate) struct Scope {
    pub(crate) peers_dir: PathBuf,
    pub(crate) local_log: PathBuf,
    pub(crate) offset_path: PathBuf,
    scope_dir: PathBuf,
    config_path: PathBuf,
    my_name: String,
}

impl Scope {
    pub(crate) fn new(peers_dir: &Path, my_name: &str) -> Self {
        let scope_dir = peers_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            peers_dir: peers_dir.to_path_buf(),
            config_path: scope_dir.join("config.json"),
            local_log: scope_dir.join("messages.jsonl"),
            offset_path: scope_dir.join("monitor_offset"),
            scope_dir,
            my_name: my_name.to_string(),
        }
    }

    pub(crate) fn identity_dir(&self) -> PathBuf {
        self.scope_dir.join("identity")
    }

    pub(crate) fn is_joiner(&self) -> bool {
        load_json(&self.config_path)
            .and_then(|value| {
                value
                    .get("host_target")
                    .and_then(Value::as_str)
                    .map(|value| !value.is_empty())
            })
            .unwrap_or(false)
    }

    pub(crate) fn room_name(&self) -> String {
        fs::read_to_string(self.scope_dir.join("room_name"))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "1:1".to_string())
    }

    pub(crate) fn subscribed_channels(&self) -> Option<BTreeSet<String>> {
        let values = load_json(&self.config_path)?
            .get("subscribed_channels")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    }

    pub(crate) fn current_name(&self) -> String {
        load_json(&self.config_path)
            .and_then(|value| {
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.my_name.clone())
    }
}

pub(crate) fn load_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}
