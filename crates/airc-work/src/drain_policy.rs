//! Pure policy evaluator for workspace lease drains.
//!
//! Given a `WorkspaceLease` and observable side data (current time,
//! optional git status of the worktree, optional PR status), classify
//! the lease into one of three decisions:
//!
//! - [`DrainDecision::Admit`]: safe to delete the worktree on disk.
//!   The variant `AdmitReason` records why so the decision is
//!   inspectable after the fact and survives record/replay.
//! - [`DrainDecision::ReportOnly`]: drain *candidate* but the policy
//!   refuses to act without explicit override. The variant carries the
//!   reason a human (or higher-layer policy) needs to inspect before
//!   admitting.
//! - [`DrainDecision::Keep`]: lease is still active or insufficient
//!   information to admit — leave the worktree on disk.
//!
//! The evaluator is *pure*: callers gather git/PR/time externally and
//! pass them in, so this module is straightforward to unit test and
//! safe to run from anywhere (CLI, daemon, persona).
//!
//! Personas-as-leasers come for free: the evaluator reads only the
//! `WorkspaceLease` record, which carries the owning `PeerId`. The
//! policy doesn't know or care whether the owner is an interactive AI
//! agent, a Continuum persona, or a long-lived service.

use serde::{Deserialize, Serialize};

use crate::model::{WorkspaceLease, WorkspaceStatus};

/// Default heartbeat staleness threshold: 24h.
///
/// Public so policy callers can compare against it when explaining a
/// decision to the user without re-deriving the constant.
pub const DEFAULT_HEARTBEAT_STALE_MS: u64 = 24 * 60 * 60 * 1000;

/// What the policy thinks should happen to a lease's worktree on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrainDecision {
    /// Safe to delete. `reason` names the specific rule that admitted.
    Admit { reason: AdmitReason },
    /// Drain candidate, but NOT safe-by-default. A human or a higher-
    /// layer policy must explicitly admit.
    ReportOnly { reason: ReportOnlyReason },
    /// Lease still active — leave the worktree alone.
    Keep,
}

/// Reasons the default policy admits a lease for drain. Closed set so
/// downstream UIs (`airc lease drain --dry-run`, hygiene reports) can
/// pattern-match exhaustively for human-readable output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AdmitReason {
    /// Lease was explicitly released by its owner (`WorkspaceReleased`
    /// event landed on the transcript).
    Released,
    /// Lease was marked orphaned by the projector (e.g., owner peer
    /// went away without releasing). Drain is the recovery path.
    Orphaned,
    /// Heartbeat exceeded the configured stale threshold. Records both
    /// the observed age and the threshold so the explanation survives
    /// without re-deriving them.
    HeartbeatExpired { age_ms: u64, threshold_ms: u64 },
    /// The PR for this lease's branch is in a terminal state.
    PrTerminal { state: PrTerminalState },
    /// The working tree is clean AND the branch has been merged into
    /// its base. Belt and suspenders for the "PR merged but the lease
    /// owner forgot to release" case when PR data isn't available.
    BranchLandedClean,
}

/// Subset of `PrStatus` representing PR states the policy considers
/// terminal for drain purposes. Carried in `AdmitReason::PrTerminal`
/// so the admit reason itself records which terminal state landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrTerminalState {
    Merged,
    Closed,
}

/// Reasons the default policy refuses to admit without explicit
/// override. Each variant carries the data a human needs to make a
/// final call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReportOnlyReason {
    /// `WorkspaceStatus::Failed` — allocation failed, the lease state
    /// itself is the symptom that wants attention. Don't silently
    /// drain because a human likely needs to see this.
    Failed,
    /// `git status --porcelain` shows untracked or modified paths.
    /// Refusing to drain is the only safe default.
    DirtyWorkingTree { dirty_paths: usize },
    /// A PR exists for this lease's branch and is still open. The user
    /// is presumably still working on it.
    PrOpen { number: u64 },
    /// No PR info available AND the branch hasn't been merged into
    /// base. May be in-progress work without a PR yet.
    BranchNotLanded,
    /// Lease state is recognized but no admit rule fires. Catch-all
    /// for "active but indeterminate" — needs human inspection.
    NoAdmitRule,
}

