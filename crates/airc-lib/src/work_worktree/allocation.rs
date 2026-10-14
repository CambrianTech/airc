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
    let output = airc_core::process::background("git")
        .current_dir(worktree)
        .args(["for-each-ref", "--format=%(objectname)", ALLOCATION_REF])
        .output()
        .map_err(|e| format!("read allocation ref: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "read allocation ref: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let object = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    if object.trim().is_empty() {
        return Ok(None);
    }
    let output = airc_core::process::background("git")
        .current_dir(worktree)
        .args(["cat-file", "blob", object.trim()])
        .output()
        .map_err(|e| format!("read allocation object: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "read allocation object: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    decode_allocation_evidence(&output.stdout, &creation_base(worktree)?).map(Some)
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

// Git consumes finite metadata on stdin. Always close and reap, including a failed write.
fn git_metadata(worktree: &Path, args: &[&str], input: &[u8]) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = airc_core::process::background("git")
        .current_dir(worktree)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git metadata spawn: {e}"))?;
    let written = child
        .stdin
        .take()
        .ok_or_else(|| "git metadata stdin unavailable".to_owned())
        .and_then(|mut stdin| {
            stdin
                .write_all(input)
                .map_err(|e| format!("git metadata write: {e}"))
        });
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git metadata wait: {e}"))?;
    written?;
    if !output.status.success() {
        return Err(format!(
            "git metadata {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

pub(super) fn record(worktree: &Path, evidence: &AllocationEvidence) -> Result<(), String> {
    let base = &evidence.creation_sha;
    let bytes = serde_json::to_vec(evidence).map_err(|e| e.to_string())?;
    let object = git_metadata(worktree, &["hash-object", "-w", "--stdin"], &bytes)?;
    // One create-only ref transaction publishes both pieces of allocation evidence.
    git_metadata(
        worktree,
        &["update-ref", "--stdin"],
        format!(
            "create {CREATION_BASE_REF} {base}\ncreate {ALLOCATION_REF} {}\n",
            object.trim()
        )
        .as_bytes(),
    )?;
    Ok(())
}
