//! An allocation is visible at its canonical path only after all setup succeeds.
use std::path::{Path, PathBuf};

/// Only Git's interrupted-clone sentinel, with no working files, is recoverable
/// automatically. Unknown/unborn repositories remain untouched and fail loudly.
pub(super) fn interrupted_clone(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    let names = entries
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<Vec<_>, _>>();
    matches!(names, Ok(names) if names.len() == 1 && names[0] == ".git")
        && path.join(".git").is_dir()
        && std::fs::read_to_string(path.join(".git/HEAD"))
            .is_ok_and(|head| head.trim() == "ref: refs/heads/.invalid")
}

pub(super) struct Staging {
    path: PathBuf,
}

impl Staging {
    pub(super) fn new(root: &Path, card: airc_work::WorkCardId) -> Result<Self, String> {
        let parent = root.join(".allocating");
        std::fs::create_dir_all(&parent)
            .map_err(|e| format!("create allocation staging {}: {e}", parent.display()))?;
        // A unique path prevents one failed/retried allocator deleting another's work.
        let path = parent.join(format!("{card}-{}", uuid::Uuid::new_v4()));
        Ok(Self { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn recover(self, destination: &Path) -> Result<PathBuf, String> {
        // Recheck immediately before moving anything. Preserve the ENTIRE old
        // directory, including refs/objects, even for this known sentinel shape.
        if !interrupted_clone(destination) {
            return Err("interrupted checkout changed during allocation; preserving it".into());
        }
        let retired_root = destination
            .parent()
            .ok_or("checkout has no parent")?
            .join(".retired");
        std::fs::create_dir_all(&retired_root)
            .map_err(|e| format!("create retirement directory: {e}"))?;
        let retired = retired_root.join(format!("interrupted-{}", uuid::Uuid::new_v4()));
        std::fs::rename(destination, &retired)
            .map_err(|e| format!("preserve interrupted clone: {e}"))?;
        if let Err(error) = self.publish(destination) {
            let rollback = std::fs::rename(&retired, destination);
            return Err(format!(
                "{error}; interrupted checkout rollback: {rollback:?}; retained at {}",
                retired.display()
            ));
        }
        Ok(retired)
    }

    pub(super) fn publish(self, destination: &Path) -> Result<(), String> {
        // Callers hold the existing card lifecycle lock; do not replace any
        // directory that appeared meanwhile, even an empty one.
        if destination.exists() {
            return Err(format!(
                "allocation destination {} already exists",
                destination.display()
            ));
        }
        std::fs::rename(&self.path, destination)
            .map_err(|e| format!("publish checkout {}: {e}", destination.display()))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // This unique private staging directory has never been handed to a
        // resident. A failed removal is retained and reported, never retried
        // against a canonical resident checkout.
        if self.path.exists() {
            if let Err(error) = std::fs::remove_dir_all(&self.path) {
                tracing::warn!(path = %self.path.display(), %error, "failed allocation staging retained");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shallow_mirror_recovers_interrupted_clone_with_allocation_evidence() {
        use super::super::{
            allocation_evidence, ensure_card_clone, git_in, resolve_commit, worktree_path_for,
            WorktreeOutcome, WorktreeSpec,
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("mirror.git");
        std::fs::create_dir(&source).unwrap();
        let git = |args: &[&str]| git_in(&source, args).unwrap();
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(source.join("README"), "tracked payload").unwrap();
        git(&["add", "README"]);
        git(&["commit", "--quiet", "-m", "base"]);
        git(&["remote", "add", "origin", source.to_str().unwrap()]);
        let base = resolve_commit(&source, "HEAD").unwrap();
        // Mark the fixture's root as a shallow boundary. This forces Git's
        // transport path even with --shared, just like the incident mirror.
        std::fs::write(source.join(".git/shallow"), format!("{base}\n")).unwrap();
        assert_eq!(git(&["rev-parse", "--is-shallow-repository"]), "true");
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        temp_env::with_var("HOME", Some(&home), || {
            let card = airc_work::WorkCardId::from_uuid(uuid::Uuid::new_v4());
            let path = worktree_path_for(card).unwrap();
            std::fs::create_dir_all(path.join(".git")).unwrap();
            std::fs::write(path.join(".git/HEAD"), "ref: refs/heads/.invalid\n").unwrap();
            std::fs::write(path.join(".git/diagnostic"), "preserve me").unwrap();
            let repo = airc_work::RepoId::new("Example/shallow").unwrap();
            let spec = WorktreeSpec {
                card_id: card,
                repo: &repo,
                clone_path: &source,
                branch: "card/recovery",
                start_point: Some("refs/heads/main"),
            };
            let WorktreeOutcome::Migrated { retired, .. } = ensure_card_clone(&spec).unwrap()
            else {
                panic!("expected recovery with preserved predecessor")
            };
            assert_eq!(
                std::fs::read_to_string(retired.join(".git/diagnostic")).unwrap(),
                "preserve me"
            );
            assert_eq!(resolve_commit(&path, "HEAD").unwrap(), base);
            assert_eq!(
                std::fs::read_to_string(path.join("README")).unwrap(),
                "tracked payload"
            );
            assert_eq!(
                allocation_evidence(&path).unwrap().unwrap().creation_sha,
                base
            );
            assert!(matches!(
                ensure_card_clone(&spec).unwrap(),
                WorktreeOutcome::Reused(_)
            ));
        });
    }

    #[test]
    fn interrupted_clone_is_not_successful_reuse_and_is_preserved() {
        let root = tempfile::tempdir().unwrap();
        temp_env::with_var("HOME", Some(root.path()), || {
            let card = airc_work::WorkCardId::from_u128(1);
            let path = super::super::worktree_path_for(card).unwrap();
            std::fs::create_dir_all(path.join(".git")).unwrap();
            std::fs::write(path.join(".git/HEAD"), "ref: refs/heads/.invalid\n").unwrap();
            // Unknown user work prevents automatic retirement, even with the sentinel.
            std::fs::write(path.join("work"), "keep").unwrap();
            let repo = airc_work::RepoId::new("Example/repo").unwrap();
            let result = super::super::ensure_card_clone(&super::super::WorktreeSpec {
                card_id: card,
                repo: &repo,
                clone_path: root.path(),
                branch: "card/fix",
                start_point: None,
            });
            assert!(result.unwrap_err().contains("incomplete"));
            assert_eq!(
                std::fs::read_to_string(path.join(".git/HEAD")).unwrap(),
                "ref: refs/heads/.invalid\n"
            );
        });
    }

    #[test]
    fn failed_allocation_is_never_published_and_never_removes_existing_work() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("card");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("work"), "keep").unwrap();
        let staged = Staging::new(root.path(), airc_work::WorkCardId::from_u128(1)).unwrap();
        let temporary = staged.path().to_owned();
        std::fs::create_dir(&temporary).unwrap();
        assert!(staged.publish(&destination).is_err());
        assert!(!temporary.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("work")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn successful_allocation_publishes_complete_tree() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("card");
        let staged = Staging::new(root.path(), airc_work::WorkCardId::from_u128(1)).unwrap();
        std::fs::create_dir(staged.path()).unwrap();
        std::fs::write(staged.path().join("ready"), "complete").unwrap();
        assert!(!destination.exists());
        staged.publish(&destination).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("ready")).unwrap(),
            "complete"
        );
    }
}
