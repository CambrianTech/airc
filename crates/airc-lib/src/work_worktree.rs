//! Claim-time worktree — the per-card workspace ANY claimer gets.
//!
//! Card 01611f25. This concern lived in `airc-cli` and resolved the repo from
//! the process's cwd (`git rev-parse --show-toplevel`), refusing when cwd's
//! repo did not match the card's. That is correct for an operator standing in
//! a checkout and impossible for a **citizen**: a persona claiming a card has
//! no meaningful cwd, so it got `worktree spawn skipped` and could not pull a
//! repo card at all. Moving the concern here — taking its inputs EXPLICITLY —
//! is what lets any claimer get a workspace.
//!
//! ## What deliberately did NOT move
//!
//! The lease-zone *policy* check stays at the CLI boundary
//! (`airc-cli/src/lease.rs`), which documents why: "cwd is process state —
//! keeping it out of the substrate preserves the substrate-of-truth /
//! SDK-composes / CLI-consumes layering." Nothing here reads cwd. What moved
//! is only the part that never needed it: **where** a card's worktree lives,
//! and creating it from an explicitly supplied clone.
//!
//! ## Invariants preserved from the CLI implementation
//!
//! - The claim is authoritative; the worktree is convenience (card d1b2798d).
//!   A git failure must never undo a claim — callers treat errors as
//!   best-effort.
//! - An existing path is REUSED, never clobbered.
//! - The path is a pure function of the card id, so a claimer (or
//!   `card_staging` rooting a citizen's hands) can look it up without guessing.

use std::path::{Path, PathBuf};

use airc_work::{RepoId, WorkCardId};

/// The lease zone, relative to the user's home: `~/.airc/worktrees`.
/// One source of truth for the location; `airc-cli`'s lease policy compares
/// against this same constant rather than spelling the path again.
pub const LEASE_ZONE_RELATIVE: &str = ".airc/worktrees";

/// How many hex chars of the card id name the worktree directory: the shown
/// width, so an operator reading the board can find the directory by eye.
pub use airc_core::shown_id::SHORT_ID_LEN;

/// Git keeps refs/worktree/* private to each linked worktree. This immutable
/// creation anchor survives branch movement and keeps its commit reachable.
pub const CREATION_BASE_REF: &str = "refs/worktree/airc-creation-base";

mod allocation;

pub use allocation::{
    allocation_evidence, decode_allocation_evidence, AllocationEvidence, ALLOCATION_REF,
};

/// Read only the recorded creation base. Legacy worktrees have no anchor: callers
/// must ask for an explicit base rather than infer one from HEAD or a remote.
pub fn creation_base(worktree: &Path) -> Result<String, String> {
    resolve_commit(worktree, CREATION_BASE_REF).map_err(|error| {
        format!("worktree creation base is unavailable; pass base_sha explicitly: {error}")
    })
}

fn resolve_commit(repo: &Path, revision: &str) -> Result<String, String> {
    use airc_work::local_git::{CommandGitRunner, GitCommandRunner};
    CommandGitRunner
        .run_git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{revision}^{{commit}}"),
            ],
        )
        .map_err(|e| format!("resolve commit {revision}: {e}"))
}

/// `~/.airc/worktrees`, or `None` when neither `$HOME` nor `$USERPROFILE` is
/// set. Environment, not cwd — a citizen has a home even with no checkout.
pub fn worktree_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join(LEASE_ZONE_RELATIVE))
}

/// The directory name for a card: the first [`SHORT_ID_LEN`] chars of its id.
pub fn short_id(card_id: WorkCardId) -> String {
    card_id.shown()
}

/// Where this card's worktree lives — a pure function of the card id, so a
/// claimer can find its workspace without having created it. This is the
/// lookup `card_staging` needs to root a citizen's hands at a repo card.
pub fn worktree_path_for(card_id: WorkCardId) -> Option<PathBuf> {
    worktree_root().map(|root| root.join(short_id(card_id)))
}

