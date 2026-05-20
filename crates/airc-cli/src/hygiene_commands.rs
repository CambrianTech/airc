use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::hygiene_cli::{HygieneAction, HygieneArgs};

const DEFAULT_POLICY_FILE: &str = ".airc-policy.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct HygienePolicy {
    workspace_root: String,
    report_paths: Vec<String>,
    hooks: Vec<String>,
    warn_free_gb: f64,
    block_free_gb: f64,
    clean_worktree_rust_targets: bool,
    clean_worktree_node_modules: bool,
    clean_main_rust_target: bool,
    clean_docker_build_cache: bool,
}

impl Default for HygienePolicy {
    fn default() -> Self {
        Self {
            workspace_root: "~/.airc-worktrees".to_string(),
            report_paths: Vec::new(),
            hooks: Vec::new(),
            warn_free_gb: 50.0,
            block_free_gb: 15.0,
            clean_worktree_rust_targets: true,
            clean_worktree_node_modules: true,
            clean_main_rust_target: false,
            clean_docker_build_cache: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct CleanupCandidate {
    kind: String,
    path: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct ReportedPath {
    path: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct ResourceSnapshot {
    free_disk_gb: f64,
    cpu_load_1m: Option<f64>,
    memory_available_gb: Option<f64>,
    gpu: &'static str,
    paths: Vec<ReportedPath>,
    hooks_configured: usize,
}

pub fn run(args: HygieneArgs) -> Result<(), Box<dyn Error>> {
    let repo_root = repo_root()?;
    let policy_path = policy_path(args.policy.as_deref(), &repo_root)?;
    match args.action {
        HygieneAction::Init { force } => run_init(&policy_path, force),
        HygieneAction::Report { json } => run_report(&policy_path, &repo_root, json),
        HygieneAction::Clean { dry_run, yes } => run_clean(&policy_path, &repo_root, dry_run, yes),
    }
}

fn run_init(path: &Path, force: bool) -> Result<(), Box<dyn Error>> {
    if path.exists() && !force {
        return Err(format!(
            "hygiene: policy already exists: {}\nuse --force to overwrite",
            path.display()
        )
        .into());
    }
    write_policy(path, &HygienePolicy::default())?;
    println!("Wrote hygiene policy: {}", path.display());
    Ok(())
}

fn run_report(path: &Path, repo_root: &Path, json_output: bool) -> Result<(), Box<dyn Error>> {
    let policy = load_policy(path)?;
    let candidates = collect_candidates(&policy, repo_root);
    if json_output {
        let snapshot = resource_snapshot(&policy);
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "policy": path.to_string_lossy(),
                "free_disk_gb": snapshot.free_disk_gb,
                "cpu_load_1m": snapshot.cpu_load_1m,
                "memory_available_gb": snapshot.memory_available_gb,
                "gpu": snapshot.gpu,
                "paths": snapshot.paths,
                "hooks_configured": snapshot.hooks_configured,
                "candidates": candidates,
            }))?
        );
    } else {
        print_report(path, &policy, &candidates);
    }
    Ok(())
}

fn run_clean(
    path: &Path,
    repo_root: &Path,
    dry_run: bool,
    yes: bool,
) -> Result<(), Box<dyn Error>> {
    let policy = load_policy(path)?;
    let candidates = collect_candidates(&policy, repo_root);
    if candidates.is_empty() {
        println!("No safe cleanup candidates found.");
        return Ok(());
    }
    if !yes && !dry_run {
        return Err("hygiene clean: refusing to delete without --yes or --dry-run".into());
    }
    let total = candidates
        .iter()
        .map(|candidate| candidate.bytes)
        .sum::<u64>();
    let verb = if dry_run { "would remove" } else { "removing" };
    println!(
        "{verb} {} safe cleanup candidate(s), {}",
        candidates.len(),
        fmt_size(total)
    );
    for candidate in candidates {
        println!(
            "- {}: {}  {}",
            candidate.kind,
            fmt_size(candidate.bytes),
            candidate.path
        );
        if !dry_run {
            fs::remove_dir_all(&candidate.path).ok();
        }
    }
    if policy.clean_docker_build_cache {
        if dry_run {
            println!("- docker: would run docker system prune -af");
        } else {
            let _ = Command::new("docker")
                .args(["system", "prune", "-af"])
                .status();
        }
    }
    Ok(())
}

fn load_policy(path: &Path) -> Result<HygienePolicy, Box<dyn Error>> {
    if !path.exists() {
        return Ok(HygienePolicy::default());
    }
    serde_json::from_str(&fs::read_to_string(path)?)
        .map_err(|error| format!("hygiene: cannot read policy {}: {error}", path.display()).into())
}

fn write_policy(path: &Path, policy: &HygienePolicy) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(policy)? + "\n")?;
    Ok(())
}

