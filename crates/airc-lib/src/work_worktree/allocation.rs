//! Worktree-private immutable allocation metadata, published with its creation anchor.
use super::{creation_base, CREATION_BASE_REF};
use airc_work::RepoId;
use std::path::Path;

/// Worktree-private ref containing the JSON allocation blob.
pub const ALLOCATION_REF: &str = "refs/worktree/airc-allocation";

/// Immutable allocation intent, distinct from the worktree's current HEAD.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AllocationEvidence {
    pub repo: RepoId,
    pub requested_branch: String,
    pub integration_base: Option<String>,
    pub start_ref: String,
    pub creation_sha: String,
}

/// Legacy trees without an allocation record return `None`; never backfill from HEAD.
pub fn allocation_evidence(worktree: &Path) -> Result<Option<AllocationEvidence>, String> {
    use airc_work::local_git::{CommandGitRunner, GitCommandRunner};
    let object = CommandGitRunner
        .run_git(
            worktree,
            &["for-each-ref", "--format=%(objectname)", ALLOCATION_REF],
        )
        .map_err(|e| e.to_string())?;
    if object.is_empty() {
        return Ok(None);
    }
    let bytes = CommandGitRunner
        .run_git_input(worktree, &["cat-file", "blob", &object], None)
        .map_err(|e| e.to_string())?;
    decode_allocation_evidence(&bytes, &creation_base(worktree)?).map(Some)
}
/// Async consumers can read Git through their process owner, then use this same
/// decoder with the recorded creation anchor (never the mutable current HEAD).
pub fn decode_allocation_evidence(
    bytes: &[u8],
    creation_sha: &str,
) -> Result<AllocationEvidence, String> {
    let evidence: AllocationEvidence =
        serde_json::from_slice(bytes).map_err(|e| format!("decode allocation evidence: {e}"))?;
    if evidence.creation_sha != creation_sha {
        return Err("allocation evidence does not match the immutable creation anchor".into());
    }
    Ok(evidence)
}

pub(super) fn record(worktree: &Path, evidence: &AllocationEvidence) -> Result<(), String> {
    use airc_work::local_git::CommandGitRunner;
    let base = &evidence.creation_sha;
    let bytes = serde_json::to_vec(evidence).map_err(|e| e.to_string())?;
    let object = CommandGitRunner
        .run_git_input(worktree, &["hash-object", "-w", "--stdin"], Some(&bytes))
        .map_err(|e| e.to_string())?;
    let object = String::from_utf8(object).map_err(|e| e.to_string())?;
    // One create-only ref transaction publishes both pieces of allocation evidence.
    CommandGitRunner
        .run_git_input(
            worktree,
            &["update-ref", "--stdin"],
            Some(
                format!(
                    "create {CREATION_BASE_REF} {base}\ncreate {ALLOCATION_REF} {}\n",
                    object.trim()
                )
                .as_bytes(),
            ),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}
