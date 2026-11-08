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
mod clone_staging;
mod default_ref;

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
                let remote_default = default_ref::origin_default(spec.clone_path)?;
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

/// The bound on a mirror's FIRST clone over the network (a submodule's, card 327e9b3e). Measured
/// IntelMac 2026-10-19T03:07Z: `clone --mirror` of CambrianTech/llama.cpp (453 MB) took 97.1 s,
/// already 81% of [`GIT_BOUND`], so a slower link would be killed and re-downloaded on every
/// staging. 600 s is ~6x that measurement: a bound kills a hang; it must not kill a slow link
/// doing real work. A refresh of the same mirror took 0.9 s and stays under [`GIT_BOUND`].
const NETWORK_MIRROR_BOUND: std::time::Duration = std::time::Duration::from_secs(600);

/// Run `git` in `dir`, bounded by [`GIT_BOUND`], never prompting (`GIT_TERMINAL_PROMPT=0`,
/// stdin closed). Returns trimmed stdout, or the failure with its stderr or the timeout.
fn git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_in_within(dir, args, GIT_BOUND)
}

/// [`git_in`] under an explicit bound, for the one operation measured to need longer
/// ([`NETWORK_MIRROR_BOUND`]).
fn git_in_within(dir: &Path, args: &[&str], bound: std::time::Duration) -> Result<String, String> {
    let shown = || format!("git {} in {}", args.join(" "), dir.display());
    let mut command = airc_core::process::background("git");
    command
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = airc_core::process::run_bounded(command, bound)
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
    // Created from the local clone (fast, no network); origin is its real remote.
    let origin = || git_in(source, &["remote", "get-url", "origin"]);
    ensure_mirror_from(repo, &source.to_string_lossy(), origin)
}

