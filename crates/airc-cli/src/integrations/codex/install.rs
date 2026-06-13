//! Codex hook installer command orchestration.

use std::path::PathBuf;

use super::{config, hooks_json};

#[derive(Debug, Default)]
pub struct HookInstallReport {
    pub lines: Vec<String>,
}

impl HookInstallReport {
    fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

pub async fn run_install_hooks(
    codex_home: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let codex_home = codex_home.unwrap_or_else(default_codex_home);
    let report = install_hooks_at(codex_home)?;
    for line in report.lines {
        println!("{line}");
    }
    Ok(())
}

pub fn install_hooks_at(
    codex_home: PathBuf,
) -> Result<HookInstallReport, Box<dyn std::error::Error>> {
    let config = codex_home.join("config.toml");
    let hooks_json = codex_home.join("hooks.json");
    let mut report = HookInstallReport::default();

    if config::enable_hooks_feature(&config)? {
        report.push(format!("enabled hooks in {}", config.display()));
    }
    if config::remove_stale_airc_filesystem_permissions(&config)? {
        report.push(format!(
            "removed stale AIRC filesystem permission profile from {}",
            config.display()
        ));
    }
    if hooks_json::install(&hooks_json)? {
        report.push(format!(
            "installed AIRC UserPromptSubmit hook in {}",
            hooks_json.display()
        ));
    }
    if config::upsert_managed_developer_instructions(&config)? {
        report.push(format!(
            "installed AIRC Codex turn contract in {}",
            config.display()
        ));
    }
    Ok(report)
}

pub fn install_hooks_for_default_home_if_present(
) -> Result<HookInstallReport, Box<dyn std::error::Error>> {
    let codex_home = default_codex_home();
    if !codex_home.exists() {
        return Ok(HookInstallReport::default());
    }
    install_hooks_at(codex_home)
}

pub async fn run_uninstall_hooks(
    codex_home: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let codex_home = codex_home.unwrap_or_else(default_codex_home);
    let config = codex_home.join("config.toml");
    let hooks_json = codex_home.join("hooks.json");

    if config::disable_managed_hooks_feature(&config)? {
        println!(
            "removed airc-managed hooks feature from {}",
            config.display()
        );
    }
    if hooks_json::uninstall(&hooks_json)? {
        println!(
            "removed AIRC UserPromptSubmit hook from {}",
            hooks_json.display()
        );
    }
    if config::remove_managed_developer_instructions(&config)? {
        println!("removed AIRC Codex turn contract from {}", config.display());
    }
    Ok(())
}

pub fn default_codex_home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".codex");
    }
    #[cfg(windows)]
    if let Some(userprofile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(userprofile).join(".codex");
    }
    PathBuf::from(".codex")
}
