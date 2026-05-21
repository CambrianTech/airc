//! Default account-room join context.
//!
//! A fresh `airc join` should not mean "one current room." It means:
//! subscribe this scope to the account lobby (`#general`) and, when
//! running inside a Git checkout, the account/org room inferred from
//! the repository remote.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::subscriptions::ChannelName;

pub const GENERAL_CHANNEL: &str = "general";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinContext {
    pub channels: Vec<ChannelName>,
    pub default: ChannelName,
}

impl JoinContext {
    pub fn from_cwd(cwd: &Path) -> Self {
        let mut names = BTreeSet::new();
        names.insert(ChannelName::general());
        if let Some(org) = infer_repo_owner_channel(cwd) {
            names.insert(org);
        }

        let default = names
            .iter()
            .find(|name| name.as_str() != GENERAL_CHANNEL)
            .cloned()
            .unwrap_or_else(ChannelName::general);
        Self {
            channels: names.into_iter().collect(),
            default,
        }
    }
}

fn infer_repo_owner_channel(cwd: &Path) -> Option<ChannelName> {
    let config = find_git_config(cwd)?;
    let remote = origin_remote_url(&std::fs::read_to_string(config).ok()?)?;
    let owner = remote_owner(&remote)?;
    ChannelName::new(owner).ok()
}

fn find_git_config(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let dotgit = dir.join(".git");
        if dotgit.is_dir() {
            return Some(dotgit.join("config"));
        }
        if dotgit.is_file() {
            let text = std::fs::read_to_string(&dotgit).ok()?;
            let gitdir = text.strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(gitdir).is_absolute() {
                PathBuf::from(gitdir)
            } else {
                dir.join(gitdir)
            };
            return Some(gitdir.join("config"));
        }
    }
    None
}

fn origin_remote_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_origin = line == r#"[remote "origin"]"#;
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "url" {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn remote_owner(url: &str) -> Option<&str> {
    let without_suffix = url.strip_suffix(".git").unwrap_or(url);
    if let Some(rest) = without_suffix.strip_prefix("git@") {
        let (_, path) = rest.split_once(':')?;
        return path.split('/').next();
    }
    if let Some((_, path)) = without_suffix.split_once("://") {
        let path = path.split_once('/')?.1;
        return path.split('/').next();
    }
    without_suffix.split('/').next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn context_without_git_is_general_only() {
        let dir = TempDir::new().unwrap();

        let context = JoinContext::from_cwd(dir.path());

        assert_eq!(names(&context), vec!["general"]);
        assert_eq!(context.default.as_str(), "general");
    }

    #[test]
    fn context_adds_github_owner_and_defaults_to_it() {
        let repo = repo_with_origin("https://github.com/CambrianTech/airc.git");

        let context = JoinContext::from_cwd(repo.path());

        assert_eq!(names(&context), vec!["cambriantech", "general"]);
        assert_eq!(context.default.as_str(), "cambriantech");
    }

    #[test]
    fn context_reads_ssh_origin() {
        let repo = repo_with_origin("git@github.com:UseIdeem/vHSM.git");

        let context = JoinContext::from_cwd(repo.path());

        assert_eq!(names(&context), vec!["general", "useideem"]);
        assert_eq!(context.default.as_str(), "useideem");
    }

    #[test]
    fn context_walks_up_from_nested_dir() {
        let repo = repo_with_origin("https://github.com/CambrianTech/continuum.git");
        let nested = repo.path().join("crates/continuum-core/src");
        std::fs::create_dir_all(&nested).unwrap();

        let context = JoinContext::from_cwd(&nested);

        assert_eq!(context.default.as_str(), "cambriantech");
    }

    #[test]
    fn context_reads_gitdir_file_for_worktrees() {
        let checkout = TempDir::new().unwrap();
        let gitdir = TempDir::new().unwrap();
        std::fs::write(
            checkout.path().join(".git"),
            format!("gitdir: {}\n", gitdir.path().display()),
        )
        .unwrap();
        std::fs::write(
            gitdir.path().join("config"),
            r#"[remote "origin"]
    url = ssh://git@github.com/OpenClaw/openclaw.git
"#,
        )
        .unwrap();

        let context = JoinContext::from_cwd(checkout.path());

        assert_eq!(names(&context), vec!["general", "openclaw"]);
        assert_eq!(context.default.as_str(), "openclaw");
    }

    fn repo_with_origin(url: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            format!(
                r#"[core]
    repositoryformatversion = 0
[remote "origin"]
    url = {url}
"#
            ),
        )
        .unwrap();
        dir
    }

    fn names(context: &JoinContext) -> Vec<&str> {
        context.channels.iter().map(ChannelName::as_str).collect()
    }
}
