//! Card clones: the mirror they borrow from, migration of held linked worktrees, submodules.

use super::*;

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
                inodes(&source.join(".git/objects")).is_disjoint(&inodes(&mirror.join("objects"))),
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

// what this catches (card 327e9b3e): a card clone's submodule downloaded from its URL on
// every staging (continuum's llama.cpp history is 510 MB) instead of borrowing objects
// from the node's mirror of it, as the superproject does; or a non-GitHub submodule
// wrongly keyed to a mirror.
#[cfg(unix)]
#[test]
fn a_card_clone_borrows_its_submodule_objects_from_the_submodules_mirror() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let git = |dir: &Path, args: &[&str]| git_in(dir, args).unwrap_or_else(|e| panic!("{e}"));
    let bare = |name: &str| {
        let p = root.join(name);
        git(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=canary",
                p.to_str().unwrap(),
            ],
        );
        p
    };
    let who = |dir: &Path| {
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
    };
    let sub_remote = bare("sub.git");
    let sub_work = root.join("sub-work");
    git(
        &root,
        &[
            "clone",
            "--quiet",
            sub_remote.to_str().unwrap(),
            sub_work.to_str().unwrap(),
        ],
    );
    who(&sub_work);
    std::fs::write(sub_work.join("vendored.txt"), "the submodule\n").unwrap();
    git(&sub_work, &["add", "."]);
    git(&sub_work, &["commit", "--quiet", "-m", "sub"]);
    git(&sub_work, &["push", "--quiet", "origin", "HEAD:canary"]);
    let remote = bare("remote.git");
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
    who(&source);
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    // The submodule is declared under its GitHub URL; git rewrites it to the local bare
    // repo, so the mirror path is exercised with no network.
    let url = "https://github.com/o/sub";
    let rewrite = format!("url.file://{}.insteadOf", sub_remote.display());
    temp_env::with_vars(
        [
            ("HOME", Some(home.to_str().unwrap())),
            ("GIT_CONFIG_COUNT", Some("2")),
            ("GIT_CONFIG_KEY_0", Some(rewrite.as_str())),
            ("GIT_CONFIG_VALUE_0", Some(url)),
            ("GIT_CONFIG_KEY_1", Some("protocol.file.allow")),
            ("GIT_CONFIG_VALUE_1", Some("always")),
        ],
        || {
            git(&source, &["submodule", "add", "--quiet", url, "vendor/dep"]);
            git(&source, &["commit", "--quiet", "-m", "add submodule"]);
            git(&source, &["push", "--quiet", "origin", "HEAD:canary"]);
            assert_eq!(
                declared_submodules(&source),
                vec![("vendor/dep".to_string(), url.to_string())]
            );
            let repo = RepoId::new("o/r").unwrap();
            let mirror = ensure_mirror(&repo, &source).expect("superproject mirror");
            let card = WorkCardId::from_u128((0x327e_9b3e_u128) << 96);
            let path = ensure_card_clone(&WorktreeSpec {
                card_id: card,
                repo: &repo,
                clone_path: &mirror,
                branch: "327e9b3e/sub",
                start_point: Some("refs/heads/canary"),
            })
            .expect("card clone")
            .path()
            .to_path_buf();
            assert_eq!(
                std::fs::read_to_string(path.join("vendor/dep/vendored.txt")).unwrap(),
                "the submodule\n"
            );
            let sub_mirror = home.join(".airc/mirrors/o/sub.git");
            assert_eq!(
                git(&path.join("vendor/dep"), &["status", "--porcelain"]),
                "",
                "submodule gitdir and worktree paths still resolve after publication"
            );
            assert_eq!(
                git(&sub_mirror, &["config", "extensions.preciousObjects"]),
                "true",
                "the submodule mirror is precious too"
            );
            let alternates = std::fs::read_to_string(
                path.join(".git/modules/vendor/dep/objects/info/alternates"),
            )
            .expect("the submodule borrows through alternates");
            assert_eq!(
                Path::new(alternates.trim()).canonicalize().unwrap(),
                sub_mirror.join("objects").canonicalize().unwrap(),
                "its objects come from the node's mirror of it, not a fresh download"
            );
        },
    );
    assert_eq!(
        github_repo_of("https://github.com/CambrianTech/llama.cpp"),
        RepoId::new("CambrianTech/llama.cpp").ok()
    );
    assert_eq!(
        github_repo_of("https://github.com/o/r.git"),
        RepoId::new("o/r").ok()
    );
    assert_eq!(
        github_repo_of("git@github.com:o/r.git"),
        RepoId::new("o/r").ok()
    );
    assert_eq!(
        github_repo_of("https://gitlab.com/o/r"),
        None,
        "another host keeps its URL"
    );
    assert_eq!(github_repo_of("/local/path"), None);
}

