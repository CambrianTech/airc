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

/// The lifecycle lock for one card's worktree: publishing a claim or a state change and
/// creating/reusing the worktree, versus retirement's board re-read and removal, are
/// mutually exclusive on this machine (card bac19e3c). It serializes LIFECYCLE steps only:
/// a process already working inside the worktree holds nothing, so this is not custody of
/// the files, and retirement must still prove the tree quiescent on its own.
///
/// Keyed by the FULL card id, never the shown prefix two cards can share. The file lives in
/// `~/.airc/worktrees/.locks/`, outside every worktree, so removing a worktree never deletes
/// a lock another process holds (`.locks` is not a short id, so cleanup's scan skips it).
#[derive(Debug)]
pub struct WorktreeLifecycleLock {
    _file: std::fs::File,
}

fn lifecycle_lock_file(card_id: WorkCardId) -> Result<std::fs::File, String> {
    let dir = worktree_root()
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?
        .join(".locks");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{card_id}.lock"));
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))
}

/// Take the card's lifecycle lock, waiting for a holder to finish. Blocking: async callers
/// run it on the blocking pool.
pub fn lifecycle_lock(card_id: WorkCardId) -> Result<WorktreeLifecycleLock, String> {
    use fs2::FileExt;
    let file = lifecycle_lock_file(card_id)?;
    file.lock_exclusive()
        .map_err(|e| format!("lock worktree lifecycle of {card_id}: {e}"))?;
    Ok(WorktreeLifecycleLock { _file: file })
}

/// Take the card's lifecycle lock only if nobody holds it: `None` means a claim or a state
/// change is in flight, and a retirement must retain rather than wait on it.
pub fn try_lifecycle_lock(card_id: WorkCardId) -> Result<Option<WorktreeLifecycleLock>, String> {
    use fs2::FileExt;
    let file = lifecycle_lock_file(card_id)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(WorktreeLifecycleLock { _file: file })),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
        Err(e) => Err(format!("lock worktree lifecycle of {card_id}: {e}")),
    }
}

/// What [`ensure_worktree`] did. Reuse is a success, not a no-op failure:
/// re-claiming a card an agent is already working must not disturb the work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOutcome {
    Created(PathBuf),
    Reused(PathBuf),
    /// A held linked worktree (the pre-d166e4f2 shape) was replaced by a card clone on the
    /// same branch and HEAD; the old tree was moved aside, whole, to `retired`.
    Migrated {
        path: PathBuf,
        retired: PathBuf,
    },
}

