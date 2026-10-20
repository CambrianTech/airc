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
    let submodules = provable_submodules(&path, repo)?;
    let modules = if submodules.is_empty() {
        None
    } else {
        Some(proven_modules_dir(&path, repo, &submodules)?)
    };
    let head = git(&path, &["rev-parse", "HEAD"])?;
    let recovery = format!("refs/airc/retired/{branch}/{head}");
    git(&path, &["update-ref", &recovery, &head])?;
    for sub in &submodules {
        keep_submodule_pin(&path, repo, sub, &branch)?;
    }
    if let Some(modules) = modules {
        // Without --force: git itself refuses a submodule with local changes
        // that appeared after the proof above.
        git(&path, &["submodule", "deinit", "--all"])?;
        // Git refuses `worktree remove` while the checkout's `modules/` exists,
        // and deinit keeps it. Every git directory in it was proven and its pin
        // kept above, so it holds nothing that is not also elsewhere.
        std::fs::remove_dir_all(modules).map_err(|e| e.to_string())?;
    }
    // No --force. Git protects locked checkouts and dirty races; the only
    // recursive deletion is the proven `modules/` above. Failure retains branch.
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

/// An initialized submodule of a card checkout, as `git submodule foreach` names it.
struct Submodule {
    name: String,
    path: String,
    pin: String,
}

/// Removing a worktree deletes its submodules' git directories, so a submodule
/// is retired only when it can hold nothing but the commit the superproject
/// records: checked out at that pin, no changes, no ignored files, and no local
/// branch or stash beyond the pin and its remotes. The pin itself is then kept
/// in the primary checkout's own store of that submodule (offline, like the
/// branch's recovery ref); with no such store the checkout is retained.
fn provable_submodules(path: &Path, repo: &Path) -> Result<Vec<Submodule>, String> {
    let listed = git(
        path,
        &[
            "submodule",
            "foreach",
            "--quiet",
            "echo \"$name|$sm_path|$sha1\"",
        ],
    )?;
    let nested = git(
        path,
        &[
            "submodule",
            "foreach",
            "--quiet",
            "--recursive",
            "echo \"$displaypath\"",
        ],
    )?;
    let submodules: Vec<Submodule> = listed
        .lines()
        .map(|line| {
            let mut parts = line.splitn(3, '|');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(name), Some(path), Some(pin)) => Ok(Submodule {
                    name: name.to_owned(),
                    path: path.to_owned(),
                    pin: pin.to_owned(),
                }),
                _ => Err(format!("unreadable submodule listing line: {line}")),
            }
        })
        .collect::<Result<_, _>>()?;
    if nested.lines().count() != submodules.len() {
        return Err(
            "a submodule has initialized submodules of its own; retain until reviewed".into(),
        );
    }
    for sub in &submodules {
        let at = path.join(&sub.path);
        let unproven = |why: &str| format!("submodule {} {why}; retain until reviewed", sub.path);
        if git(&at, &["rev-parse", "HEAD"])? != sub.pin {
            return Err(unproven("is not at the commit the superproject records"));
        }
        if !git(&at, &["status", "--porcelain"])?.is_empty() {
            return Err(unproven("has uncommitted work"));
        }
        if !git(
            &at,
            &["ls-files", "--others", "--ignored", "--exclude-standard"],
        )?
        .is_empty()
        {
            return Err(unproven("contains ignored files"));
        }
        if !git(
            &at,
            &["rev-list", "--branches", "--not", "--remotes", "HEAD"],
        )?
        .is_empty()
        {
            return Err(unproven("has a local branch beyond its pin and remotes"));
        }
        if git(&at, &["rev-parse", "-q", "--verify", "refs/stash"]).is_ok() {
            return Err(unproven("has a stash"));
        }
        if !primary_store(repo, &sub.name).is_dir() {
            return Err(unproven(
                "has no store in the primary checkout to keep its pin",
            ));
        }
    }
    Ok(submodules)
}

