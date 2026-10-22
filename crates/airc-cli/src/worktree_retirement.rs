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
    release_submodules(&path)?;
    // No --force or recursive deletion. Git protects locked checkouts and dirty
    // races; submodules were released above only if they held nothing unique.
    // Failure retains branch.
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

/// Let `worktree remove` retire a worktree that initialized submodules. Git keeps
/// each submodule's repository in the worktree's own gitdir (`modules/`) and
/// refuses to remove the worktree while that directory exists, even after a
/// clean `deinit`, so every repo with a submodule (continuum's vendored engine)
/// left each merged card's checkout behind for a human.
///
/// `modules/` is deleted only when every repository in it was examined and holds
/// nothing that exists nowhere else (Codex on #1602: never infer safety from
/// what a scan skipped). Every repository on disk must belong to an initialized
/// submodule at its recorded commit; one that does not (a deinitialized
/// submodule's repository) is unexamined, so the worktree is kept. Each examined
/// repository must have no stash, no commit (refs or reflog) reachable from neither its remotes
/// nor the recorded commit, and no ignored files in its checkout. Every git call
/// is checked; a failure keeps the worktree. `deinit` runs WITHOUT --force, so
/// git itself refuses a checkout that changed since.
fn release_submodules(path: &Path) -> Result<(), String> {
    let git_dir = git(path, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let modules = Path::new(&git_dir).join("modules");
    if !modules.is_dir() {
        return Ok(());
    }
    let mut examined = Vec::new();
    for line in git(path, &["submodule", "status", "--recursive"])?.lines() {
        // `submodule status` leads each line with its state (' ' at the recorded
        // commit, '-' not initialized, '+' elsewhere, 'U' conflicted). `git()` trims
        // the output, which strips the first line's leading ' ', so a line that
        // starts with the commit itself is a ' ' line.
        let (state, rest) = match line.chars().next() {
            Some(mark @ (' ' | '-' | '+' | 'U')) => (mark, &line[1..]),
            Some(c) if c.is_ascii_hexdigit() => (' ', line),
            _ => return Err(format!("unreadable submodule status line: {line}")),
        };
        let mut fields = rest.split_whitespace();
        let (Some(recorded), Some(sub)) = (fields.next(), fields.next()) else {
            return Err(format!("unreadable submodule status line: {line}"));
        };
        match state {
            ' ' => {}
            '-' => continue, // not initialized here: its repository, if any, stays unexamined
            _ => {
                return Err(format!(
                    "submodule {sub} is not at its recorded commit ({state})"
                ))
            }
        }
        let checkout = path.join(sub);
        let repo = git(&checkout, &["rev-parse", "--absolute-git-dir"])?;
        let unique = git(
            &checkout,
            &[
                "rev-list",
                "--all",
                "--reflog",
                "--not",
                "--remotes",
                recorded,
            ],
        )?;
        if !unique.is_empty() {
            return Err(format!("submodule {sub} holds commits found nowhere else"));
        }
        if !git(&checkout, &["stash", "list"])?.is_empty() {
            return Err(format!("submodule {sub} holds a stash"));
        }
        if !git(
            &checkout,
            &["ls-files", "--others", "--ignored", "--exclude-standard"],
        )?
        .is_empty()
        {
            return Err(format!(
                "submodule {sub} contains ignored files; retain until reviewed"
            ));
        }
        examined.push(
            Path::new(&repo)
                .canonicalize()
                .map_err(|e| format!("submodule {sub} repository: {e}"))?,
        );
    }
    for repo in repositories_under(&modules)? {
        if !examined.contains(&repo) {
            return Err(format!(
                "unexamined submodule repository retained: {}",
                repo.display()
            ));
        }
    }
    git(path, &["submodule", "deinit", "--all"])?;
    std::fs::remove_dir_all(&modules).map_err(|e| format!("submodule repositories retained: {e}"))
}

/// Every git repository under `dir`: a directory holding `HEAD` and `objects/`,
/// and the nested submodules in its own `modules/`. Canonical paths, so they
/// compare with what `rev-parse --absolute-git-dir` reports.
fn repositories_under(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        if next.join("HEAD").is_file() && next.join("objects").is_dir() {
            found.push(next.canonicalize().map_err(|e| e.to_string())?);
            let nested = next.join("modules");
            if nested.is_dir() {
                pending.push(nested);
            }
            continue;
        }
        for entry in std::fs::read_dir(&next).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(found)
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

    /// A worktree with an initialized submodule pinned at the recorded commit, the
    /// shape of every continuum card checkout (core/vendor/llama.cpp).
    fn with_submodule(f: &Fixture) -> std::path::PathBuf {
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
        f.tree.join("child")
    }

    // what this catches: `git worktree remove` refusing every worktree that
    // initialized a submodule, which left each merged card's checkout behind
    // (continuum, 2026-10-22: 20b4f33b and c180de5d removed by hand).
    #[test]
    fn retires_a_clean_worktree_with_an_initialized_submodule() {
        let f = Fixture::new();
        with_submodule(&f);
        // merged, as a retired card's branch is, so the branch itself retires too
        git(&f.repo, &["merge", "--ff-only", "aabbccdd/fix"]).unwrap();
        retire(&f.tree, &f.root).unwrap();
        assert!(!f.tree.exists());
        assert!(git(
            &f.repo,
            &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
        )
        .is_err());
    }

    // what this catches: releasing a submodule repository that holds the only
    // copy of a commit. The commit sits on the submodule's branch while its
    // checkout is back at the recorded commit, so the superproject reads clean
    // and only the release check can see it; the worktree, its submodule and
    // the branch are all kept.
    #[test]
    fn preserves_a_submodule_holding_a_commit_found_nowhere_else() {
        let f = Fixture::new();
        let child = with_submodule(&f);
        let recorded = git(&f.tree, &["rev-parse", "HEAD:child"]).unwrap();
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
                "only here",
            ],
        )
        .unwrap();
        git(&child, &["checkout", "--quiet", "--detach", &recorded]).unwrap();
        assert!(git(&f.tree, &["status", "--porcelain"]).unwrap().is_empty());
        let error = retire(&f.tree, &f.root).unwrap_err();
        assert!(error.contains("found nowhere else"), "{error}");
        assert!(child.join(".git").exists());
        assert!(git(
            &f.repo,
            &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
        )
        .is_ok());
    }

    // what this catches (Codex on #1602): a scan of initialized submodules only,
    // then deleting every repository under modules/. A submodule deinitialized
    // after a commit landed only on its branch keeps its repository on disk but
    // is skipped by any initialized-only scan; it must keep the worktree.
    #[test]
    fn preserves_a_deinitialized_submodule_repository_it_could_not_examine() {
        let f = Fixture::new();
        let child = with_submodule(&f);
        let recorded = git(&f.tree, &["rev-parse", "HEAD:child"]).unwrap();
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
                "only here",
            ],
        )
        .unwrap();
        git(&child, &["checkout", "--quiet", "--detach", &recorded]).unwrap();
        let repo = git(&child, &["rev-parse", "--absolute-git-dir"]).unwrap();
        git(&f.tree, &["submodule", "deinit", "child"]).unwrap();
        let error = retire(&f.tree, &f.root).unwrap_err();
        assert!(error.contains("unexamined"), "{error}");
        assert!(
            Path::new(&repo).join("HEAD").is_file(),
            "the repository is kept"
        );
        assert!(git(
            &f.repo,
            &["rev-parse", "--verify", "refs/heads/aabbccdd/fix"]
        )
        .is_ok());
    }

    // what this catches (Codex on #1602): a commit made on a detached HEAD and left behind
    // when the checkout returned to the recorded commit. No ref names it, so `--all` alone
    // misses it; only the reflog still reaches it, and that keeps the repository.
    #[test]
    fn preserves_a_submodule_commit_only_its_reflog_reaches() {
        let f = Fixture::new();
        let child = with_submodule(&f);
        let recorded = git(&f.tree, &["rev-parse", "HEAD:child"]).unwrap();
        git(&child, &["checkout", "--quiet", "--detach", &recorded]).unwrap();
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
                "detached only",
            ],
        )
        .unwrap();
        git(&child, &["checkout", "--quiet", "--detach", &recorded]).unwrap();
        assert!(git(&f.tree, &["status", "--porcelain"]).unwrap().is_empty());
        let error = retire(&f.tree, &f.root).unwrap_err();
        assert!(error.contains("found nowhere else"), "{error}");
        assert!(child.join(".git").exists());
    }

    // what this catches (Codex on #1602): the superproject's ignored-files check
    // does not recurse, so an ignored artifact inside a submodule checkout would
    // be deleted with the worktree.
    #[test]
    fn preserves_an_ignored_artifact_inside_a_submodule() {
        let f = Fixture::new();
        let child = with_submodule(&f);
        let exclude = git(&child, &["rev-parse", "--git-path", "info/exclude"]).unwrap();
        let exclude = child.join(exclude);
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        std::fs::write(&exclude, "artifact\n").unwrap();
        std::fs::write(child.join("artifact"), "valuable").unwrap();
        let error = retire(&f.tree, &f.root).unwrap_err();
        assert!(error.contains("ignored files"), "{error}");
        assert!(child.join("artifact").is_file());
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