/// Inputs the policy needs beyond the lease itself. All optional
/// except `now_ms` and `config` so callers in different contexts
/// (interactive CLI vs. headless daemon) can provide whatever they
/// have available.
#[derive(Debug, Clone)]
pub struct PolicyInputs<'a> {
    pub now_ms: u64,
    pub git: Option<&'a GitStatusSummary>,
    pub pr: Option<&'a PrStatus>,
    pub config: &'a PolicyConfig,
}

/// Tunables for the default policy. Defaults match the
/// `rust-substrate-grievances-and-gaps.md` doc language; consumers
/// (CLI flags, persona configuration) can override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfig {
    /// Heartbeat older than this admits the lease (`HeartbeatExpired`).
    pub heartbeat_stale_ms: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            heartbeat_stale_ms: DEFAULT_HEARTBEAT_STALE_MS,
        }
    }
}

/// What the policy needs to know about the on-disk working tree.
/// `dirty_paths` is the count of entries reported by
/// `git status --porcelain`; `branch_merged_to_base` reflects
/// `git merge-base --is-ancestor BRANCH BASE`. Callers compute these
/// once per lease so the evaluator stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusSummary {
    pub dirty_paths: usize,
    pub branch_merged_to_base: bool,
}

/// What the policy needs to know about the lease's PR. `NotFound`
/// distinguishes "we asked GitHub and there's no PR" from "we didn't
/// look" (caller passes `None` for the whole `PrStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrStatus {
    Open { number: u64 },
    Merged { number: u64 },
    Closed { number: u64 },
    NotFound,
}