fn collect_candidates(policy: &HygienePolicy, repo_root: &Path) -> Vec<CleanupCandidate> {
    let workspace_root = expand_home(&policy.workspace_root);
    let mut candidates = Vec::new();
    if policy.clean_worktree_rust_targets {
        for path in find_dirs_with_suffix(&workspace_root, &["src", "workers", "target"]) {
            candidates.push(candidate("worktree-rust-target", path));
        }
    }
    if policy.clean_worktree_node_modules {
        for path in find_dirs_with_suffix(&workspace_root, &["src", "node_modules"]) {
            candidates.push(candidate("worktree-node-modules", path));
        }
    }
    if policy.clean_main_rust_target {
        let path = repo_root.join("src").join("workers").join("target");
        if path.exists() {
            candidates.push(candidate("main-rust-target", path));
        }
    }
    candidates.sort_by(|left, right| right.bytes.cmp(&left.bytes));
    candidates
}

fn candidate(kind: &str, path: PathBuf) -> CleanupCandidate {
    CleanupCandidate {
        kind: kind.to_string(),
        bytes: path_size(&path),
        path: path.to_string_lossy().to_string(),
    }
}

fn find_dirs_with_suffix(root: &Path, suffix: &[&str]) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    if !root.exists() {
        return matches;
    }
    visit_dirs(root, &mut |path| {
        if has_suffix(path, suffix) {
            matches.push(path.to_path_buf());
            WalkDecision::SkipChildren
        } else if matches!(
            path.file_name().and_then(OsStr::to_str),
            Some(".git" | "dist" | ".continuum")
        ) {
            WalkDecision::SkipNamed(&["target", "node_modules"])
        } else {
            WalkDecision::Continue
        }
    });
    matches
}

fn resource_snapshot(policy: &HygienePolicy) -> ResourceSnapshot {
    let paths = policy
        .report_paths
        .iter()
        .map(|raw| expand_home(raw))
        .filter(|path| path.exists())
        .map(|path| ReportedPath {
            bytes: path_size(&path),
            path: path.to_string_lossy().to_string(),
        })
        .collect();
    ResourceSnapshot {
        free_disk_gb: gb(fs2::available_space(expand_home(&policy.workspace_root)).unwrap_or(0)),
        cpu_load_1m: cpu_load_1m(),
        memory_available_gb: memory_available_gb(),
        gpu: "hook-required",
        paths,
        hooks_configured: policy.hooks.len(),
    }
}

fn print_report(path: &Path, policy: &HygienePolicy, candidates: &[CleanupCandidate]) {
    let snapshot = resource_snapshot(policy);
    println!("# airc hygiene report");
    println!("policy: {}", path.display());
    println!(
        "workspace_root: {}",
        expand_home(&policy.workspace_root).display()
    );
    println!("free_disk: {:.1} GiB", snapshot.free_disk_gb);
    if let Some(load) = snapshot.cpu_load_1m {
        println!("cpu_load_1m: {load:.2}");
    }
    if let Some(memory) = snapshot.memory_available_gb {
        println!("memory_available: {memory:.1} GiB");
    }
    println!("gpu: {}", snapshot.gpu);
    println!("hooks_configured: {}", snapshot.hooks_configured);
    if snapshot.free_disk_gb < policy.block_free_gb {
        println!(
            "status: BLOCK ({:.1} GiB < {:.1} GiB)",
            snapshot.free_disk_gb, policy.block_free_gb
        );
    } else if snapshot.free_disk_gb < policy.warn_free_gb {
        println!(
            "status: WARN ({:.1} GiB < {:.1} GiB)",
            snapshot.free_disk_gb, policy.warn_free_gb
        );
    } else {
        println!("status: OK");
    }
    println!();
    if candidates.is_empty() {
        println!("No safe cleanup candidates found.");
    } else {
        let total = candidates
            .iter()
            .map(|candidate| candidate.bytes)
            .sum::<u64>();
        println!(
            "safe_cleanup_candidates: {} ({})",
            candidates.len(),
            fmt_size(total)
        );
        for candidate in candidates.iter().take(80) {
            println!(
                "- {}: {}  {}",
                candidate.kind,
                fmt_size(candidate.bytes),
                candidate.path
            );
        }
        if candidates.len() > 80 {
            println!("... {} more", candidates.len() - 80);
        }
    }
    if !snapshot.paths.is_empty() {
        println!();
        println!("reported_paths:");
        for item in snapshot.paths {
            println!("- {}  {}", fmt_size(item.bytes), item.path);
        }
    }
}

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    let current = std::env::current_dir()?;
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&current)
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !raw.is_empty() {
                return Ok(PathBuf::from(raw));
            }
        }
    }
    Ok(current)
}

