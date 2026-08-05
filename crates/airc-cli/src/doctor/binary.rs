//! Binary freshness — is the installed binary in step with its source
//! checkout?
//!
//! Surfaces "old binary on PATH", the symptom hit when running a pre-#885
//! binary against a post-#885 schema. The comparison is **directional**
//! (git ancestry, not `!=`): a mismatch names WHICH side is behind,
//! because a current binary against a lagging checkout is a note about the
//! checkout, not a stale binary. See [`classify_source_drift`].

use std::path::Path;

use super::{short_sha, Check, CheckConfig, CheckContext, Finding};

/// Installed binary vs its source checkout. Two `git` probes on a local
/// path — cheap, always runs.
pub(super) struct BinaryFreshnessCheck;

#[async_trait::async_trait]
impl Check for BinaryFreshnessCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::always("binary")
    }

    async fn run(&self, _ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_binary_freshness()
    }
}

fn check_binary_freshness() -> Vec<Finding> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return vec![Finding::info("binary", "couldn't resolve current exe path")],
    };
    let canonical = exe.canonicalize().unwrap_or_else(|_| exe.clone());

    let mut findings = vec![Finding::info(
        "binary",
        format!("install: {}", canonical.display()),
    )];

    // Compare the baked-in build sha (from build.rs) against the
    // current HEAD of the install source tree.
    if !crate::build_info::is_unknown() {
        findings.push(Finding::info(
            "binary",
            format!(
                "build: {} on {}",
                crate::build_info::COMMIT_SHORT,
                crate::build_info::BRANCH
            ),
        ));
        if let Some((source_dir, source_head)) = source_tree_head() {
            let ancestry = source_ancestry(&source_dir, crate::build_info::COMMIT, &source_head);
            findings.push(render_source_drift(classify_source_drift(
                crate::build_info::COMMIT,
                &source_head,
                ancestry,
            )));
        }
    } else {
        findings.push(Finding::info(
            "binary",
            "build sha unknown (git unavailable at compile time); skipping drift check",
        ));
    }

    findings
}

fn source_tree_head() -> Option<(std::path::PathBuf, String)> {
    // The install source path is conventionally `~/.airc/src` per
    // install.sh, but we resolve it the same way `update_commands`
    // does so the two stay aligned.
    let source = crate::update_commands::install_source_dir().ok()?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some((source, value))
    }
}

/// How the installed binary's commit relates to the source checkout's HEAD.
/// Separated from [`classify_source_drift`] so the decision is pure and the
/// git probing is the only part that touches the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ancestry {
    Same,
    /// The binary's commit is an ancestor of source HEAD — the BINARY is behind.
    BinaryBehind,
    /// Source HEAD is an ancestor of the binary's commit — the SOURCE is behind.
    SourceBehind,
    /// Neither commit reaches the other.
    Diverged,
    /// The source tree has no object for the binary's commit (never fetched, or
    /// not a usable git tree). We cannot judge which side is stale.
    Unknowable,
}

/// Which side of a binary-vs-source mismatch is actually the stale one.
///
/// The predecessor of this check compared the two commits and, on ANY
/// mismatch, blamed the BINARY and prescribed `airc update`. That names the
/// wrong object whenever the source checkout is the thing that's behind —
/// measured on BIGMAMA: a freshly installed, CORRECT binary at 701ca0e was
/// reported as `installed binary drifted from source tree` because
/// `install-source` pointed at a worktree still detached at an older commit.
/// The operator then "fixed" the healthy object to silence a warning aimed at
/// the wrong one. A signal has to name the thing that is actually wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceDriftState {
    Match,
    BinaryBehind { binary: String, source: String },
    SourceBehind { binary: String, source: String },
    Diverged { binary: String, source: String },
    Unknowable { binary: String, source: String },
}

/// Pure: no IO, fully testable. `binary` / `source` are full git SHAs.
fn classify_source_drift(binary: &str, source: &str, ancestry: Ancestry) -> SourceDriftState {
    let pair = || (short_sha(binary), short_sha(source));
    match ancestry {
        Ancestry::Same => SourceDriftState::Match,
        Ancestry::BinaryBehind => {
            let (binary, source) = pair();
            SourceDriftState::BinaryBehind { binary, source }
        }
        Ancestry::SourceBehind => {
            let (binary, source) = pair();
            SourceDriftState::SourceBehind { binary, source }
        }
        Ancestry::Diverged => {
            let (binary, source) = pair();
            SourceDriftState::Diverged { binary, source }
        }
        Ancestry::Unknowable => {
            let (binary, source) = pair();
            SourceDriftState::Unknowable { binary, source }
        }
    }
}

/// Render the drift verdict. Only [`SourceDriftState::BinaryBehind`] and
/// `Diverged` are `Warn` — a current binary against a lagging checkout is a
/// note about the CHECKOUT, not a health problem with the node, and must not
/// inflate doctor's "need attention" count.
fn render_source_drift(state: SourceDriftState) -> Finding {
    match state {
        SourceDriftState::Match => {
            Finding::ok("binary", "installed binary matches source checkout HEAD")
        }
        SourceDriftState::BinaryBehind { binary, source } => Finding::warn(
            "binary",
            format!(
                "installed binary is BEHIND its source checkout (binary={binary} source={source})"
            ),
            "run `airc update` to rebuild + reinstall",
        ),
        SourceDriftState::SourceBehind { binary, source } => Finding::info(
            "binary",
            format!(
                "installed binary ({binary}) is CURRENT; the source checkout is behind it \
                 (source={source}) — the binary is fine, the checkout is the stale object \
                 (`git -C <install-source> fetch && git -C <install-source> reset --hard <channel>`)"
            ),
        ),
        SourceDriftState::Diverged { binary, source } => Finding::warn(
            "binary",
            format!(
                "installed binary and source checkout have diverged (binary={binary} \
                 source={source}) — neither reaches the other"
            ),
            "inspect the install source checkout; `airc update` will rebuild from the channel tip",
        ),
        SourceDriftState::Unknowable { binary, source } => Finding::info(
            "binary",
            format!(
                "source checkout has no record of the binary's commit (binary={binary} \
                 source={source}); can't judge which side is stale"
            ),
        ),
    }
}