/// The checkout's own `modules/` directory, only when every git directory in it
/// belongs to a proven submodule (a deinitialized one left behind is not proven).
fn proven_modules_dir(
    path: &Path,
    repo: &Path,
    submodules: &[Submodule],
) -> Result<std::path::PathBuf, String> {
    let admin = std::path::PathBuf::from(git(path, &["rev-parse", "--absolute-git-dir"])?);
    let worktrees = repo
        .join("worktrees")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let modules = admin.join("modules");
    if !admin
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(&worktrees)
    {
        return Err("checkout's git directory is not a linked worktree's".into());
    }
    let mut pending = vec![modules.clone()];
    while let Some(dir) = pending.pop() {
        if dir.join("HEAD").is_file() && dir.join("objects").is_dir() {
            let name = dir.strip_prefix(&modules).map_err(|e| e.to_string())?;
            if !submodules.iter().any(|sub| Path::new(&sub.name) == name) {
                return Err(format!(
                    "submodule store {} is not initialized here, so it was not proven; retain until reviewed",
                    name.display()
                ));
            }
            continue;
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(modules)
}

fn primary_store(repo: &Path, name: &str) -> std::path::PathBuf {
    repo.join("modules").join(name)
}

fn keep_submodule_pin(
    path: &Path,
    repo: &Path,
    sub: &Submodule,
    branch: &str,
) -> Result<(), String> {
    let store = primary_store(repo, &sub.name);
    let kept = format!("refs/airc/retired/{branch}/{}/{}", sub.path, sub.pin);
    git(
        &path.join(&sub.path),
        &[
            "push",
            "--quiet",
            store.to_str().ok_or("non-UTF8 submodule store path")?,
            &format!("{}:{kept}", sub.pin),
        ],
    )
    .map(|_| ())
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

    /// Commits a submodule `child` on the card branch and returns its pin. With
    /// `primary_store`, the card is merged and the primary checkout initializes
    /// the submodule, so `<common>/modules/child` exists to keep the pin.
    fn with_submodule(f: &Fixture, primary_store: bool) -> String {
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
        if primary_store {
            // The card merged, and the primary checkout initialized the merged
            // submodule, as a real retirement finds it.
            git(&f.repo, &["merge", "--ff-only", "aabbccdd/fix"]).unwrap();
            git(
                &f.repo,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                ],
            )
            .unwrap();
        }
        git(&f.tree.join("child"), &["rev-parse", "HEAD"]).unwrap()
    }

    // what this catches: card d853c9f9 — every continuum card initializes the
    // llama.cpp submodule, so a plain `worktree remove` refused all of them and
    // 186 merged checkouts (93G) accumulated on one machine.
    #[test]
    fn retires_a_clean_pinned_submodule_and_keeps_its_pin() {
        let f = Fixture::new();
        let pin = with_submodule(&f, true);
        retire(&f.tree, &f.root).unwrap();
        assert!(!f.tree.exists());
        let store = f.repo.join(".git/modules/child");
        assert_eq!(
            git(
                &store,
                &[
                    "rev-parse",
                    &format!("refs/airc/retired/aabbccdd/fix/child/{pin}")
                ]
            )
            .unwrap(),
            pin
        );
    }

    #[test]
    fn preserves_a_submodule_that_could_hold_work() {
        for case in ["no-primary-store", "untracked", "local-branch"] {
            let f = Fixture::new();
            let pin = with_submodule(&f, case != "no-primary-store");
            let child = f.tree.join("child");
            match case {
                "no-primary-store" => {}
                "untracked" => std::fs::write(child.join("work"), "valuable").unwrap(),
                "local-branch" => {
                    git(&child, &["checkout", "-q", "-b", "extra"]).unwrap();
                    git(
                        &child,
                        &[
                            "-c",
                            "user.name=Fixture",
                            "-c",
                            "user.email=fixture@example.test",
                            "commit",
                            "--allow-empty",
                            "-m",
                            "local only",
                        ],
                    )
                    .unwrap();
                    git(&child, &["checkout", "-q", &pin]).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(retire(&f.tree, &f.root).is_err(), "{case}");
            assert!(child.join(".git").exists(), "{case}");
            assert!(git(
                &f.repo,
                &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
            )
            .is_ok());
        }
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