/// Run the default policy against a lease.
pub fn evaluate(lease: &WorkspaceLease, inputs: &PolicyInputs<'_>) -> DrainDecision {
    use AdmitReason::*;
    use DrainDecision::*;
    use ReportOnlyReason::*;

    // Status-driven terminal decisions first. These are deterministic
    // from the lease record alone; no side inputs needed.
    match lease.status {
        WorkspaceStatus::Released => return Admit { reason: Released },
        WorkspaceStatus::Orphaned => return Admit { reason: Orphaned },
        WorkspaceStatus::Failed => return ReportOnly { reason: Failed },
        // No worktree has been materialized yet; nothing on disk to
        // drain. Keep so the caller doesn't list it as a candidate.
        WorkspaceStatus::Requested => return Keep,
        WorkspaceStatus::Allocated | WorkspaceStatus::Active => {}
    }

    // Heartbeat staleness — admit if past threshold. Uses
    // `saturating_sub` so a clock skew where heartbeat_at_ms > now_ms
    // yields age=0 (keep) rather than underflow.
    let age = inputs.now_ms.saturating_sub(lease.heartbeat_at_ms);
    if age > inputs.config.heartbeat_stale_ms {
        return Admit {
            reason: HeartbeatExpired {
                age_ms: age,
                threshold_ms: inputs.config.heartbeat_stale_ms,
            },
        };
    }

    // PR-terminal short-circuits the git checks. A merged/closed PR
    // means the lease is done regardless of working-tree state.
    if let Some(pr) = inputs.pr {
        match pr {
            PrStatus::Merged { .. } => {
                return Admit {
                    reason: PrTerminal {
                        state: PrTerminalState::Merged,
                    },
                };
            }
            PrStatus::Closed { .. } => {
                return Admit {
                    reason: PrTerminal {
                        state: PrTerminalState::Closed,
                    },
                };
            }
            PrStatus::Open { number } => {
                return ReportOnly {
                    reason: PrOpen { number: *number },
                };
            }
            PrStatus::NotFound => {}
        }
    }

    // Git-driven: clean + landed admits; dirty always blocks.
    if let Some(git) = inputs.git {
        if git.dirty_paths > 0 {
            return ReportOnly {
                reason: DirtyWorkingTree {
                    dirty_paths: git.dirty_paths,
                },
            };
        }
        if git.branch_merged_to_base {
            return Admit {
                reason: BranchLandedClean,
            };
        }
        return ReportOnly {
            reason: BranchNotLanded,
        };
    }

    // No side inputs, lease still active per heartbeat. Defer to the
    // caller — a daemon would loop again later; the CLI would report.
    ReportOnly {
        reason: NoAdmitRule,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ClaimId, RepoId, WorkCardId, WorkspaceId};
    use crate::model::BranchName;
    use airc_core::ids::PeerId;
    use uuid::Uuid;

    fn lease(status: WorkspaceStatus, heartbeat_at_ms: u64) -> WorkspaceLease {
        WorkspaceLease {
            workspace_id: WorkspaceId::from_uuid(Uuid::new_v4()),
            card_id: WorkCardId::from_uuid(Uuid::new_v4()),
            claim_id: ClaimId::from_uuid(Uuid::new_v4()),
            owner: PeerId::from_uuid(Uuid::new_v4()),
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            path: "/Users/x/.airc/worktrees/x/feature".to_string(),
            branch: BranchName::new("feat/x").unwrap(),
            base: BranchName::new("rust-rewrite").unwrap(),
            status,
            disk_bytes: None,
            created_at_ms: 1_000,
            heartbeat_at_ms,
            released_at_ms: if status == WorkspaceStatus::Released {
                Some(heartbeat_at_ms)
            } else {
                None
            },
        }
    }

    fn inputs<'a>(
        now_ms: u64,
        git: Option<&'a GitStatusSummary>,
        pr: Option<&'a PrStatus>,
        config: &'a PolicyConfig,
    ) -> PolicyInputs<'a> {
        PolicyInputs {
            now_ms,
            git,
            pr,
            config,
        }
    }

    #[test]
    fn released_admits_regardless_of_inputs() {
        let l = lease(WorkspaceStatus::Released, 100);
        let cfg = PolicyConfig::default();
        let decision = evaluate(&l, &inputs(200, None, None, &cfg));
        assert_eq!(
            decision,
            DrainDecision::Admit {
                reason: AdmitReason::Released,
            }
        );
    }

    #[test]
    fn orphaned_admits() {
        let l = lease(WorkspaceStatus::Orphaned, 100);
        let cfg = PolicyConfig::default();
        assert_eq!(
            evaluate(&l, &inputs(200, None, None, &cfg)),
            DrainDecision::Admit {
                reason: AdmitReason::Orphaned,
            }
        );
    }

    #[test]
    fn failed_is_report_only() {
        let l = lease(WorkspaceStatus::Failed, 100);
        let cfg = PolicyConfig::default();
        assert_eq!(
            evaluate(&l, &inputs(200, None, None, &cfg)),
            DrainDecision::ReportOnly {
                reason: ReportOnlyReason::Failed,
            }
        );
    }

    #[test]
    fn requested_keeps_no_disk_to_drain() {
        let l = lease(WorkspaceStatus::Requested, 100);
        let cfg = PolicyConfig::default();
        assert_eq!(
            evaluate(&l, &inputs(200, None, None, &cfg)),
            DrainDecision::Keep
        );
    }

    #[test]
    fn stale_heartbeat_admits_past_threshold() {
        let l = lease(WorkspaceStatus::Active, 100);
        let cfg = PolicyConfig {
            heartbeat_stale_ms: 1_000,
        };
        // now=2000, hb=100 → age=1900 > 1000
        let decision = evaluate(&l, &inputs(2_000, None, None, &cfg));
        match decision {
            DrainDecision::Admit {
                reason:
                    AdmitReason::HeartbeatExpired {
                        age_ms,
                        threshold_ms,
                    },
            } => {
                assert_eq!(age_ms, 1_900);
                assert_eq!(threshold_ms, 1_000);
            }
            other => panic!("expected HeartbeatExpired, got {other:?}"),
        }
    }

    #[test]
    fn fresh_heartbeat_no_side_inputs_is_report_only_no_admit_rule() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        // heartbeat fresh (delta 50ms), no git, no PR → no rule admits.
        assert_eq!(
            evaluate(&l, &inputs(1_050, None, None, &cfg)),
            DrainDecision::ReportOnly {
                reason: ReportOnlyReason::NoAdmitRule,
            }
        );
    }

    #[test]
    fn pr_merged_admits_even_on_dirty_tree() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let pr = PrStatus::Merged { number: 42 };
        let git = GitStatusSummary {
            dirty_paths: 5,
            branch_merged_to_base: false,
        };
        // PR-terminal short-circuits dirty tree.
        let decision = evaluate(&l, &inputs(1_050, Some(&git), Some(&pr), &cfg));
        assert_eq!(
            decision,
            DrainDecision::Admit {
                reason: AdmitReason::PrTerminal {
                    state: PrTerminalState::Merged,
                },
            }
        );
    }

    #[test]
    fn pr_closed_admits() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let pr = PrStatus::Closed { number: 99 };
        assert_eq!(
            evaluate(&l, &inputs(1_050, None, Some(&pr), &cfg)),
            DrainDecision::Admit {
                reason: AdmitReason::PrTerminal {
                    state: PrTerminalState::Closed,
                },
            }
        );
    }

    #[test]
    fn pr_open_is_report_only() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let pr = PrStatus::Open { number: 7 };
        assert_eq!(
            evaluate(&l, &inputs(1_050, None, Some(&pr), &cfg)),
            DrainDecision::ReportOnly {
                reason: ReportOnlyReason::PrOpen { number: 7 },
            }
        );
    }

    #[test]
    fn dirty_tree_is_report_only_without_pr_data() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let git = GitStatusSummary {
            dirty_paths: 3,
            branch_merged_to_base: true, // even merged, dirty blocks
        };
        assert_eq!(
            evaluate(&l, &inputs(1_050, Some(&git), None, &cfg)),
            DrainDecision::ReportOnly {
                reason: ReportOnlyReason::DirtyWorkingTree { dirty_paths: 3 },
            }
        );
    }

    #[test]
    fn clean_landed_admits_without_pr_data() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let git = GitStatusSummary {
            dirty_paths: 0,
            branch_merged_to_base: true,
        };
        assert_eq!(
            evaluate(&l, &inputs(1_050, Some(&git), None, &cfg)),
            DrainDecision::Admit {
                reason: AdmitReason::BranchLandedClean,
            }
        );
    }

    #[test]
    fn clean_unlanded_is_report_only_branch_not_landed() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let git = GitStatusSummary {
            dirty_paths: 0,
            branch_merged_to_base: false,
        };
        assert_eq!(
            evaluate(&l, &inputs(1_050, Some(&git), None, &cfg)),
            DrainDecision::ReportOnly {
                reason: ReportOnlyReason::BranchNotLanded,
            }
        );
    }

    #[test]
    fn pr_not_found_falls_through_to_git_checks() {
        let l = lease(WorkspaceStatus::Active, 1_000);
        let cfg = PolicyConfig::default();
        let pr = PrStatus::NotFound;
        let git = GitStatusSummary {
            dirty_paths: 0,
            branch_merged_to_base: true,
        };
        assert_eq!(
            evaluate(&l, &inputs(1_050, Some(&git), Some(&pr), &cfg)),
            DrainDecision::Admit {
                reason: AdmitReason::BranchLandedClean,
            }
        );
    }

    #[test]
    fn clock_skew_heartbeat_in_future_keeps_active() {
        // hb=2000 > now=1000 → saturating_sub yields age=0
        // age=0 < threshold → no HeartbeatExpired admit; falls through.
        let l = lease(WorkspaceStatus::Active, 2_000);
        let cfg = PolicyConfig::default();
        assert_eq!(
            evaluate(&l, &inputs(1_000, None, None, &cfg)),
            DrainDecision::ReportOnly {
                reason: ReportOnlyReason::NoAdmitRule,
            }
        );
    }
}
