//! Retire managed, terminal worktrees without losing their branch history.
use std::path::Path;

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = airc_core::process::background("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

/// Caller must establish terminal card ownership. This function checks the
/// filesystem again immediately before removing anything. A recovery ref keeps
/// the exact history available even after a squash merge removes the remote ref.
pub(crate) fn retire(path: &Path, managed_root: &Path) -> Result<(), String> {
    let root = managed_root.canonicalize().map_err(|e| e.to_string())?;
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if path == root || !path.starts_with(&root) || !path.join(".git").is_file() {
        return Err("refusing non-managed or primary checkout".into());
    }
    let branch = git(&path, &["symbolic-ref", "--short", "HEAD"])?;
    if matches!(branch.as_str(), "main" | "master" | "canary") {
        return Err(format!("refusing integration branch {branch}"));
    }
    if crate::work_commands::probe_dirty_status_at(&path)
        != crate::work_commands::DirtyStatus::Clean
    {
        return Err("worktree has uncommitted, unpushed, or unclassified work".into());
    }
    // Ignored files can contain valuable local artifacts too. Keep them rather
    // than relying on worktree remove's willingness to discard ignored files.
    if !git(
        &path,
        &["ls-files", "--others", "--ignored", "--exclude-standard"],
    )?
    .is_empty()
    {
        return Err("worktree contains ignored files; retain until reviewed".into());
    }
    let common = git(
        &path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let repo = Path::new(&common);
    let branch_line = format!("branch refs/heads/{branch}");
    if git(repo, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter(|line| *line == branch_line)
        .count()
        != 1
    {
        return Err(format!(
            "branch {branch} is shared or its worktree registration is ambiguous"
        ));
    }
    let head = git(&path, &["rev-parse", "HEAD"])?;
    let recovery = format!("refs/airc/retired/{branch}/{head}");
    git(&path, &["update-ref", &recovery, &head])?;
    // No --force, recursive deletion, or submodule fallback. Git protects locked
    // checkouts, dirty races, and initialized submodules. Failure retains branch.
    git(
        repo,
        &[
            "worktree",
            "remove",
            path.to_str().ok_or("non-UTF8 worktree path")?,
        ],
    )?;
    retire_branch(repo, &branch, &head).map_err(|error| {
        format!("checkout removed; branch {branch} retained: {error}; recovery: {recovery}")
    })?;
    println!("retired: {branch}; recovery: {recovery}");
    Ok(())
}

/// Use Git's branch operation, not raw ref deletion: a checkout can appear
/// after the earlier registration probe. Git also retains histories it cannot
/// prove merged (including squash histories without a suitable upstream).
fn retire_branch(repo: &Path, branch: &str, expected_head: &str) -> Result<(), String> {
    if git(repo, &["rev-parse", &format!("refs/heads/{branch}")])? != expected_head {
        return Err("branch advanced since recovery snapshot".into());
    }
    git(repo, &["branch", "-d", "--", branch])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        _temp: tempfile::TempDir,
        repo: std::path::PathBuf,
        root: std::path::PathBuf,
        tree: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            let root = temp.path().join("managed");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            git(&repo, &["init", "-b", "main"]).unwrap();
            git(&repo, &["config", "user.email", "fixture@example.test"]).unwrap();
            git(&repo, &["config", "user.name", "Fixture"]).unwrap();
            git(&repo, &["commit", "--allow-empty", "-m", "base"]).unwrap();
            git(&repo, &["update-ref", "refs/remotes/origin/canary", "HEAD"]).unwrap();
            let tree = root.join("aabbccdd");
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "aabbccdd/fix",
                    tree.to_str().unwrap(),
                ],
            )
            .unwrap();
            Self {
                _temp: temp,
                repo,
                root,
                tree,
            }
        }
    }
    #[test]
    fn retires_branch_and_preserves_exact_recovery_commit() {
        let f = Fixture::new();
        let head = git(&f.tree, &["rev-parse", "HEAD"]).unwrap();
        retire(&f.tree, &f.root).unwrap();
        assert!(!f.tree.exists());
        assert!(git(
            &f.repo,
            &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
        )
        .is_err());
        assert_eq!(
            git(
                &f.repo,
                &[
                    "rev-parse",
                    &format!("refs/airc/retired/aabbccdd/fix/{head}")
                ]
            )
            .unwrap(),
            head
        );
    }
    #[test]
    fn retains_squash_branch_without_upstream_and_keeps_original_history() {
        let f = Fixture::new();
        std::fs::write(f.tree.join("change"), "fix").unwrap();
        git(&f.tree, &["add", "change"]).unwrap();
        git(&f.tree, &["commit", "-m", "original fix"]).unwrap();
        let original = git(&f.tree, &["rev-parse", "HEAD"]).unwrap();
        git(&f.repo, &["merge", "--squash", "aabbccdd/fix"]).unwrap();
        git(&f.repo, &["commit", "-m", "squashed fix"]).unwrap();
        git(
            &f.repo,
            &["update-ref", "refs/remotes/origin/canary", "HEAD"],
        )
        .unwrap();
        assert_ne!(original, git(&f.repo, &["rev-parse", "HEAD"]).unwrap());
        let error = retire(&f.tree, &f.root).unwrap_err();
        assert!(error.contains("branch aabbccdd/fix retained"), "{error}");
        assert!(!f.tree.exists());
        assert_eq!(
            git(&f.repo, &["rev-parse", "refs/heads/aabbccdd/fix"]).unwrap(),
            original
        );
        assert_eq!(
            git(
                &f.repo,
                &[
                    "rev-parse",
                    &format!("refs/airc/retired/aabbccdd/fix/{original}")
                ]
            )
            .unwrap(),
            original
        );
    }

    #[test]
    fn preserves_branch_checked_out_after_original_worktree_removal() {
        let f = Fixture::new();
        let head = git(&f.tree, &["rev-parse", "HEAD"]).unwrap();
        git(&f.repo, &["worktree", "remove", f.tree.to_str().unwrap()]).unwrap();
        // Deterministically model another agent taking the branch between the
        // checkout removal / registration probe and the final branch operation.
        let other = f.root.join("late-checkout");
        git(
            &f.repo,
            &["worktree", "add", other.to_str().unwrap(), "aabbccdd/fix"],
        )
        .unwrap();
        assert!(retire_branch(&f.repo, "aabbccdd/fix", &head).is_err());
        assert_eq!(git(&other, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(
            git(&f.repo, &["rev-parse", "refs/heads/aabbccdd/fix"]).unwrap(),
            head
        );
    }

    #[test]
    fn preserves_initialized_submodule_checkout() {
        let f = Fixture::new();
        let source = f.root.parent().unwrap().join("submodule-source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init"]).unwrap();
        git(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "submodule",
            ],
        )
        .unwrap();
        git(
            &f.tree,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                source.to_str().unwrap(),
                "child",
            ],
        )
        .unwrap();
        git(&f.tree, &["commit", "-am", "submodule"]).unwrap();
        git(
            &f.tree,
            &["update-ref", "refs/remotes/origin/canary", "HEAD"],
        )
        .unwrap();
        assert!(retire(&f.tree, &f.root).is_err());
        assert!(f.tree.join("child/.git").exists());
        assert!(git(
            &f.repo,
            &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
        )
        .is_ok());
    }

    #[test]
    fn preserves_branch_shared_by_another_worktree() {
        let f = Fixture::new();
        let other = f.root.join("other");
        git(
            &f.repo,
            &[
                "worktree",
                "add",
                "--force",
                other.to_str().unwrap(),
                "aabbccdd/fix",
            ],
        )
        .unwrap();
        assert!(retire(&f.tree, &f.root).is_err());
        assert!(f.tree.exists());
        assert!(other.exists());
    }

    #[test]
    fn preserves_dirty_unpushed_locked_and_primary_checkouts() {
        for case in ["dirty", "unpushed", "locked", "ignored"] {
            let f = Fixture::new();
            match case {
                "dirty" => std::fs::write(f.tree.join("work"), "valuable").unwrap(),
                "unpushed" => {
                    git(&f.tree, &["commit", "--allow-empty", "-m", "local"]).unwrap();
                }
                "locked" => {
                    git(&f.repo, &["worktree", "lock", f.tree.to_str().unwrap()]).unwrap();
                }
                "ignored" => {
                    std::fs::write(f.repo.join(".git/info/exclude"), "artifact\n").unwrap();
                    std::fs::write(f.tree.join("artifact"), "valuable").unwrap();
                }
                _ => unreachable!(),
            }
            assert!(retire(&f.tree, &f.root).is_err(), "{case}");
            assert!(f.tree.exists());
            assert!(git(
                &f.repo,
                &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
            )
            .is_ok());
        }
        let f = Fixture::new();
        assert!(retire(&f.repo, f.repo.parent().unwrap()).is_err());
    }
}