fn policy_path(value: Option<&Path>, repo_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    Ok(match value {
        Some(path) => expand_home_path(path)?,
        None => repo_root.join(DEFAULT_POLICY_FILE),
    })
}

fn path_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return metadata.len();
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                continue;
            };
            if metadata.is_file() || metadata.file_type().is_symlink() {
                total = total.saturating_add(metadata.len());
            } else if metadata.is_dir() {
                stack.push(child);
            }
        }
    }
    total
}

fn visit_dirs(path: &Path, visitor: &mut impl FnMut(&Path) -> WalkDecision<'_>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return;
    }
    let decision = visitor(path);
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if matches!(decision, WalkDecision::SkipChildren)
            || decision.skips(
                child
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default(),
            )
        {
            continue;
        }
        visit_dirs(&child, visitor);
    }
}

enum WalkDecision<'a> {
    Continue,
    SkipChildren,
    SkipNamed(&'a [&'a str]),
}

impl WalkDecision<'_> {
    fn skips(&self, name: &str) -> bool {
        match self {
            Self::SkipNamed(names) => names.contains(&name),
            _ => false,
        }
    }
}

fn has_suffix(path: &Path, suffix: &[&str]) -> bool {
    let parts = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    parts.ends_with(suffix)
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(value));
    }
    PathBuf::from(value)
}

fn expand_home_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    Ok(expand_home(
        path.to_str().ok_or("policy path is not UTF-8")?,
    ))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

fn cpu_load_1m() -> Option<f64> {
    #[cfg(unix)]
    {
        let mut load = [0.0f64; 1];
        // SAFETY: getloadavg writes at most the provided one-element buffer.
        if unsafe { libc::getloadavg(load.as_mut_ptr(), 1) } == 1 {
            return Some(load[0]);
        }
    }
    None
}

fn memory_available_gb() -> Option<f64> {
    memory_available_gb_linux().or_else(memory_available_gb_macos)
}

fn memory_available_gb_linux() -> Option<f64> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kb as f64 / (1024.0 * 1024.0));
        }
    }
    None
}

fn memory_available_gb_macos() -> Option<f64> {
    let output = Command::new("vm_stat").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let page_size = raw
        .lines()
        .find_map(|line| {
            line.split_once("page size of")?
                .1
                .split_once("bytes")?
                .0
                .trim()
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(4096);
    let mut pages = 0u64;
    for line in raw.lines() {
        if line.starts_with("Pages free:")
            || line.starts_with("Pages inactive:")
            || line.starts_with("Pages speculative:")
        {
            if let Some(value) = line
                .split_once(':')
                .and_then(|(_, value)| value.trim().trim_end_matches('.').parse::<u64>().ok())
            {
                pages = pages.saturating_add(value);
            }
        }
    }
    (pages > 0).then(|| gb(pages.saturating_mul(page_size)))
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

fn fmt_size(bytes: u64) -> String {
    let mut value = bytes as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            if unit == "B" {
                return format!("{} {unit}", value as u64);
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.1} TiB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_round_trips_default_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".airc-policy.json");
        write_policy(&path, &HygienePolicy::default()).unwrap();

        assert_eq!(load_policy(&path).unwrap(), HygienePolicy::default());
    }

    #[test]
    fn unknown_policy_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".airc-policy.json");
        fs::write(&path, r#"{"workspace_root":"/tmp","surprise":true}"#).unwrap();

        assert!(load_policy(&path).is_err());
    }

    #[test]
    fn candidates_find_rebuildable_worktree_caches() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspaces");
        fs::create_dir_all(
            workspace
                .join("one")
                .join("src")
                .join("workers")
                .join("target"),
        )
        .unwrap();
        fs::write(
            workspace
                .join("one")
                .join("src")
                .join("workers")
                .join("target")
                .join("artifact"),
            "abc",
        )
        .unwrap();
        fs::create_dir_all(workspace.join("one").join("src").join("node_modules")).unwrap();
        fs::write(
            workspace
                .join("one")
                .join("src")
                .join("node_modules")
                .join("module"),
            "abcd",
        )
        .unwrap();
        let policy = HygienePolicy {
            workspace_root: workspace.to_string_lossy().to_string(),
            ..HygienePolicy::default()
        };

        let candidates = collect_candidates(&policy, dir.path());

        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == "worktree-rust-target" && candidate.bytes == 3));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == "worktree-node-modules" && candidate.bytes == 4));
    }
}