/// The IO half: ask git how the two commits relate. Any probe failure resolves
/// to [`Ancestry::Unknowable`] — we decline to name a culprit rather than
/// guess one, which is the whole point of this check.
fn source_ancestry(source_dir: &Path, binary: &str, source_head: &str) -> Ancestry {
    if binary == source_head {
        return Ancestry::Same;
    }
    if !commit_exists(source_dir, binary) {
        return Ancestry::Unknowable;
    }
    match (
        is_ancestor(source_dir, binary, source_head),
        is_ancestor(source_dir, source_head, binary),
    ) {
        (Some(true), _) => Ancestry::BinaryBehind,
        (_, Some(true)) => Ancestry::SourceBehind,
        (Some(false), Some(false)) => Ancestry::Diverged,
        // A probe that errored outright tells us nothing.
        _ => Ancestry::Unknowable,
    }
}

fn commit_exists(source_dir: &Path, sha: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(source_dir)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `Some(true)` if `ancestor` reaches `descendant`, `Some(false)` if it
/// provably doesn't, `None` if git couldn't answer.
fn is_ancestor(source_dir: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(source_dir)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .ok()?;
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        // 128 = bad object / not a repo; anything else is not a verdict.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Status;

    /// what this catches: the warn that named the WRONG OBJECT. On 2026-08-05
    /// BIGMAMA installed a correct binary (701ca0e) and doctor reported
    /// `installed binary drifted from source tree` — because `install-source`
    /// pointed at a worktree still detached at an older commit. The binary was
    /// current; the CHECKOUT was stale. The operator then advanced the healthy
    /// object to silence a warning aimed at the wrong one, and doctor counted a
    /// perfectly healthy node as "1 of 9 need attention". A current binary
    /// against a lagging checkout is INFO about the checkout, never a binary warn.
    #[test]
    fn stale_checkout_blames_the_checkout_not_the_current_binary() {
        let state = classify_source_drift(
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
            Ancestry::SourceBehind,
        );
        assert_eq!(
            state,
            SourceDriftState::SourceBehind {
                binary: "111111111111".to_string(),
                source: "222222222222".to_string(),
            }
        );
        let finding = render_source_drift(state);
        assert_eq!(
            finding.status,
            Status::Info,
            "a healthy binary must not inflate the need-attention count"
        );
        assert!(
            finding.detail.contains("CURRENT"),
            "must say the binary is fine: {}",
            finding.detail
        );
        assert!(
            !finding.detail.contains("binary drifted"),
            "must not accuse the binary: {}",
            finding.detail
        );
    }

    #[test]
    fn genuinely_stale_binary_still_warns_and_still_prescribes_update() {
        // what this catches: the fix for the mis-blame must not blunt the case
        // it was built for — an old binary on PATH is the original symptom
        // (pre-#885 binary against post-#885 schema) and must stay a WARN whose
        // fix is the real recovery path.
        let finding = render_source_drift(classify_source_drift(
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
            Ancestry::BinaryBehind,
        ));
        assert_eq!(finding.status, Status::Warn);
        assert!(finding.detail.contains("BEHIND"));
        assert_eq!(
            finding.fix.as_deref(),
            Some("run `airc update` to rebuild + reinstall")
        );
    }

    #[test]
    fn unknowable_ancestry_declines_to_name_a_culprit() {
        // what this catches: when the source tree has never fetched the binary's
        // commit we know the shas differ but NOT which is stale. Guessing here is
        // how the original bug shipped, so the honest verdict is info-with-both-shas.
        let finding = render_source_drift(classify_source_drift(
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
            Ancestry::Unknowable,
        ));
        assert_eq!(finding.status, Status::Info);
        assert!(finding.detail.contains("can't judge"));
    }

    /// what this catches: the ancestry probe itself, against REAL git history
    /// rather than reasoning about it. Every verdict above is only as good as
    /// this mapping, and `merge-base --is-ancestor` exit codes (0/1/128) are
    /// exactly the kind of thing that is easy to get backwards on paper.
    #[test]
    fn source_ancestry_reads_real_git_history() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .expect("git must be available to run this test");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q", "."]);
        std::fs::write(root.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let first = git(&["rev-parse", "HEAD"]);
        std::fs::write(root.join("a"), "2").unwrap();
        git(&["commit", "-qam", "two"]);
        let second = git(&["rev-parse", "HEAD"]);
        assert_ne!(first, second);

        assert_eq!(
            source_ancestry(root, &second, &second),
            Ancestry::Same,
            "identical commits"
        );
        assert_eq!(
            source_ancestry(root, &first, &second),
            Ancestry::BinaryBehind,
            "binary built from an older commit than the checkout"
        );
        assert_eq!(
            source_ancestry(root, &second, &first),
            Ancestry::SourceBehind,
            "THE BIGMAMA CASE: checkout is behind the installed binary"
        );
        assert_eq!(
            source_ancestry(root, "0123456789012345678901234567890123456789", &second),
            Ancestry::Unknowable,
            "a commit this tree has never seen is not evidence against the binary"
        );
    }
}
