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

use airc_work::WorkCardId;

/// The lease zone, relative to the user's home: `~/.airc/worktrees`.
/// One source of truth for the location; `airc-cli`'s lease policy compares
/// against this same constant rather than spelling the path again.
pub const LEASE_ZONE_RELATIVE: &str = ".airc/worktrees";

/// How many hex chars of the card id name the worktree directory. Matches the
/// short id used in board output, so an operator reading the board can find
/// the directory by eye.
pub const SHORT_ID_LEN: usize = 8;

/// `~/.airc/worktrees`, or `None` when neither `$HOME` nor `$USERPROFILE` is
/// set. Environment, not cwd — a citizen has a home even with no checkout.
pub fn worktree_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join(LEASE_ZONE_RELATIVE))
}

/// The directory name for a card: the first [`SHORT_ID_LEN`] chars of its id.
pub fn short_id(card_id: WorkCardId) -> String {
    card_id.to_string().chars().take(SHORT_ID_LEN).collect()
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
    /// Local clone of the card's repo that the worktree branches from.
    pub clone_path: &'a Path,
    /// Branch to create for the work.
    pub branch: &'a str,
    /// Branch/commit the new branch starts from (e.g. `origin/canary`).
    /// `None` means git's default — the clone's current HEAD — which is what
    /// the operator CLI has always done from inside a checkout. A citizen,
    /// whose clone may be parked anywhere, should pass one explicitly.
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

    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(spec.clone_path)
        .args(["worktree", "add", "-b", spec.branch])
        .arg(path.as_os_str());
    if let Some(start) = spec.start_point {
        cmd.arg(start);
    }
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
    let out = std::process::Command::new("git")
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
        let out = std::process::Command::new("git")
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
