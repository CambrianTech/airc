//! Stable work-domain data models.

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P0,
    P1,
    #[default]
    P2,
    P3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Open,
    Claimed,
    InProgress,
    Blocked,
    Review,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    Planned,
    Active,
    Blocked,
    Landing,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Requested,
    Allocated,
    Active,
    Released,
    Orphaned,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAvailabilityState {
    Ready,
    Busy,
    Away,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BranchName(String);

impl BranchName {
    pub fn new(value: impl Into<String>) -> Result<Self, BranchNameError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(BranchNameError::Empty);
        }
        if trimmed.contains(char::is_whitespace) {
            return Err(BranchNameError::ContainsWhitespace);
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BranchName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BranchNameError {
    #[error("branch name cannot be empty")]
    Empty,
    #[error("branch name cannot contain whitespace")]
    ContainsWhitespace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repo: RepoId,
    pub number: u64,
    pub head: BranchName,
    pub base: BranchName,
}

/// Git object id such as a commit SHA. Kept as a string because Git
/// installations may expose full SHA-1 today and SHA-256 in newer
/// repositories; validation only enforces a non-empty hexadecimal id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GitObjectId(String);

impl GitObjectId {
    pub fn new(value: impl Into<String>) -> Result<Self, GitObjectIdError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(GitObjectIdError::Empty);
        }
        if !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(GitObjectIdError::NonHex);
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GitObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GitObjectIdError {
    #[error("git object id cannot be empty")]
    Empty,
    #[error("git object id must be hexadecimal")]
    NonHex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirtyState {
    Clean,
    Dirty,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrCheckState {
    Queued,
    Running,
    Passed,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrReviewState {
    Requested,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrMergeState {
    Open,
    Draft,
    Ready,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCard {
    pub card_id: WorkCardId,
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
    pub state: CardState,
    pub owner: Option<PeerId>,
    pub claim_id: Option<ClaimId>,
    pub claim_expires_at_ms: Option<u64>,
    pub last_heartbeat_at_ms: Option<u64>,
    pub pull_request: Option<PullRequestRef>,
    pub created_by: PeerId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLease {
    pub workspace_id: WorkspaceId,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub repo: RepoId,
    pub path: String,
    pub branch: BranchName,
    pub base: BranchName,
    pub status: WorkspaceStatus,
    pub disk_bytes: Option<u64>,
    pub created_at_ms: u64,
    pub heartbeat_at_ms: u64,
    pub released_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HygieneReport {
    pub repo: RepoId,
    pub reporter: PeerId,
    pub total_bytes: u64,
    pub rebuildable_bytes: u64,
    pub workspace_count: usize,
    pub recorded_at_ms: u64,
}

/// Categorizes drain candidates by destruction-safety. Closed set so
/// policy code (PR 4 hygiene cleaner) can pattern-match exhaustively.
///
/// `Unknown` is the catch-all for anything a future adapter discovers
/// that doesn't fit existing categories. Default policy MUST treat
/// `Unknown` as non-rebuildable and refuse to drain without explicit
/// opt-in — that's the "no silent destruction" half of the drain rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainCandidateCategory {
    /// e.g. Cargo `target/`, Gradle `build/`, sccache — rebuilt by
    /// tooling on next invocation. Safe to drain by default.
    RebuildableCache,
    /// e.g. compiled artifacts checked into worktrees, generated docs,
    /// rendered media. Reproducible but the regeneration may be expensive
    /// or require manual steps. Not safe-by-default.
    GeneratedArtifact,
    /// e.g. `~/.cargo/registry`, `~/.npm`, `~/.gradle/caches`, Maven
    /// local. Re-downloads on next build. Safe to drain by default
    /// (network cost only).
    DownloadedDependency,
    /// Docker image layers + buildkit caches. Re-pulls/rebuilds on next
    /// docker run. Network + CPU cost; not safe-by-default because the
    /// rebuild may be slow.
    DockerLayer,
    /// Model weights and inference caches. Re-downloads can be very
    /// large; not safe-by-default.
    ModelCache,
    /// Test recordings, Playwright traces, instrumentation captures.
    /// Often single-purpose; deletion may break in-flight debugging.
    /// Not safe-by-default.
    TraceArtifact,
    /// Unrecognized. Policy must NOT touch by default.
    Unknown,
}

impl DrainCandidateCategory {
    /// Whether the default policy treats this category as safe to drain
    /// without explicit user confirmation. Conservative by design — only
    /// rebuildable/downloaded categories are safe-by-default. Anything
    /// else requires opt-in so policy never silently destroys work.
    pub fn safe_by_default(self) -> bool {
        matches!(self, Self::RebuildableCache | Self::DownloadedDependency)
    }
}

/// A single drain candidate identified by a workspace inventory pass.
/// Recorded in `WorkspaceDrainRequested` so the decision is inspectable
/// after the fact (record/replay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainCandidate {
    pub path: String,
    pub category: DrainCandidateCategory,
    /// Best-effort size estimate at request time. Actual reclaim shows
    /// up on the completion event as `DrainOutcome.bytes_reclaimed`.
    pub est_bytes: u64,
}

/// Severity of disk pressure. Distinguished so policy can escalate
/// gracefully (telemetry → drain rebuildable → drain aggressive →
/// block new work).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureLevel {
    /// Comfortable headroom. Telemetry only.
    Nominal,
    /// Approaching policy thresholds; consider draining rebuildable caches.
    Elevated,
    /// At policy thresholds; drain rebuildable caches now.
    High,
    /// Below safety floor; drain everything safe + require explicit
    /// confirmation for Unknown/non-rebuildable categories.
    Critical,
}

/// Per-drain outcome carried by `WorkspaceDrainCompleted`. Partial
/// drains are first-class — a drain that reclaimed half its candidates
/// reports the half that succeeded AND the reasons the rest didn't.
/// "It worked" without specifics is not an acceptable completion record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainOutcome {
    pub bytes_reclaimed: u64,
    pub paths_touched: Vec<String>,
    pub paths_skipped: Vec<String>,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_name_rejects_empty_and_whitespace() {
        assert!(matches!(BranchName::new(""), Err(BranchNameError::Empty)));
        assert!(matches!(
            BranchName::new("feat/bad branch"),
            Err(BranchNameError::ContainsWhitespace)
        ));
        assert_eq!(
            BranchName::new(" feat/good ").unwrap().as_str(),
            "feat/good"
        );
    }

    #[test]
    fn git_object_id_normalizes_hex_and_rejects_unknown_shapes() {
        assert_eq!(GitObjectId::new(" ABCD1234 ").unwrap().as_str(), "abcd1234");
        assert!(matches!(GitObjectId::new(""), Err(GitObjectIdError::Empty)));
        assert!(matches!(
            GitObjectId::new("not-a-sha"),
            Err(GitObjectIdError::NonHex)
        ));
    }
}