/// [`ensure_mirror`]'s one implementation: a new mirror is cloned from `clone_from` (a local
/// clone, or for a submodule its URL) and pointed at `origin()`; an existing one refreshes.
fn ensure_mirror_from(
    repo: &RepoId,
    clone_from: &str,
    origin: impl FnOnce() -> Result<String, String>,
) -> Result<PathBuf, String> {
    use fs2::FileExt;
    let path = mirror_path_for(repo)
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/mirrors/".to_string())?;
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(format!("mirror path {} has no parent", path.display()));
    };
    let name = name.to_string_lossy().into_owned();
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    // ONE creator or refresher per mirror at a time (Fable on #1591): residents staging at once
    // after a deploy would otherwise create the same mirror together, so one could borrow a
    // half-written mirror, or have its alternates removed by another's failed creation. The
    // file lock is held until this returns (the guard is the open file).
    let lock_path = parent.join(format!("{name}.lock"));
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
    lock.lock_exclusive()
        .map_err(|e| format!("lock {}: {e}", lock_path.display()))?;
    if !path.exists() {
        // Built under a temporary name and published by rename only once complete, so no
        // caller, and no crash midway, ever sees a half-made mirror at `path`. A failure
        // removes only this temporary directory, never a mirror anyone borrows from.
        let staging = parent.join(format!("{name}.creating"));
        let _ = std::fs::remove_dir_all(&staging); // only ever our own leftover, under the lock
        let made = (|| -> Result<(), String> {
            let origin = origin()?;
            let staging_str = staging.to_string_lossy().to_string();
            // From a local clone this is a disk copy; from a URL it is the whole history over
            // the network, measured to need far longer.
            let over_network = clone_from.contains("://") || clone_from.starts_with("git@");
            git_in_within(
                parent,
                &[
                    "clone",
                    "--quiet",
                    "--mirror",
                    "--no-hardlinks",
                    clone_from,
                    &staging_str,
                ],
                if over_network {
                    NETWORK_MIRROR_BOUND
                } else {
                    GIT_BOUND
                },
            )?;
            git_in(&staging, &["remote", "set-url", "origin", &origin])?;
            git_in(&staging, &["config", "core.repositoryformatversion", "1"])?;
            git_in(&staging, &["config", "extensions.preciousObjects", "true"])?;
            git_in(&staging, &["config", "gc.auto", "0"])?;
            // A new mirror still holds the SOURCE's local branches until this prune-fetch
            // succeeds; none of them may become a card base, so it must succeed before publish.
            git_in(&staging, &["fetch", "--quiet", "--prune", "origin"])?;
            std::fs::rename(&staging, &path)
                .map_err(|e| format!("publish mirror {}: {e}", path.display()))
        })();
        if made.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        return made.map(|()| path);
    }
    // Refs follow the real remote; objects are never pruned (preciousObjects).
    if let Err(error) = git_in(&path, &["fetch", "--quiet", "--prune", "origin"]) {
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
    // Resident callers do not already hold the CLI's lifecycle lock. Own it
    // here so staging/recovery and retirement cannot race.
    let _lifecycle = lifecycle_lock(spec.card_id)?;
    let path = worktree_path_for(spec.card_id)
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    let interrupted = clone_staging::interrupted_clone(&path);
    if path.exists() && !interrupted {
        // A killed clone can leave .git/HEAD -> .invalid. Preserve it, but
        // never report that partial allocation as a usable workspace.
        resolve_commit(&path, "HEAD").map_err(|error| {
            format!(
                "card checkout {} is incomplete; preserved for recovery: {error}",
                path.display()
            )
        })?;
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
        } else if allocation_evidence(&path)?.is_none() {
            return Err(format!(
                "card checkout {} has no completed allocation evidence; preserved for recovery",
                path.display()
            ));
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
    let staging = clone_staging::Staging::new(root, spec.card_id)?;
    let staged = staging.path();
    let path_str = staged.to_string_lossy().to_string();
    // Git ignores --shared/--local for shallow sources and transfers a pack.
    // The observed Continuum mirror transferred 1.58 GB, exceeding 120s.
    let clone_bound =
        if git_in(spec.clone_path, &["rev-parse", "--is-shallow-repository"])? == "true" {
            NETWORK_MIRROR_BOUND
        } else {
            GIT_BOUND
        };
    git_in_within(
        root,
        &[
            "clone",
            "--quiet",
            "--shared",
            "--no-checkout",
            &mirror_str,
            &path_str,
        ],
        clone_bound,
    )?;
    git_in(staged, &["remote", "set-url", "origin", &origin])?;
    git_in(staged, &["checkout", "--quiet", "-b", spec.branch, &base])?;
    let evidence = AllocationEvidence {
        repo: spec.repo.clone(),
        requested_branch: spec.branch.to_owned(),
        integration_base,
        start_ref,
        creation_sha: base,
    };
    init_submodules(staged)?;
    allocation::record(staged, &evidence)?;
    if interrupted {
        let retired = staging.recover(&path)?;
        return Ok(WorktreeOutcome::Migrated { path, retired });
    }
    staging.publish(&path)?;
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
    // A card clone borrows each GitHub submodule's objects from the node's own mirror of it,
    // exactly as it borrows the superproject's (card 327e9b3e): continuum's llama.cpp history
    // is 510 MB, and without this every new or migrated card clone downloaded it from GitHub
    // (measured IntelMac 2026-10-19). A submodule with no mirror (another host, a local
    // path, a mirror that could not be made) is initialised by the plain update below.
    for (sub_path, url) in declared_submodules(worktree) {
        let Some(repo) = github_repo_of(&url) else {
            continue;
        };
        let mirror = match ensure_mirror_from(&repo, &url, || Ok(url.clone())) {
            Ok(mirror) => mirror,
            Err(error) => {
                tracing::warn!(submodule = %sub_path, %error, "submodule mirror unavailable; it initialises from its URL");
                continue;
            }
        };
        let mirror = mirror.to_string_lossy().into_owned();
        if let Err(error) = git_in(
            worktree,
            &[
                "submodule",
                "update",
                "--init",
                "--reference",
                &mirror,
                "--",
                &sub_path,
            ],
        ) {
            tracing::warn!(submodule = %sub_path, %error, "submodule init from its mirror failed; it initialises from its URL");
        }
    }
    // Bounded like every git here (Fable on #1591): anything not borrowed from a mirror comes
    // over the network, so it takes the network bound, never an unbounded wait.
    if let Err(error) = git_in_within(
        worktree,
        &["submodule", "update", "--init", "--recursive"],
        NETWORK_MIRROR_BOUND,
    ) {
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
            error
        ));
    }
    Ok(())
}

/// Each submodule `checkout` declares in its `.gitmodules`, as (path, URL). None when it has
/// no `.gitmodules` (git exits non-zero on a missing file, which here means "no submodules").
fn declared_submodules(checkout: &Path) -> Vec<(String, String)> {
    let Ok(listed) = git_in(
        checkout,
        &[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.(path|url)$",
        ],
    ) else {
        return Vec::new();
    };
    let mut by_name: std::collections::BTreeMap<String, (Option<String>, Option<String>)> =
        std::collections::BTreeMap::new();
    for line in listed.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let Some(rest) = key.strip_prefix("submodule.") else {
            continue;
        };
        if let Some(name) = rest.strip_suffix(".path") {
            by_name.entry(name.to_owned()).or_default().0 = Some(value.to_owned());
        } else if let Some(name) = rest.strip_suffix(".url") {
            by_name.entry(name.to_owned()).or_default().1 = Some(value.to_owned());
        }
    }
    by_name
        .into_values()
        .filter_map(|(path, url)| Some((path?, url?)))
        .collect()
}

/// The GitHub `owner/name` a remote URL names, which keys its node mirror. `None` for any
/// other remote: those initialise from their URL as before.
fn github_repo_of(url: &str) -> Option<RepoId> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    RepoId::new(rest.strip_suffix(".git").unwrap_or(rest)).ok()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests;