impl WorktreeOutcome {
    pub fn path(&self) -> &Path {
        match self {
            WorktreeOutcome::Created(p)
            | WorktreeOutcome::Reused(p)
            | WorktreeOutcome::Migrated { path: p, .. } => p,
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

/// The commit a new card checkout starts from, resolved ONCE in the source clone: the
/// integration base the evidence records, the ref it came from, and the immutable sha.
/// Shared by [`ensure_worktree`] and [`ensure_card_clone`], so both allocate from the same
/// base by the same rule.
fn resolve_allocation(spec: &WorktreeSpec<'_>) -> Result<(Option<String>, String, String), String> {
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
        // Only the remote-tracking ref follows upstream rewrites. Existing
        // worktrees and their immutable creation anchors are never reset.
        let refspec = format!("+refs/heads/{branch}:{start_ref}");
        CommandGitRunner
            .run_git(spec.clone_path, &["fetch", "--no-tags", "origin", &refspec])
            .map_err(|e| format!("refresh integration base before allocation: {e}"))?;
    }
    let base = resolve_commit(spec.clone_path, &start_ref)?;
    Ok((integration_base, start_ref, base))
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

    let (integration_base, start_ref, base) = resolve_allocation(spec)?;
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

/// Where this node's mirror of `repo` lives: `~/.airc/mirrors/<owner>/<repo>.git`. A pure
/// function of the repo, like [`worktree_path_for`] is of the card (card d166e4f2).
pub fn mirror_path_for(repo: &RepoId) -> Option<PathBuf> {
    home_dir().map(|home| {
        home.join(".airc")
            .join("mirrors")
            .join(format!("{}.git", repo.as_str()))
    })
}

/// The bound on one git call made while staging a card (card d166e4f2). Measured on
/// IntelMac 2026-10-17T14:49:59Z: an incremental `fetch --prune origin` against GitHub took
/// 1.26 s, and a full `clone --mirror --no-hardlinks` of the continuum repo (206 MB of
/// objects) took 1.21 s. 120 s is ~95x the slowest of those, headroom for a cold DNS/TLS
/// handshake on the slowest tier. A hang (an auth prompt, a stalled remote) is killed and
/// reported, never waited out on a resident's claim path.
const GIT_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// Run `git` in `dir`, bounded by [`GIT_BOUND`], never prompting (`GIT_TERMINAL_PROMPT=0`,
/// stdin closed). Returns trimmed stdout, or the failure with its stderr or the timeout.
fn git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let shown = || format!("git {} in {}", args.join(" "), dir.display());
    let mut command = airc_core::process::background("git");
    command
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = airc_core::process::run_bounded(command, GIT_BOUND)
        .map_err(|error| format!("{} {error}", shown()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}",
            shown(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Is `path` a LINKED worktree (its `.git` a file naming a gitdir elsewhere), as card
/// checkouts were before card d166e4f2? A card clone's `.git` is a directory.
/// [`ensure_card_clone`] asks this to decide whether a held card still needs migrating.
pub fn is_linked_worktree(path: &Path) -> bool {
    path.join(".git").is_file()
}

/// The node's bare mirror of `repo`: created once from `source` (a local clone of it), then
/// refreshed from the REAL remote on every call. Card clones borrow its objects read-only
/// ([`ensure_card_clone`]), so a card checkout never shares a writable file with the deploy
/// checkout or with another card.
///
/// - `--no-hardlinks`: the mirror shares no inode with `source`. A hard link would make a
///   write in the mirror a write in the deploy repo's pack (Opus on d166e4f2).
/// - `extensions.preciousObjects` + `gc.auto 0`: git refuses any prune or gc that could drop
///   an object, because card clones still reference it through their alternates. A dropped
///   object would corrupt those clones silently.
/// - `origin` is `source`'s own origin URL, so fetches come from the real remote and never
///   from the deploy checkout's local branches.
/// - It is first cloned FROM `source`, so it briefly holds `source`'s local branches (an
///   operator's or agent's WIP). The prune-fetch drops every ref the real remote lacks, and
///   a new mirror whose first fetch fails is removed, so no local branch can become a base.
pub fn ensure_mirror(repo: &RepoId, source: &Path) -> Result<PathBuf, String> {
    let path = mirror_path_for(repo)
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/mirrors/".to_string())?;
    let created = !path.exists();
    if created {
        let parent = path
            .parent()
            .ok_or_else(|| format!("mirror path {} has no parent", path.display()))?;
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        let origin = git_in(source, &["remote", "get-url", "origin"])?;
        let source_str = source.to_string_lossy().to_string();
        let path_str = path.to_string_lossy().to_string();
        git_in(
            parent,
            &[
                "clone",
                "--quiet",
                "--mirror",
                "--no-hardlinks",
                &source_str,
                &path_str,
            ],
        )?;
        git_in(&path, &["remote", "set-url", "origin", &origin])?;
        git_in(&path, &["config", "core.repositoryformatversion", "1"])?;
        git_in(&path, &["config", "extensions.preciousObjects", "true"])?;
        git_in(&path, &["config", "gc.auto", "0"])?;
    }
    // Refs follow the real remote; objects are never pruned (preciousObjects).
    if let Err(error) = git_in(&path, &["fetch", "--quiet", "--prune", "origin"]) {
        if created {
            // A new mirror still holds the SOURCE's local branches until this prune-fetch
            // succeeds. Keep none of them as a possible card base: remove what was just made.
            let _ = std::fs::remove_dir_all(&path);
            return Err(error);
        }
        // An existing mirror stages from the refs it already has; the base is still resolved
        // against them, and the staleness is said, not hidden.
        tracing::warn!(mirror = %path.display(), %error, "repo mirror refresh failed; staging from its existing refs");
    }
    Ok(path)
}

/// Create the card's checkout as its OWN repository, or report that it already exists.
///
/// The checkout lives at the same [`worktree_path_for`] path a worktree would, with its
/// `.git` a directory INSIDE it (card d166e4f2, Joel: "do it right" instead of #5116's grant
/// of the deploy repo's .git). Objects are borrowed from `spec.clone_path`, which must be
/// the node's [`ensure_mirror`] of the repo, through `--shared` alternates. A confined
/// resident then needs exactly one READ-only root (`<mirror>/objects`); every write she
/// makes, objects included, lands in her own `.git`.
///
/// `origin` is set to the mirror's origin, the real remote, so the act-end carry pushes
/// (a local-path origin never carries).
pub fn ensure_card_clone(spec: &WorktreeSpec<'_>) -> Result<WorktreeOutcome, String> {
    let path = worktree_path_for(spec.card_id)
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    if path.exists() {
        if is_linked_worktree(&path) {
            match migrate_linked_checkout(spec.clone_path, &path) {
                Ok(Some(retired)) => return Ok(WorktreeOutcome::Migrated { path, retired }),
                Ok(None) => {}
                // The old tree is untouched on every error path, and its file tools still work,
                // so the resident keeps it; the reason is said, not hidden.
                Err(error) => tracing::warn!(
                    checkout = %path.display(), %error,
                    "linked card checkout could not be migrated to a card clone; reusing it"
                ),
            }
        }
        return Ok(WorktreeOutcome::Reused(path));
    }
    let root = path
        .parent()
        .ok_or_else(|| format!("checkout path {} has no parent", path.display()))?;
    std::fs::create_dir_all(root)
        .map_err(|e| format!("create lease zone {}: {e}", root.display()))?;
    let (integration_base, start_ref, base) = resolve_allocation(spec)?;
    let origin = git_in(spec.clone_path, &["remote", "get-url", "origin"])?;
    let mirror_str = spec.clone_path.to_string_lossy().to_string();
    let path_str = path.to_string_lossy().to_string();
    git_in(
        root,
        &[
            "clone",
            "--quiet",
            "--shared",
            "--no-checkout",
            &mirror_str,
            &path_str,
        ],
    )?;
    git_in(&path, &["remote", "set-url", "origin", &origin])?;
    git_in(&path, &["checkout", "--quiet", "-b", spec.branch, &base])?;
    let evidence = AllocationEvidence {
        repo: spec.repo.clone(),
        requested_branch: spec.branch.to_owned(),
        integration_base,
        start_ref,
        creation_sha: base,
    };
    allocation::record(&path, &evidence)?;
    init_submodules(&path)?;
    Ok(WorktreeOutcome::Created(path))
}

/// Replace a held linked worktree with a card clone, when nothing can be lost by doing so.
///
/// Opus measured on the M5, 2026-10-17: all 93 checkouts under `~/.airc/worktrees` were linked
/// worktrees, including every card a resident held. Reusing them would keep their gitdir in the
/// source repo's `.git`, which a confined resident cannot write, so her git would fail again.
///
/// It migrates only a tree that is clean, on a branch, and whose HEAD is on the real remote
/// (reachable from an `origin/*` ref of the fresh clone). Then:
/// 1. clone from `mirror` into `.migrating/<short>`, check out the same branch at the same HEAD,
///    and carry the allocation evidence (creation base included) over;
/// 2. move the old tree, whole, to `.retired/<short>-<unix secs>`, then the clone into place.
///    If the second move fails, the old tree is moved back.
///
/// Nothing is deleted except our own half-made staging clone, and the source repo is never
/// written: its `.git/worktrees/<id>` entry becomes prunable metadata. `Ok(None)` means kept,
/// with the reason logged.
fn migrate_linked_checkout(mirror: &Path, path: &Path) -> Result<Option<PathBuf>, String> {
    let keep = |reason: &str| {
        tracing::warn!(
            checkout = %path.display(), reason,
            "linked card checkout kept: its git still needs the source repo's .git"
        );
        Ok(None)
    };
    if !git_in(path, &["status", "--porcelain"])?.is_empty() {
        return keep("uncommitted changes");
    }
    let Ok(branch) = git_in(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]) else {
        return keep("detached HEAD");
    };
    let head = git_in(path, &["rev-parse", "HEAD"])?;
    let evidence = allocation_evidence(path)?;
    let creation = resolve_commit(path, CREATION_BASE_REF).ok();
    let (zone, name) = match (path.parent(), path.file_name()) {
        (Some(zone), Some(name)) => (zone, name.to_string_lossy().into_owned()),
        _ => return Err(format!("checkout path {} has no parent", path.display())),
    };
    let staged = zone.join(".migrating").join(&name);
    let discard = |staged: &Path| {
        if staged.exists() {
            // Only ever our own staging clone, never a checkout anyone worked in.
            let _ = std::fs::remove_dir_all(staged);
        }
    };
    discard(&staged);
    std::fs::create_dir_all(zone.join(".migrating"))
        .map_err(|e| format!("create {}: {e}", zone.join(".migrating").display()))?;
    let staged_str = staged.to_string_lossy().into_owned();
    let mirror_str = mirror.to_string_lossy().into_owned();
    let prepared = (|| -> Result<bool, String> {
        git_in(
            zone,
            &[
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                &mirror_str,
                &staged_str,
            ],
        )?;
        let pushed = git_in(
            &staged,
            &[
                "for-each-ref",
                "--count=1",
                "--contains",
                &head,
                "refs/remotes/origin",
            ],
        )
        .unwrap_or_default();
        if pushed.is_empty() {
            return Ok(false);
        }
        let origin = git_in(mirror, &["remote", "get-url", "origin"])?;
        git_in(&staged, &["remote", "set-url", "origin", &origin])?;
        git_in(&staged, &["checkout", "--quiet", "-b", &branch, &head])?;
        let upstream = format!("origin/{branch}");
        if git_in(&staged, &["rev-parse", "--verify", "--quiet", &upstream]).is_ok() {
            git_in(
                &staged,
                &["branch", "--quiet", "--set-upstream-to", &upstream],
            )?;
        }
        match (&evidence, &creation) {
            (Some(evidence), _) => allocation::record(&staged, evidence)?,
            (None, Some(base)) => {
                git_in(&staged, &["update-ref", CREATION_BASE_REF, base, ""])?;
            }
            // A legacy tree with no anchor stays without one: never backfill from HEAD.
            (None, None) => {}
        }
        init_submodules(&staged)?;
        Ok(true)
    })();
    match prepared {
        Ok(true) => {}
        Ok(false) => {
            discard(&staged);
            return keep("HEAD is not on the remote");
        }
        Err(error) => {
            discard(&staged);
            return Err(error);
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let retired = zone.join(".retired").join(format!("{name}-{stamp}"));
    std::fs::create_dir_all(zone.join(".retired"))
        .map_err(|e| format!("create {}: {e}", zone.join(".retired").display()))?;
    std::fs::rename(path, &retired).map_err(|e| {
        discard(&staged);
        format!(
            "move {} aside to {}: {e}",
            path.display(),
            retired.display()
        )
    })?;
    if let Err(e) = std::fs::rename(&staged, path) {
        let restored = std::fs::rename(&retired, path);
        discard(&staged);
        return Err(format!(
            "move card clone into {}: {e}; old tree restored: {}",
            path.display(),
            restored.is_ok()
        ));
    }
    Ok(Some(retired))
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

    // what this catches (card bac19e3c): claim/reopen and worktree retirement were not
    // exclusive, so a retirement's board read could be overtaken by a new claim before it
    // removed the tree. The lifecycle lock makes them exclusive per card: while one holder has
    // it, a retirement's try-lock sees it held; once released it is free. It is keyed by the
    // FULL card id, so two cards sharing an 8-hex prefix never contend.
    #[test]
    fn the_lifecycle_lock_is_exclusive_per_full_card_id() {
        let tmp = tempfile::tempdir().unwrap();
        temp_env::with_var("HOME", Some(tmp.path()), || {
            let a = WorkCardId::from_u128(0xabcd_ef01_0000_0000_0000_0000_0000_0001);
            let b = WorkCardId::from_u128(0xabcd_ef01_0000_0000_0000_0000_0000_0002);
            assert_eq!(
                short_id(a),
                short_id(b),
                "fixture: the two ids share a short id"
            );
            let held = lifecycle_lock(a).expect("first holder takes it");
            assert!(
                try_lifecycle_lock(a).unwrap().is_none(),
                "held: a retirement must not take it"
            );
            assert!(
                try_lifecycle_lock(b).unwrap().is_some(),
                "same prefix, different card: free"
            );
            drop(held);
            assert!(
                try_lifecycle_lock(a).unwrap().is_some(),
                "released: free again"
            );
            let locks = tmp.path().join(LEASE_ZONE_RELATIVE).join(".locks");
            assert!(
                locks.join(format!("{a}.lock")).exists(),
                "outside every worktree"
            );
            assert!(
                worktree_path_for(a).unwrap().parent() == locks.parent(),
                "beside the worktrees"
            );
        });
    }

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
                // Upstream can rewrite its integration history. A new allocation
                // follows that non-FF move while the prior allocation stays pinned.
                git(&repo, &["branch", "-f", "canary", &main]);
                let rewritten_spec = WorktreeSpec {
                    card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                    branch: "after-upstream-rewrite",
                    ..spec.clone()
                };
                let rewritten = ensure_worktree(&rewritten_spec).unwrap().path().to_owned();
                assert_eq!(
                    resolve_commit(&repo, "refs/remotes/origin/canary").unwrap(),
                    main
                );
                assert_eq!(creation_base(&rewritten).unwrap(), main);
                assert_eq!(
                    allocation_evidence(&rewritten)
                        .unwrap()
                        .unwrap()
                        .creation_sha,
                    main
                );
                assert_eq!(resolve_commit(&path, "HEAD").unwrap(), integration);
                assert_eq!(creation_base(&path).unwrap(), integration);
                assert_eq!(resolve_commit(&repo, "HEAD").unwrap(), main);
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

    // what this catches (card d3c914d8): the shared runner let git prompt for
    // credentials, so a claim's allocation fetch from a remote that wanted them
    // blocked on a terminal prompt nobody would answer. Git itself reports what it sees.
    #[test]
    fn shared_git_runner_never_prompts_for_credentials() {
        use airc_work::local_git::{CommandGitRunner, GitCommandRunner};
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let seen = CommandGitRunner
            .run_git(
                tmp.path(),
                &[
                    "-c",
                    "alias.show-prompt=!printf %s \"$GIT_TERMINAL_PROMPT\"",
                    "show-prompt",
                ],
            )
            .unwrap();
        assert_eq!(seen, "0");
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

    // what this catches (card d166e4f2): a card checkout that writes into, or shares a
    // writable file with, the repo it was cut from. #5116 had to grant the DEPLOY repo's .git
    // because card worktrees were registered there. A card clone must own its .git, borrow
    // objects only from a disposable mirror, push to the real remote, never touch the source,
    // and survive the mirror losing refs.
    #[test]
    fn a_card_clone_owns_its_git_and_borrows_only_from_a_precious_mirror() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical on unix (/var -> /private/var on macOS, so paths git reports match ours).
        // NOT on Windows: canonicalize yields a \\?\ verbatim path that git cannot mkdir
        // (CI job 117425627237: "cannot mkdir \\?\C:\...: Invalid argument").
        let root = if cfg!(windows) {
            tmp.path().to_path_buf()
        } else {
            tmp.path().canonicalize().unwrap()
        };
        let git = |dir: &Path, args: &[&str]| git_in(dir, args).unwrap_or_else(|e| panic!("{e}"));
        let remote = root.join("remote.git");
        git(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=canary",
                remote.to_str().unwrap(),
            ],
        );
        let source = root.join("source");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
        );
        git(&source, &["config", "user.email", "t@t"]);
        git(&source, &["config", "user.name", "t"]);
        std::fs::write(source.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "base"]);
        git(&source, &["push", "--quiet", "origin", "HEAD:canary"]);
        // An agent's local WIP branch in the source, never pushed: it must not survive into
        // the mirror, or it could become a card base.
        git(&source, &["branch", "agent-wip"]);
        let source_refs = git(&source, &["for-each-ref"]);
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        temp_env::with_var("HOME", Some(&home), || {
            let repo = RepoId::new("o/r").unwrap();
            let mirror = ensure_mirror(&repo, &source).expect("mirror is created");
            assert_eq!(mirror, home.join(".airc/mirrors/o/r.git"));
            assert_eq!(
                git(&mirror, &["remote", "get-url", "origin"]),
                remote.to_str().unwrap(),
                "the mirror fetches from the real remote"
            );
            assert_eq!(
                git(&mirror, &["config", "extensions.preciousObjects"]),
                "true"
            );
            assert!(
                git_in(&mirror, &["rev-parse", "--verify", "refs/heads/agent-wip"]).is_err(),
                "the source's local-only branch is pruned from the mirror"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let inodes = |dir: &Path| -> std::collections::HashSet<u64> {
                    walkdir_files(dir)
                        .into_iter()
                        .map(|f| std::fs::metadata(f).unwrap().ino())
                        .collect()
                };
                assert!(
                    inodes(&source.join(".git/objects"))
                        .is_disjoint(&inodes(&mirror.join("objects"))),
                    "--no-hardlinks: the mirror shares no inode with the source"
                );
            }

            let card = WorkCardId::from_u128(0xd166_e4f2_0000_0000_0000_0000_0000_0001);
            let branch = "d166e4f2/fix";
            let spec = WorktreeSpec {
                card_id: card,
                repo: &repo,
                clone_path: &mirror,
                branch,
                start_point: Some("refs/heads/canary"),
            };
            let path = ensure_card_clone(&spec)
                .expect("card clone is created")
                .path()
                .to_path_buf();
            assert_eq!(path, worktree_path_for(card).unwrap());
            assert!(
                path.join(".git").is_dir(),
                "the checkout owns its .git: a directory, never a gitdir file"
            );
            assert!(
                !is_linked_worktree(&path),
                "a card clone is not a linked worktree"
            );
            let alternates =
                std::fs::read_to_string(path.join(".git/objects/info/alternates")).unwrap();
            assert_eq!(
                Path::new(alternates.trim()).canonicalize().unwrap(),
                mirror.join("objects").canonicalize().unwrap(),
                "it borrows from exactly the mirror's objects"
            );
            assert_eq!(
                git(&path, &["remote", "get-url", "origin"]),
                remote.to_str().unwrap(),
                "origin is the real remote, so the carry pushes"
            );
            assert_eq!(git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]), branch);

            git(&path, &["config", "user.email", "t@t"]);
            git(&path, &["config", "user.name", "t"]);
            std::fs::write(path.join("lib.rs"), "fn a() { fixed() }\n").unwrap();
            git(&path, &["commit", "--quiet", "-am", "the fix"]);
            let fix = git(&path, &["rev-parse", "HEAD"]);
            git(&path, &["push", "--quiet", "-u", "origin", "HEAD"]);
            assert_eq!(
                git(&remote, &["rev-parse", branch]),
                fix,
                "the push lands on the real remote"
            );
            assert!(
                git_in(&mirror, &["cat-file", "-e", &format!("{fix}^{{commit}}")]).is_err(),
                "her new objects live in her own .git, not the mirror"
            );
            assert_eq!(
                git(&source, &["for-each-ref"]),
                source_refs,
                "the source repo is never written"
            );
            assert!(
                !source.join(".git/worktrees").exists(),
                "no worktree is registered in the source"
            );
            assert!(matches!(
                ensure_card_clone(&spec),
                Ok(WorktreeOutcome::Reused(_))
            ));
            // A legacy card checkout (a linked worktree, .git a FILE) is told apart, so card
            // staging can migrate it instead of reusing a checkout git can't use confined.
            let legacy = root.join("legacy");
            git(
                &source,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "legacy",
                    legacy.to_str().unwrap(),
                ],
            );
            assert!(is_linked_worktree(&legacy));
            git(&source, &["worktree", "remove", legacy.to_str().unwrap()]);
            git(&source, &["branch", "-D", "legacy"]);

            // The mirror loses the ref her base came from; prune and pack deletion are both refused,
            // so her clone still reads its history through the alternates.
            let base = git(&path, &["rev-parse", "HEAD~1"]);
            git(&mirror, &["update-ref", "-d", "refs/heads/canary"]);
            assert!(
                git_in(&mirror, &["prune"]).is_err(),
                "preciousObjects refuses a prune"
            );
            assert!(
                git_in(&mirror, &["repack", "-a", "-d", "-k", "--quiet"]).is_err(),
                "preciousObjects refuses deleting packs too, so no repack can drop her objects"
            );
            assert_eq!(
                git(&path, &["log", "--format=%H", "-1", &base]),
                base,
                "her base is still readable"
            );
        });
    }

