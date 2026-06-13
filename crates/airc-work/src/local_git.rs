//! Local git workspace observation adapter.
//!
//! This module is deliberately below CLI/monitor concerns: it reads one
//! git worktree and produces typed work-domain events. Consumers decide
//! when to call it and how to persist the returned snapshot.

use std::path::{Path, PathBuf};
use std::process::Command;

use airc_core::PeerId;

use crate::{
    BranchName, DirtyState, GitBranchMoved, GitCommitObserved, GitDirtyStateChanged, GitObjectId,
    RepoId, WorkEvent, WorkspaceId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGitWorkspace {
    pub repo: RepoId,
    pub workspace_id: Option<WorkspaceId>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGitSnapshot {
    pub branch: BranchName,
    pub head: GitObjectId,
    pub summary: Option<String>,
    pub dirty_state: DirtyState,
    pub dirty_paths: u64,
    pub untracked_paths: u64,
}

pub trait GitCommandRunner {
    fn run_git(&self, repo_path: &Path, args: &[&str]) -> Result<String, LocalGitError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CommandGitRunner;

impl GitCommandRunner for CommandGitRunner {
    fn run_git(&self, repo_path: &Path, args: &[&str]) -> Result<String, LocalGitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .map_err(|source| LocalGitError::GitSpawn {
                repo_path: repo_path.to_path_buf(),
                source,
            })?;

        if !output.status.success() {
            return Err(LocalGitError::GitCommand {
                repo_path: repo_path.to_path_buf(),
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

#[derive(Debug, Clone)]
pub struct LocalGitObserver<R = CommandGitRunner> {
    runner: R,
}

impl Default for LocalGitObserver<CommandGitRunner> {
    fn default() -> Self {
        Self::new(CommandGitRunner)
    }
}

impl<R> LocalGitObserver<R>
where
    R: GitCommandRunner,
{
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn observe(
        &self,
        workspace: &LocalGitWorkspace,
    ) -> Result<LocalGitSnapshot, LocalGitError> {
        let branch = parse_branch(
            self.runner
                .run_git(&workspace.path, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        )?;
        let head = GitObjectId::new(
            self.runner
                .run_git(&workspace.path, &["rev-parse", "HEAD"])?,
        )?;
        let summary = optional_summary(
            self.runner
                .run_git(&workspace.path, &["log", "-1", "--pretty=%s"])?,
        );
        let (dirty_state, dirty_paths, untracked_paths) = parse_status_porcelain(
            &self
                .runner
                .run_git(&workspace.path, &["status", "--porcelain=v1"])?,
        );

        Ok(LocalGitSnapshot {
            branch,
            head,
            summary,
            dirty_state,
            dirty_paths,
            untracked_paths,
        })
    }
}

pub fn local_git_events_since(
    workspace: &LocalGitWorkspace,
    previous: Option<&LocalGitSnapshot>,
    current: &LocalGitSnapshot,
    observed_by: PeerId,
    observed_at_ms: u64,
) -> Vec<WorkEvent> {
    let mut events = Vec::new();

    if previous.is_none_or(|snapshot| snapshot.head != current.head) {
        events.push(WorkEvent::GitCommitObserved(GitCommitObserved {
            repo: workspace.repo.clone(),
            commit: current.head.clone(),
            branch: Some(current.branch.clone()),
            summary: current.summary.clone(),
            observed_by,
            observed_at_ms,
        }));
    }

    if previous
        .is_none_or(|snapshot| snapshot.branch != current.branch || snapshot.head != current.head)
    {
        events.push(WorkEvent::GitBranchMoved(GitBranchMoved {
            repo: workspace.repo.clone(),
            branch: current.branch.clone(),
            old_head: previous.map(|snapshot| snapshot.head.clone()),
            new_head: current.head.clone(),
            moved_by: observed_by,
            moved_at_ms: observed_at_ms,
        }));
    }

    if previous.is_none_or(|snapshot| dirty_changed(snapshot, current)) {
        events.push(WorkEvent::GitDirtyStateChanged(GitDirtyStateChanged {
            repo: workspace.repo.clone(),
            workspace_id: workspace.workspace_id,
            path: workspace.path.display().to_string(),
            state: current.dirty_state,
            dirty_paths: current.dirty_paths,
            untracked_paths: current.untracked_paths,
            changed_by: observed_by,
            changed_at_ms: observed_at_ms,
        }));
    }

    events
}

fn dirty_changed(previous: &LocalGitSnapshot, current: &LocalGitSnapshot) -> bool {
    previous.dirty_state != current.dirty_state
        || previous.dirty_paths != current.dirty_paths
        || previous.untracked_paths != current.untracked_paths
}

fn parse_branch(value: String) -> Result<BranchName, LocalGitError> {
    BranchName::new(value).map_err(LocalGitError::BranchName)
}

fn optional_summary(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn parse_status_porcelain(value: &str) -> (DirtyState, u64, u64) {
    let mut dirty_paths = 0;
    let mut untracked_paths = 0;

    for line in value.lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with("??") {
            untracked_paths += 1;
        } else {
            dirty_paths += 1;
        }
    }

    let state = if dirty_paths == 0 && untracked_paths == 0 {
        DirtyState::Clean
    } else {
        DirtyState::Dirty
    };

    (state, dirty_paths, untracked_paths)
}

#[derive(Debug, thiserror::Error)]
pub enum LocalGitError {
    #[error("failed to spawn git for {repo_path}: {source}")]
    GitSpawn {
        repo_path: PathBuf,
        source: std::io::Error,
    },
    #[error("git command failed in {repo_path}: git {args:?} exited {status:?}: {stderr}")]
    GitCommand {
        repo_path: PathBuf,
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    #[error(transparent)]
    BranchName(#[from] crate::model::BranchNameError),
    #[error(transparent)]
    GitObjectId(#[from] crate::model::GitObjectIdError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeGitRunner {
        outputs: BTreeMap<Vec<String>, Result<String, String>>,
    }

    impl FakeGitRunner {
        fn with_output(mut self, args: &[&str], output: &str) -> Self {
            self.outputs.insert(
                args.iter().map(|arg| (*arg).to_string()).collect(),
                Ok(output.to_string()),
            );
            self
        }

        fn with_error(mut self, args: &[&str], stderr: &str) -> Self {
            self.outputs.insert(
                args.iter().map(|arg| (*arg).to_string()).collect(),
                Err(stderr.to_string()),
            );
            self
        }
    }

    impl GitCommandRunner for FakeGitRunner {
        fn run_git(&self, repo_path: &Path, args: &[&str]) -> Result<String, LocalGitError> {
            let key: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
            match self.outputs.get(&key) {
                Some(Ok(output)) => Ok(output.clone()),
                Some(Err(stderr)) => Err(LocalGitError::GitCommand {
                    repo_path: repo_path.to_path_buf(),
                    args: key,
                    status: Some(128),
                    stderr: stderr.clone(),
                }),
                None => Err(LocalGitError::GitCommand {
                    repo_path: repo_path.to_path_buf(),
                    args: key,
                    status: Some(1),
                    stderr: "missing fake output".to_string(),
                }),
            }
        }
    }

    fn workspace() -> LocalGitWorkspace {
        LocalGitWorkspace {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            workspace_id: Some(WorkspaceId::from_u128(7)),
            path: PathBuf::from("/work/airc"),
        }
    }

    fn clean_runner() -> FakeGitRunner {
        FakeGitRunner::default()
            .with_output(&["rev-parse", "--abbrev-ref", "HEAD"], "rust-rewrite")
            .with_output(
                &["rev-parse", "HEAD"],
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .with_output(&["log", "-1", "--pretty=%s"], "latest work")
            .with_output(&["status", "--porcelain=v1"], "")
    }

    #[test]
    fn observe_reads_clean_git_snapshot() {
        let observer = LocalGitObserver::new(clean_runner());

        let snapshot = observer.observe(&workspace()).unwrap();

        assert_eq!(snapshot.branch.as_str(), "rust-rewrite");
        assert_eq!(
            snapshot.head.as_str(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(snapshot.summary.as_deref(), Some("latest work"));
        assert_eq!(snapshot.dirty_state, DirtyState::Clean);
        assert_eq!(snapshot.dirty_paths, 0);
        assert_eq!(snapshot.untracked_paths, 0);
    }

    #[test]
    fn observe_counts_dirty_and_untracked_paths() {
        let runner = clean_runner().with_output(
            &["status", "--porcelain=v1"],
            " M crates/airc-work/src/local_git.rs\n?? docs/new.md\nA  src/lib.rs\n",
        );
        let observer = LocalGitObserver::new(runner);

        let snapshot = observer.observe(&workspace()).unwrap();

        assert_eq!(snapshot.dirty_state, DirtyState::Dirty);
        assert_eq!(snapshot.dirty_paths, 2);
        assert_eq!(snapshot.untracked_paths, 1);
    }

    #[test]
    fn initial_observation_emits_commit_branch_and_dirty_events() {
        let snapshot = LocalGitObserver::new(clean_runner())
            .observe(&workspace())
            .unwrap();

        let events =
            local_git_events_since(&workspace(), None, &snapshot, PeerId::from_u128(9), 100);

        assert!(matches!(events[0], WorkEvent::GitCommitObserved(_)));
        assert!(matches!(events[1], WorkEvent::GitBranchMoved(_)));
        assert!(matches!(events[2], WorkEvent::GitDirtyStateChanged(_)));
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn unchanged_snapshot_emits_no_events() {
        let snapshot = LocalGitObserver::new(clean_runner())
            .observe(&workspace())
            .unwrap();

        let events = local_git_events_since(
            &workspace(),
            Some(&snapshot),
            &snapshot,
            PeerId::from_u128(9),
            100,
        );

        assert!(events.is_empty());
    }

    #[test]
    fn dirty_change_emits_only_dirty_state_event() {
        let previous = LocalGitObserver::new(clean_runner())
            .observe(&workspace())
            .unwrap();
        let current = LocalGitSnapshot {
            dirty_state: DirtyState::Dirty,
            dirty_paths: 1,
            untracked_paths: 0,
            ..previous.clone()
        };

        let events = local_git_events_since(
            &workspace(),
            Some(&previous),
            &current,
            PeerId::from_u128(9),
            100,
        );

        assert!(matches!(events[0], WorkEvent::GitDirtyStateChanged(_)));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn head_change_emits_commit_and_branch_events() {
        let previous = LocalGitObserver::new(clean_runner())
            .observe(&workspace())
            .unwrap();
        let current = LocalGitSnapshot {
            head: GitObjectId::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
            summary: Some("new head".to_string()),
            ..previous.clone()
        };

        let events = local_git_events_since(
            &workspace(),
            Some(&previous),
            &current,
            PeerId::from_u128(9),
            100,
        );

        assert!(matches!(events[0], WorkEvent::GitCommitObserved(_)));
        assert!(matches!(events[1], WorkEvent::GitBranchMoved(_)));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn git_command_failure_is_not_converted_to_unknown_state() {
        let runner =
            clean_runner().with_error(&["status", "--porcelain=v1"], "not a git repository");
        let observer = LocalGitObserver::new(runner);

        let err = observer.observe(&workspace()).unwrap_err();

        assert!(matches!(err, LocalGitError::GitCommand { .. }));
    }
}
