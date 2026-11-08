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

mod card_clone;
