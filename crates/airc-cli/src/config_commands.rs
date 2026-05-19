use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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

fn load_config(path: &Path) -> ConfigFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| ConfigFile {
            subscribed_channels: Vec::new(),
            channel_gists: std::collections::BTreeMap::new(),
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
}
