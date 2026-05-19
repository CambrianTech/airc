use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    subscribed_channels: Vec<String>,
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

fn load_config(path: &Path) -> ConfigFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| ConfigFile {
            subscribed_channels: Vec::new(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        let config = load_config(&dir.path().join("missing.json"));

        assert!(config.subscribed_channels.is_empty());
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
}
