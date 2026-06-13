//! Card 1eae6f3e: the scope's default room is DURABLE scope state.
//!
//! Observed 3+ times in 36h: after `airc init` or a daemon bounce the
//! scope's default room silently flipped `#cambriantech` → `#general`,
//! and the next `airc msg` misrouted. Root cause: `ensure_join_context`
//! treated the cwd-inferred context default as authoritative on EVERY
//! run. Recovery paths (resume/update skills, monitor reattach, init
//! re-runs) invoke bare `airc join` — often from a cwd with no git
//! checkout, where `JoinContext::from_cwd` infers `#general` — and the
//! re-run demoted the durable default.
//!
//! Contract pinned here:
//! 1. Re-running `join_default_context` from a non-repo cwd must NOT
//!    flip an existing default.
//! 2. Re-running it with the SAME context is idempotent for default.
//! 3. Reopening the scope (daemon bounce / `airc init` re-run)
//!    preserves the default.
//! 4. FIRST entry into a project context still promotes the project
//!    room over a lazily-seeded `#general` (the fix must not
//!    overcorrect into "never set the default").
//!
//! All scopes use isolated homes + wire roots (hermetic; no shared
//! machine state).

use std::path::Path;

use airc_lib::airc::Airc;
use tempfile::TempDir;

const REPO_ORIGIN: &str = "https://github.com/CambrianTech/airc.git";

/// A fake checkout whose origin owner resolves to `cambriantech`.
fn repo_dir() -> TempDir {
    let dir = TempDir::new().expect("create repo tempdir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("create .git");
    std::fs::write(
        dir.path().join(".git/config"),
        format!(
            r#"[core]
    repositoryformatversion = 0
[remote "origin"]
    url = {REPO_ORIGIN}
"#
        ),
    )
    .expect("write git config");
    dir
}

async fn open_scope(home: &Path, wire_root: &Path) -> Airc {
    Airc::open_with_wire_root_for_test(home, wire_root)
        .await
        .expect("open scope")
}

async fn default_channel(airc: &Airc) -> Option<String> {
    let set = airc.subscription_set().await.expect("load subscriptions");
    set.default.map(|name| name.as_str().to_string())
}

/// THE BUG: bare `airc join` re-run from a cwd outside any git
/// checkout (daemon-bounce recovery, monitor resume, init re-run)
/// infers a `#general`-only context and silently demoted the durable
/// default. It must not.
#[tokio::test]
async fn rejoin_from_non_repo_cwd_must_not_flip_default() {
    let home = TempDir::new().expect("home");
    let wire_root = TempDir::new().expect("wire root");
    let repo = repo_dir();
    let non_repo = TempDir::new().expect("non-repo cwd");

    let airc = open_scope(home.path(), wire_root.path()).await;
    airc.join_default_context(repo.path())
        .await
        .expect("join from repo cwd");
    assert_eq!(
        default_channel(&airc).await.as_deref(),
        Some("cambriantech"),
        "first join inside the repo makes the project room default"
    );

    airc.join_default_context(non_repo.path())
        .await
        .expect("rejoin from non-repo cwd");
    assert_eq!(
        default_channel(&airc).await.as_deref(),
        Some("cambriantech"),
        "re-running join from a non-repo cwd must NOT flip the durable \
         default to #general — this is the silent-misroute bug (card 1eae6f3e)"
    );
}

/// Same context twice: pure idempotence — default and subscription set
/// unchanged.
#[tokio::test]
async fn rejoin_same_context_is_idempotent_for_default() {
    let home = TempDir::new().expect("home");
    let wire_root = TempDir::new().expect("wire root");
    let repo = repo_dir();

    let airc = open_scope(home.path(), wire_root.path()).await;
    airc.join_default_context(repo.path())
        .await
        .expect("first join");
    let before = airc.subscription_set().await.expect("set before");

    airc.join_default_context(repo.path())
        .await
        .expect("second join");
    let after = airc.subscription_set().await.expect("set after");

    assert_eq!(
        before.default, after.default,
        "idempotent re-join must not move the default"
    );
    assert_eq!(
        before.subscribed.keys().collect::<Vec<_>>(),
        after.subscribed.keys().collect::<Vec<_>>(),
        "idempotent re-join must not change the subscription set"
    );
}

/// Daemon bounce / `airc init` re-run: a fresh handle over the same
/// scope store sees the same default, and a recovery re-join from a
/// bare cwd still doesn't demote it.
#[tokio::test]
async fn reopen_scope_preserves_default_across_bounce() {
    let home = TempDir::new().expect("home");
    let wire_root = TempDir::new().expect("wire root");
    let repo = repo_dir();
    let non_repo = TempDir::new().expect("non-repo cwd");

    {
        let airc = open_scope(home.path(), wire_root.path()).await;
        airc.join_default_context(repo.path())
            .await
            .expect("join from repo cwd");
        assert_eq!(
            default_channel(&airc).await.as_deref(),
            Some("cambriantech")
        );
    } // handle dropped — the "daemon died" half of the bounce

    let reopened = open_scope(home.path(), wire_root.path()).await;
    assert_eq!(
        default_channel(&reopened).await.as_deref(),
        Some("cambriantech"),
        "default must survive a scope reopen (daemon bounce / init re-run)"
    );
    let current = reopened.current_room().await.expect("current room");
    assert_eq!(
        current.name, "cambriantech",
        "current_room (what `airc msg` targets) must match the durable default"
    );

    // The typical post-bounce recovery: bare `airc join` from $HOME.
    reopened
        .join_default_context(non_repo.path())
        .await
        .expect("recovery rejoin");
    assert_eq!(
        default_channel(&reopened).await.as_deref(),
        Some("cambriantech"),
        "post-bounce recovery join must not demote the default"
    );
}

/// Guard against overcorrection: the FIRST entry into a project
/// context must still promote the project room, even when `#general`
/// was already lazily seeded as default (fresh scope that sent a
/// message before ever joining a repo context).
#[tokio::test]
async fn first_repo_join_promotes_project_room_over_seeded_general() {
    let home = TempDir::new().expect("home");
    let wire_root = TempDir::new().expect("wire root");
    let repo = repo_dir();

    let airc = open_scope(home.path(), wire_root.path()).await;
    let seeded = airc.current_room().await.expect("lazy seed");
    assert_eq!(seeded.name, "general", "fresh scope seeds #general");
    assert_eq!(default_channel(&airc).await.as_deref(), Some("general"));

    airc.join_default_context(repo.path())
        .await
        .expect("first repo join");
    assert_eq!(
        default_channel(&airc).await.as_deref(),
        Some("cambriantech"),
        "first entry into a project context still promotes the project room"
    );
}
