//! The production mirror-to-card path, with only local Git remotes.

use super::*;
use crate::work_worktree::{
    allocation_evidence, ensure_card_clone, ensure_mirror, git_in, resolve_commit, WorktreeSpec,
};
use airc_work::{RepoId, WorkCardId};

#[test]
fn mirror_allocation_uses_origins_default_not_the_source_checkout_head() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let git = |dir: &Path, args: &[&str]| git_in(dir, args).unwrap();
    let remote = root.join("remote.git");
    git(
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            remote.to_str().unwrap(),
        ],
    );
    let source = root.join("source");
    git(
        root,
        &[
            "clone",
            "--quiet",
            remote.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    );
    git(&source, &["config", "user.name", "test"]);
    git(&source, &["config", "user.email", "test@example.invalid"]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "main"],
    );
    git(&source, &["push", "--quiet", "origin", "HEAD:main"]);
    let main = resolve_commit(&source, "HEAD").unwrap();
    git(&source, &["checkout", "--quiet", "-b", "agent-work"]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "unpublished"],
    );
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    temp_env::with_vars(
        [("HOME", Some(home.as_os_str())), ("AIRC_PR_BASE", None)],
        || {
            let repo = RepoId::new("Example/mirror-default").unwrap();
            let mirror = ensure_mirror(&repo, &source).unwrap();
            // Mirror refresh prunes origin/HEAD inherited from the local clone.
            // Git versions differ on whether refresh also repairs mirror HEAD;
            // explicitly retain the stale source-agent HEAD for this regression.
            assert!(git_in(&mirror, &["symbolic-ref", "refs/remotes/origin/HEAD"]).is_err());
            git(&mirror, &["symbolic-ref", "HEAD", "refs/heads/agent-work"]);
            assert_eq!(
                git(&mirror, &["symbolic-ref", "HEAD"]),
                "refs/heads/agent-work"
            );
            let card = WorkCardId::from_uuid(uuid::Uuid::new_v4());
            let spec = WorktreeSpec {
                card_id: card,
                repo: &repo,
                clone_path: &mirror,
                branch: "card/first",
                start_point: None,
            };
            let first = ensure_card_clone(&spec).unwrap();
            assert_eq!(resolve_commit(first.path(), "HEAD").unwrap(), main);
            let evidence = allocation_evidence(first.path()).unwrap().unwrap();
            assert_eq!(evidence.integration_base.as_deref(), Some("main"));
            assert_eq!(evidence.start_ref, "refs/remotes/origin/main");
            assert_eq!(evidence.creation_sha, main);

            // A changed remote default must win over the stale mirror HEAD.
            git(&source, &["push", "--quiet", "origin", "HEAD:next"]);
            git(&remote, &["symbolic-ref", "HEAD", "refs/heads/next"]);
            let second = ensure_card_clone(&WorktreeSpec {
                card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                branch: "card/second",
                ..spec.clone()
            })
            .unwrap();
            assert_eq!(
                resolve_commit(second.path(), "HEAD").unwrap(),
                resolve_commit(&source, "HEAD").unwrap()
            );
            assert_eq!(
                allocation_evidence(second.path())
                    .unwrap()
                    .unwrap()
                    .integration_base
                    .as_deref(),
                Some("next")
            );
            assert_eq!(
                allocation_evidence(first.path()).unwrap().unwrap(),
                evidence
            );
            assert_eq!(resolve_commit(first.path(), "HEAD").unwrap(), main);

            // Explicit caller policy still bypasses default discovery entirely.
            git(&remote, &["symbolic-ref", "HEAD", "refs/heads/missing"]);
            assert!(origin_default(&mirror).is_err());
            git(&source, &["push", "--quiet", "origin", "main:canary"]);
            let configured_repo = RepoId::new("CambrianTech/continuum").unwrap();
            let configured = ensure_card_clone(&WorktreeSpec {
                card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                repo: &configured_repo,
                branch: "card/configured",
                ..spec.clone()
            })
            .unwrap();
            assert_eq!(resolve_commit(configured.path(), "HEAD").unwrap(), main);
            assert_eq!(
                allocation_evidence(configured.path())
                    .unwrap()
                    .unwrap()
                    .integration_base
                    .as_deref(),
                Some("canary")
            );
            let explicit = ensure_card_clone(&WorktreeSpec {
                card_id: WorkCardId::from_uuid(uuid::Uuid::new_v4()),
                branch: "card/explicit",
                start_point: Some(&main),
                ..spec
            })
            .unwrap();
            assert_eq!(resolve_commit(explicit.path(), "HEAD").unwrap(), main);
        },
    );
}

#[test]
fn default_advertisement_requires_one_symbolic_branch() {
    assert_eq!(
        advertised_default("ref: refs/heads/release/stable\tHEAD\nabc\tHEAD\n").unwrap(),
        "refs/remotes/origin/release/stable"
    );
    for output in [
        "",
        "abc\tHEAD\n",
        "ref: refs/tags/v1\tHEAD\n",
        "ref: refs/heads/main\tother\n",
        "ref: refs/heads/a\tHEAD\nref: refs/heads/b\tHEAD\n",
    ] {
        assert!(advertised_default(output).is_err(), "{output}");
    }
}