    // what this catches (card 2209dee4, Opus on #5132): every checkout a resident held on the M5
    // was a linked worktree, and reusing one keeps her gitdir in the source repo's .git. A clean
    // tree whose HEAD is on the remote must become a card clone on the same branch and HEAD,
    // keeping its allocation evidence; a dirty or unpushed tree must be kept, untouched.
    #[test]
    fn a_held_linked_worktree_migrates_only_when_nothing_can_be_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let root = if cfg!(windows) {
            tmp.path().to_path_buf()
        } else {
            tmp.path().canonicalize().unwrap()
        };
        let git = |dir: &Path, args: &[&str]| git_in(dir, args).unwrap_or_else(|e| panic!("{e}"));
        let remote = root.join("remote.git");
        git(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=canary",
                remote.to_str().unwrap(),
            ],
        );
        let source = root.join("source");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
        );
        let who = |dir: &Path| {
            git(dir, &["config", "user.email", "t@t"]);
            git(dir, &["config", "user.name", "t"]);
        };
        who(&source);
        std::fs::write(source.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "--quiet", "-m", "base"]);
        git(&source, &["push", "--quiet", "origin", "HEAD:canary"]);
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        temp_env::with_var("HOME", Some(&home), || {
            let repo = RepoId::new("o/r").unwrap();
            let legacy = |n: u128, branch: &str| {
                let card = WorkCardId::from_u128((0x2209_dee0 + n) << 96);
                let spec = WorktreeSpec {
                    card_id: card,
                    repo: &repo,
                    clone_path: &source,
                    branch,
                    start_point: Some("origin/canary"),
                };
                let path = ensure_worktree(&spec).unwrap().path().to_path_buf();
                assert!(is_linked_worktree(&path));
                who(&path);
                std::fs::write(path.join("lib.rs"), format!("fn a() {{ {n} }}\n")).unwrap();
                git(&path, &["commit", "--quiet", "-am", "work"]);
                (card, path)
            };
            let (pushed_card, pushed) = legacy(1, "2209dee4/pushed");
            git(&pushed, &["push", "--quiet", "origin", "HEAD"]);
            let (unpushed_card, unpushed) = legacy(2, "2209dee4/unpushed");
            let (dirty_card, dirty) = legacy(3, "2209dee4/dirty");
            git(&dirty, &["push", "--quiet", "origin", "HEAD"]);
            std::fs::write(dirty.join("wip.txt"), "uncommitted").unwrap();
            let pushed_head = git(&pushed, &["rev-parse", "HEAD"]);
            let pushed_evidence = allocation_evidence(&pushed).unwrap().expect("recorded");
            // Staging refreshes the mirror before every checkout, so it has the pushes.
            let mirror = ensure_mirror(&repo, &source).unwrap();
            let stage = |card: WorkCardId, branch: &str| {
                ensure_card_clone(&WorktreeSpec {
                    card_id: card,
                    repo: &repo,
                    clone_path: &mirror,
                    branch,
                    start_point: None,
                })
                .unwrap()
            };

            let WorktreeOutcome::Migrated { path, retired } = stage(pushed_card, "2209dee4/pushed")
            else {
                panic!("a clean, pushed linked worktree migrates");
            };
            assert_eq!(path, pushed);
            assert!(!is_linked_worktree(&path), "it is a card clone now");
            assert_eq!(
                git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
                "2209dee4/pushed"
            );
            assert_eq!(git(&path, &["rev-parse", "HEAD"]), pushed_head, "same HEAD");
            assert_eq!(
                git(&path, &["status", "--porcelain"]),
                "",
                "checked out clean"
            );
            assert_eq!(
                git(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
                "origin/2209dee4/pushed"
            );
            assert_eq!(
                allocation_evidence(&path).unwrap(),
                Some(pushed_evidence),
                "the allocation evidence and creation base carry over"
            );
            assert_eq!(
                git(&path, &["remote", "get-url", "origin"]),
                remote.to_str().unwrap()
            );
            assert!(
                retired.starts_with(home.join(".airc/worktrees/.retired"))
                    && is_linked_worktree(&retired),
                "the old tree is moved aside whole, never deleted"
            );
            let staging_is_empty = || {
                std::fs::read_dir(home.join(".airc/worktrees/.migrating"))
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true)
            };
            assert!(staging_is_empty());
            assert!(matches!(
                stage(pushed_card, "2209dee4/pushed"),
                WorktreeOutcome::Reused(_)
            ));

            for (card, path, why) in [
                (unpushed_card, &unpushed, "a commit not on the remote"),
                (dirty_card, &dirty, "uncommitted work"),
            ] {
                assert_eq!(
                    stage(card, "x"),
                    WorktreeOutcome::Reused(path.clone()),
                    "kept: {why}"
                );
                assert!(is_linked_worktree(path), "untouched: {why}");
            }
            assert!(dirty.join("wip.txt").exists());
            assert!(staging_is_empty(), "no staging clone is left behind");
        });
    }

    #[cfg(unix)]
    fn walkdir_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p)
                } else {
                    out.push(p)
                }
            }
        }
        out
    }
}