// what this catches (Fable on #1591): residents staging at once after a deploy all create
// the same mirror together, and one borrows a half-written mirror, or a failed creation
// deletes objects another clone already borrowed. One mirror must result, complete,
// with no temporary directory left behind.
#[test]
fn concurrent_first_stagings_publish_one_complete_mirror() {
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
    git(&source, &["config", "user.email", "t@t"]);
    git(&source, &["config", "user.name", "t"]);
    std::fs::write(source.join("lib.rs"), "fn a() {}\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "base"]);
    git(&source, &["push", "--quiet", "origin", "HEAD:canary"]);
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    temp_env::with_var("HOME", Some(&home), || {
        let repo = RepoId::new("o/r").unwrap();
        let made: Vec<Result<PathBuf, String>> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| ensure_mirror(&repo, &source)))
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().expect("no staging thread panics"))
                .collect()
        });
        let mirror = home.join(".airc/mirrors/o/r.git");
        for result in &made {
            assert_eq!(
                result
                    .as_ref()
                    .expect("every concurrent staging gets the mirror"),
                &mirror
            );
        }
        assert_eq!(
            git(&mirror, &["config", "extensions.preciousObjects"]),
            "true",
            "the published mirror is complete"
        );
        assert!(git_in(&mirror, &["rev-parse", "--verify", "refs/heads/canary"]).is_ok());
        assert!(
            !home.join(".airc/mirrors/o/r.git.creating").exists(),
            "no temporary mirror left behind"
        );
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

// what this catches (continuum card 7f824214, Kimi on the Windows node 2026-11-08): a card
// clone with no allocation evidence was refused forever ("preserved for recovery") with no
// recovery, so a resident sat on a weeks-old checkout that could never submit. One holding
// nothing unpublished is moved aside whole and re-allocated at the current start point; one
// holding an unpushed commit (on a branch or a detached HEAD), an untracked file or a stash is
// kept byte-identical and the
// refusal names what it holds.
#[test]
fn an_unanchored_card_clone_is_reallocated_only_when_nothing_in_it_is_unpublished() {
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
    let commit = |dir: &Path, text: &str| {
        std::fs::write(dir.join("lib.rs"), text).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "--quiet", "-m", text]);
    };
    commit(&source, "fn old() {}\n");
    git(&source, &["push", "--quiet", "origin", "HEAD:canary"]);
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    temp_env::with_var("HOME", Some(&home), || {
        let repo = RepoId::new("o/r").unwrap();
        let stage = |card: WorkCardId| {
            let mirror = ensure_mirror(&repo, &source).unwrap();
            ensure_card_clone(&WorktreeSpec {
                card_id: card,
                repo: &repo,
                clone_path: &mirror,
                branch: "7f824214/work",
                start_point: None,
            })
        };
        // A card clone made before allocation evidence existed: the same tree, its two refs gone.
        let unanchored = |n: u128| {
            let card = WorkCardId::from_u128((0x7f82_4210 + n) << 96);
            let path = stage(card).unwrap().path().to_path_buf();
            git(&path, &["update-ref", "-d", allocation::ALLOCATION_REF]);
            git(&path, &["update-ref", "-d", CREATION_BASE_REF]);
            assert_eq!(allocation_evidence(&path).unwrap(), None);
            who(&path);
            (card, path)
        };
        let (clean_card, clean) = unanchored(1);
        let (unpushed_card, unpushed) = unanchored(2);
        commit(&unpushed, "fn hers() {}\n");
        let (untracked_card, untracked) = unanchored(3);
        std::fs::write(untracked.join("notes.txt"), "hers").unwrap();
        let (detached_card, detached) = unanchored(5);
        git(&detached, &["checkout", "--quiet", "--detach"]);
        commit(&detached, "fn detached() {}\n");
        let (stashed_card, stashed) = unanchored(4);
        std::fs::write(stashed.join("lib.rs"), "fn wip() {}\n").unwrap();
        git(&stashed, &["stash", "--quiet"]);
        // canary moves on after they were made.
        commit(&source, "fn new() {}\n");
        git(&source, &["push", "--quiet", "origin", "HEAD:canary"]);
        let canary = git(&source, &["rev-parse", "HEAD"]);

        let Ok(WorktreeOutcome::Reallocated { path, retired }) = stage(clean_card) else {
            panic!("a clean, published, unanchored clone is reallocated");
        };
        assert_eq!(path, clean);
        assert_eq!(
            git(&path, &["rev-parse", "HEAD"]),
            canary,
            "a fresh allocation at the current start point"
        );
        assert_eq!(
            allocation_evidence(&path).unwrap().map(|e| e.creation_sha),
            Some(canary),
            "with its evidence"
        );
        assert!(
            retired.starts_with(home.join(".airc/worktrees/.retired"))
                && retired.join("lib.rs").exists(),
            "the old tree is moved aside whole"
        );
        assert!(matches!(stage(clean_card), Ok(WorktreeOutcome::Reused(_))));

        for (card, path, holds) in [
            (unpushed_card, &unpushed, "not on the remote"),
            (untracked_card, &untracked, "untracked"),
            (stashed_card, &stashed, "a stash"),
            // Codex on #1668: a detached HEAD's commit is on no branch.
            (detached_card, &detached, "HEAD at"),
        ] {
            let head = git(path, &["rev-parse", "HEAD"]);
            let Err(refusal) = stage(card) else {
                panic!("kept: {holds}")
            };
            assert!(
                refusal.contains(holds),
                "the refusal names what it holds: {refusal}"
            );
            assert_eq!(
                git(path, &["rev-parse", "HEAD"]),
                head,
                "untouched: {holds}"
            );
            assert_eq!(
                allocation_evidence(path).unwrap(),
                None,
                "never backfilled: {holds}"
            );
        }
        assert!(untracked.join("notes.txt").exists());
        assert!(!git(&stashed, &["stash", "list"]).is_empty());
    });
}
