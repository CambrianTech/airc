//! Codex hook installer command orchestration.

use std::path::PathBuf;

use crate::{codex_config, codex_hooks_json};

pub async fn run_install_hooks(
    codex_home: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let codex_home = codex_home.unwrap_or_else(default_codex_home);
    let config = codex_home.join("config.toml");
    let hooks_json = codex_home.join("hooks.json");

    if codex_config::enable_hooks_feature(&config)? {
        println!("enabled hooks in {}", config.display());
    }
    if codex_config::remove_stale_airc_filesystem_permissions(&config)? {
        println!(
            "removed stale AIRC filesystem permission profile from {}",
            config.display()
        );
    }
    if codex_hooks_json::install(&hooks_json)? {
        println!(
            "installed AIRC UserPromptSubmit hook in {}",
            hooks_json.display()
        );
    }
    if codex_config::remove_managed_developer_instructions(&config)? {
        println!(
            "removed legacy AIRC Codex polling instructions from {}",
            config.display()
        );
    }
    Ok(())
}

pub async fn run_uninstall_hooks(
    codex_home: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let codex_home = codex_home.unwrap_or_else(default_codex_home);
    let config = codex_home.join("config.toml");
    let hooks_json = codex_home.join("hooks.json");

    if codex_config::disable_managed_hooks_feature(&config)? {
        println!(
            "removed airc-managed hooks feature from {}",
            config.display()
        );
    }
    if codex_hooks_json::uninstall(&hooks_json)? {
        println!(
            "removed AIRC UserPromptSubmit hook from {}",
            hooks_json.display()
        );
    }
    Ok(())
}

fn default_codex_home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".codex");
    }
    #[cfg(windows)]
    if let Some(userprofile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(userprofile).join(".codex");
    }
    PathBuf::from(".codex")
}