/// What [`ensure_worktree`] did. Reuse is a success, not a no-op failure:
/// re-claiming a card an agent is already working must not disturb the work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOutcome {
    Created(PathBuf),
    Reused(PathBuf),
}

impl WorktreeOutcome {
    pub fn path(&self) -> &Path {
        match self {
            WorktreeOutcome::Created(p) | WorktreeOutcome::Reused(p) => p,
        }
    }
}

/// Everything creating a worktree needs, supplied by the caller. `clone_path`
/// is the crux: the CLI derives it from cwd, a citizen gets it from config —
/// either way this module never guesses ([[no-fallbacks-ever]]).
#[derive(Debug, Clone)]
pub struct WorktreeSpec<'a> {
    pub card_id: WorkCardId,
    pub repo: &'a RepoId,
    /// Local clone of the card's repo that the worktree branches from.
    pub clone_path: &'a Path,
    /// Branch to create for the work.
    pub branch: &'a str,
    /// Branch/commit the new branch starts from (e.g. `origin/canary`).
    /// `None` resolves the configured repository integration branch's remote ref.
    /// Otherwise use the remote's recorded symbolic default (`origin/HEAD`).
    pub start_point: Option<&'a str>,
}

/// Create the card's worktree, or report that it already exists.
///
/// Best-effort by contract: every error is a reason the convenience was not
/// provided, never a reason to undo a claim.
pub fn ensure_worktree(spec: &WorktreeSpec<'_>) -> Result<WorktreeOutcome, String> {
    let path = worktree_path_for(spec.card_id)
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    if path.exists() {
        return Ok(WorktreeOutcome::Reused(path));
    }
    let root = path
        .parent()
        .ok_or_else(|| format!("worktree path {} has no parent", path.display()))?;
    std::fs::create_dir_all(root)
        .map_err(|e| format!("create lease zone {}: {e}", root.display()))?;

    // Resolve once before creation and give Git the immutable object, so a branch
    // moving concurrently cannot make the recorded base differ from the new tree.
    let mut integration_base = crate::work_repository::configured_base_branch(spec.repo);
    let start_ref = match spec.start_point {
        Some(explicit) => explicit.to_owned(),
        None => match integration_base.as_deref() {
            Some(base) => format!("refs/remotes/origin/{base}"),
            None => {
                use airc_work::local_git::{CommandGitRunner, GitCommandRunner};
                let remote_default = CommandGitRunner
                    .run_git(
                        spec.clone_path,
                        &["symbolic-ref", "refs/remotes/origin/HEAD"],
                    )
                    .map_err(|e| {
                        format!("remote default unavailable; supply start_point explicitly: {e}")
                    })?;
                let branch = remote_default
                    .strip_prefix("refs/remotes/origin/")
                    .filter(|branch| !branch.is_empty() && *branch != "HEAD")
                    .ok_or_else(|| {
                        format!("remote default is not an origin branch: {remote_default}")
                    })?;
                integration_base = Some(branch.to_owned());
                remote_default
            }
        },
    };
    if spec.start_point.is_none() {
        use airc_work::local_git::{CommandGitRunner, GitCommandRunner};
        let branch = integration_base
            .as_deref()
            .ok_or_else(|| "selected integration base is unavailable".to_owned())?;
        let refspec = format!("refs/heads/{branch}:{start_ref}");
        CommandGitRunner
            .run_git(spec.clone_path, &["fetch", "--no-tags", "origin", &refspec])
            .map_err(|e| format!("refresh integration base before allocation: {e}"))?;
    }
    let base = resolve_commit(spec.clone_path, &start_ref)?;
    let mut cmd = airc_core::process::background("git");
    cmd.current_dir(spec.clone_path)
        .args(["worktree", "add", "-b", spec.branch])
        .arg(path.as_os_str())
        .arg(&base);
    let out = cmd
        .output()
        .map_err(|e| format!("spawn git worktree add: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git worktree add failed in {}: {}",
            spec.clone_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Create-only publication never overwrites an existing anchor.
    // Record before submodule initialization, whose failure leaves the tree reusable.
    let evidence = AllocationEvidence {
        repo: spec.repo.clone(),
        requested_branch: spec.branch.to_owned(),
        integration_base,
        start_ref,
        creation_sha: base.clone(),
    };
    allocation::record(&path, &evidence)?;
    init_submodules(&path)?;
    Ok(WorktreeOutcome::Created(path))
}

/// Initialise submodules in a freshly created worktree.
///
/// `git worktree add` does NOT populate submodules — the new tree gets empty
/// directories where they should be. A lease worktree exists to be built in, and a
/// worktree that cannot build is not a lease, so this runs as part of creating one
/// rather than being left as a step every claimer rediscovers.
///
/// Measured 2026-09-04: this cost three agents on two machines a full evening
/// between them. In continuum the empty directory is `core/vendor/llama.cpp`, and
/// the build fails as a CMake error about a missing `CMakeLists.txt` — naming a path
/// the reader never chose, with nothing pointing at submodules.
///
/// The failure is WORSE than a local one because of the shared `CARGO_TARGET_DIR`
/// the fleet runs by doctrine: the failed build caches a build-script result with the
/// worktree's path baked in, so the operator's MAIN checkout — which has the
/// submodule and built fine minutes earlier — then fails with the identical error
/// naming a worktree directory it is not in. Recovery is `cargo clean -p <crate>`
/// plus a full rebuild.
///
/// Do NOT "fix" this by symlinking the parent clone's vendor directory: git then
/// refuses `status`/`diff` with "expected submodule path not to be a symbolic link",
/// which trades a build failure for a broken diff (M5, same evening).
///
/// A repo with no submodules is unaffected — the command is a successful no-op — so
/// this is safe for every scope, not only continuum.
fn init_submodules(worktree: &std::path::Path) -> Result<(), String> {
    let out = airc_core::process::background("git")
        .current_dir(worktree)
        .args(["submodule", "update", "--init", "--recursive"])
        .output()
        .map_err(|e| format!("spawn git submodule update in {}: {e}", worktree.display()))?;
    if !out.status.success() {
        // Loud, not silent: the worktree EXISTS at this point, so say so plainly and
        // hand over the exact command. Swallowing this would hand back a lease that
        // looks ready and fails at the first build — the shape the doc above
        // describes, which is the thing being fixed.
        return Err(format!(
            "worktree {} was created but `git submodule update --init --recursive` \
             failed there: {}\n  Builds in it will fail on the empty submodule paths \
             (and can poison a shared CARGO_TARGET_DIR). Run that command in the \
             worktree before building.",
            worktree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_id() -> WorkCardId {
        WorkCardId::from_uuid(
            "01611f25-bd86-49c8-af4a-174c2cce7293"
                .parse()
                .expect("fixed test uuid parses"),
        )
    }

    // what this catches: the path must be a PURE function of the card id, or a
    // claimer and `card_staging` would disagree about where a citizen's hands
    // are rooted (card 01611f25). Also pins the 8-char shape the board prints.
    #[test]
    fn worktree_path_is_a_pure_function_of_the_card_id() {
        let id = card_id();
        assert_eq!(short_id(id), "01611f25");
        let a = worktree_path_for(id);
        let b = worktree_path_for(id);
        assert_eq!(a, b, "same card must always resolve to the same workspace");
        if let Some(p) = a {
            assert!(p.ends_with("01611f25"));
            assert!(p.to_string_lossy().contains(LEASE_ZONE_RELATIVE));
        }
    }

    // what this catches: an existing worktree must be REUSED, never clobbered —
    // re-claiming a card an agent is mid-work on would otherwise destroy
    // uncommitted work.
    //
    // `temp_env::with_var` rather than a hand-rolled set_var/restore: this
    // crate's test binary runs in parallel and siblings read `$HOME`
    // (`airc.rs`, `mesh_identity.rs`). airc.rs:2907 already documents that
    // exact race making another test flaky on a clean canary — reintroducing
    // the pattern here would have been a known-bad move (#1377 review).
    #[test]
    fn an_existing_worktree_is_reused_not_clobbered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let id = card_id();

        temp_env::with_var("HOME", Some(tmp.path()), || {
            let path = worktree_path_for(id).expect("path resolves under synthetic HOME");
            std::fs::create_dir_all(&path).expect("pre-create the worktree dir");
            std::fs::write(path.join("uncommitted.txt"), b"work in progress").expect("write");

            let spec = WorktreeSpec {
                card_id: id,
                repo: &RepoId::new("Example/repo").unwrap(),
                clone_path: tmp.path(),
                branch: "irrelevant",
                start_point: None,
            };
            let outcome = ensure_worktree(&spec).expect("reuse path never shells git");
            assert_eq!(outcome, WorktreeOutcome::Reused(path.clone()));
            assert!(
                path.join("uncommitted.txt").exists(),
                "reuse must not disturb work in progress"
            );
        });
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = airc_core::process::background("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &std::path::Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.invalid"]);
        git(dir, &["config", "user.name", "t"]);
        // A worktree of a repo with no commit has no start point to add from.
        std::fs::write(dir.join("README"), b"root\n").expect("write README");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "root"]);
    }

    // what this catches: moving the integration branch or committing work must not
    // change an omitted submission base; reuse cannot invent a legacy base either.
    #[test]
    fn creation_base_is_pinned_per_worktree_and_never_inferred_on_reuse() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let original = resolve_commit(&repo, "HEAD").unwrap();
        temp_env::with_var("HOME", Some(&home), || {
            let spec = WorktreeSpec {
                card_id: card_id(),
                repo: &RepoId::new("Example/repo").unwrap(),
                clone_path: &repo,
                branch: "first",
                start_point: Some("main"),
            };
            let first = ensure_worktree(&spec).unwrap().path().to_owned();
            assert_eq!(creation_base(&first).unwrap(), original);
            let allocated = allocation_evidence(&first).unwrap().unwrap();
            assert_eq!(allocated.repo, *spec.repo);
            assert_eq!(allocated.requested_branch, "first");
            assert_eq!(allocated.start_ref, "main");
            assert_eq!(allocated.creation_sha, original);
            let mut replacement = allocated.clone();
            replacement.requested_branch = "different intent".into();
            assert!(allocation::record(&first, &replacement).is_err());
            assert_eq!(
                allocation_evidence(&first).unwrap(),
                Some(allocated.clone())
            );
            git(
                &repo,
                &["commit", "--allow-empty", "-qm", "integration moved"],
            );
            let advanced = resolve_commit(&repo, "HEAD").unwrap();
            assert_ne!(advanced, original);
            let second_spec = WorktreeSpec {
                card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                branch: "second",
                ..spec.clone()
            };
            let second = ensure_worktree(&second_spec).unwrap().path().to_owned();
            assert_eq!(creation_base(&second).unwrap(), advanced);
            assert_eq!(
                creation_base(&first).unwrap(),
                original,
                "anchors are worktree-local"
            );
            git(
                &first,
                &["commit", "--allow-empty", "-qm", "work committed"],
            );
            assert_eq!(
                ensure_worktree(&spec).unwrap(),
                WorktreeOutcome::Reused(first.clone())
            );
            assert_eq!(creation_base(&first).unwrap(), original);
            assert_eq!(allocation_evidence(&first).unwrap(), Some(allocated));
            assert!(
                creation_base(&repo).is_err(),
                "main checkout must not inherit linked anchor"
            );
            assert_eq!(allocation_evidence(&repo).unwrap(), None);
            git(&first, &["update-ref", CREATION_BASE_REF, &advanced]);
            assert!(allocation_evidence(&first)
                .unwrap_err()
                .contains("does not match"));
            git(&first, &["update-ref", CREATION_BASE_REF, &original]);
            git(&first, &["update-ref", "-d", ALLOCATION_REF]);
            assert_eq!(allocation_evidence(&first).unwrap(), None);
            git(&first, &["update-ref", "-d", CREATION_BASE_REF]);
            assert!(matches!(
                ensure_worktree(&spec).unwrap(),
                WorktreeOutcome::Reused(_)
            ));
            assert!(
                creation_base(&first).unwrap_err().contains("pass base_sha"),
                "reusing a legacy tree must not record its changed HEAD as the base"
            );
            assert_eq!(allocation_evidence(&first).unwrap(), None);
        });
    }

    #[test]
    fn default_allocation_uses_repository_policy_not_clone_head() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let old_integration = resolve_commit(&repo, "HEAD").unwrap();
        git(&repo, &["remote", "add", "origin", repo.to_str().unwrap()]);
        git(&repo, &["checkout", "-qb", "canary"]);
        git(
            &repo,
            &["commit", "--allow-empty", "-qm", "remote advanced"],
        );
        let integration = resolve_commit(&repo, "HEAD").unwrap();
        git(&repo, &["checkout", "-q", "main"]);
        git(
            &repo,
            &["update-ref", "refs/remotes/origin/canary", &old_integration],
        );
        git(&repo, &["commit", "--allow-empty", "-qm", "unrelated main"]);
        let main = resolve_commit(&repo, "HEAD").unwrap();
        assert_ne!(integration, main);
        temp_env::with_vars(
            [
                ("HOME", Some(tmp.path().as_os_str())),
                ("AIRC_PR_BASE", None),
            ],
            || {
                let spec = WorktreeSpec {
                    card_id: card_id(),
                    repo: &RepoId::new("CambrianTech/continuum").unwrap(),
                    clone_path: &repo,
                    branch: "work",
                    start_point: None,
                };
                let path = ensure_worktree(&spec).unwrap().path().to_owned();
                assert_eq!(resolve_commit(&path, "HEAD").unwrap(), integration);
                let evidence = allocation_evidence(&path).unwrap().unwrap();
                assert_eq!(evidence.integration_base.as_deref(), Some("canary"));
                assert_eq!(evidence.start_ref, "refs/remotes/origin/canary");
                assert_eq!(evidence.creation_sha, integration);
                // An unavailable configured ref must not fall back to unrelated main.
                git(&repo, &["branch", "-D", "canary"]);
                assert_eq!(
                    ensure_worktree(&spec).unwrap(),
                    WorktreeOutcome::Reused(path)
                );
                let missing = WorktreeSpec {
                    card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                    branch: "missing",
                    ..spec.clone()
                };
                assert!(ensure_worktree(&missing)
                    .unwrap_err()
                    .contains("refresh integration base"));
                assert!(!worktree_path_for(missing.card_id).unwrap().exists());
                let unknown = WorktreeSpec {
                    card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                    repo: &RepoId::new("Example/unknown").unwrap(),
                    branch: "unknown",
                    ..spec
                };
                assert!(ensure_worktree(&unknown)
                    .unwrap_err()
                    .contains("supply start_point"));
                assert!(!worktree_path_for(unknown.card_id).unwrap().exists());
                // Generic repositories use their recorded remote default, even
                // when it has an unfamiliar name and the clone is on main.
                git(&repo, &["branch", "trunk/custom", &integration]);
                git(
                    &repo,
                    &[
                        "update-ref",
                        "refs/remotes/origin/trunk/custom",
                        &old_integration,
                    ],
                );
                git(
                    &repo,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/trunk/custom",
                    ],
                );
                let generic = ensure_worktree(&unknown).unwrap().path().to_owned();
                assert_eq!(resolve_commit(&generic, "HEAD").unwrap(), integration);
                let evidence = allocation_evidence(&generic).unwrap().unwrap();
                assert_eq!(evidence.integration_base.as_deref(), Some("trunk/custom"));
                assert_eq!(evidence.start_ref, "refs/remotes/origin/trunk/custom");
                // A known policy must not quietly use that generic default if
                // its configured branch is absent.
                assert!(ensure_worktree(&missing)
                    .unwrap_err()
                    .contains("refresh integration base"));
            },
        );
    }

    #[test]
    fn shared_git_runner_drains_output_while_writing_finite_input() {
        use airc_work::local_git::CommandGitRunner;
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        // Both pipes exceed ordinary platform pipe capacity. Writing all stdin
        // before reading stdout would deadlock this real Git batch operation.
        let request = "nonexistent-object\n".repeat(32768);
        let expected = "nonexistent-object missing\n".repeat(32768);
        let result = CommandGitRunner
            .run_git_input(
                tmp.path(),
                &["cat-file", "--batch"],
                Some(request.as_bytes()),
            )
            .unwrap();
        assert_eq!(result, expected.as_bytes());
        let error = CommandGitRunner
            .run_git_input(
                tmp.path(),
                &[
                    "-c",
                    "alias.reject-input=!echo retained-output; echo rejected-input >&2; exit 7",
                    "reject-input",
                ],
                Some(request.as_bytes()),
            )
            .unwrap_err();
        match error {
            airc_work::local_git::LocalGitError::GitCommand {
                output,
                input_error,
                ..
            } => {
                assert_eq!(output.status.code(), Some(7));
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap().trim(),
                    "retained-output"
                );
                assert!(String::from_utf8(output.stderr)
                    .unwrap()
                    .contains("rejected-input"));
                assert!(
                    input_error.is_some(),
                    "the rejected large input must remain visible beside exit/output"
                );
            }
            other => panic!("lost completed process outcome: {other:?}"),
        }
    }

    // what this catches: `git worktree add` leaving submodules EMPTY, so a lease
    // worktree cannot build the thing it was leased for. In continuum that surfaces
    // as a CMake error about a missing CMakeLists.txt under `core/vendor/llama.cpp`
    // — naming a path the reader never chose, with nothing pointing at submodules —
    // and because the fleet shares one CARGO_TARGET_DIR by doctrine, the failed
    // build then poisons the operator's MAIN checkout too. Measured 2026-09-04 on
    // two machines (IntelMac, and M5 three times).
    //
    // Uses a second local repo as the submodule source: real git submodule
    // machinery, no network. `protocol.file.allow=always` because git refuses local
    // file transports for submodules by default (CVE-2022-39253).
    #[test]
    fn a_created_worktree_has_its_submodules_populated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let sub = tmp.path().join("subrepo");
        let main = tmp.path().join("mainrepo");
        for d in [&home, &sub, &main] {
            std::fs::create_dir_all(d).expect("mkdir");
        }

        init_repo(&sub);
        std::fs::write(sub.join("vendored.txt"), b"i am the submodule\n").expect("write");
        git(&sub, &["add", "."]);
        git(&sub, &["commit", "-qm", "vendored"]);

        init_repo(&main);
        git(
            &main,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().expect("utf8 sub path"),
                "vendor/dep",
            ],
        );
        git(&main, &["commit", "-qm", "add submodule"]);

        temp_env::with_var("HOME", Some(&home), || {
            // Local file transport must be allowed for the recursive init too.
            temp_env::with_var("GIT_ALLOW_PROTOCOL", Some("file"), || {
                let spec = WorktreeSpec {
                    card_id: card_id(),
                    repo: &RepoId::new("Example/repo").unwrap(),
                    clone_path: &main,
                    branch: "lease-branch",
                    start_point: Some("main"),
                };
                let outcome = ensure_worktree(&spec).expect("worktree created");
                let path = match outcome {
                    WorktreeOutcome::Created(p) => p,
                    other => panic!("expected Created, got {other:?}"),
                };
                assert!(
                    path.join("vendor/dep/vendored.txt").exists(),
                    "the submodule must be POPULATED in the lease worktree, not an \
                     empty directory — an empty one is what breaks the build and \
                     poisons the shared target dir. Contents: {:?}",
                    std::fs::read_dir(path.join("vendor/dep")).map(|d| d
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name())
                        .collect::<Vec<_>>())
                );
            });
        });
    }
}
