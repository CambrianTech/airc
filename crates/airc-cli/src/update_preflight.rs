//! Candidate-owned, read-only store admission before binary replacement.
use std::path::Path;

type Error = Box<dyn std::error::Error>;

#[derive(Debug, thiserror::Error)]
#[error("candidate refused store {path}: {source}; installed binary retained")]
struct StoreRefusal {
    path: std::path::PathBuf,
    #[source]
    source: airc_store::StoreError,
}

// Deployment is blocked until durable consumer inventory covers sibling scopes.
// This checks only the requested scope and its account; expiring presence
// beacons are not an authoritative inventory and are deliberately not used.
pub async fn run(scope_home: &Path, fresh_install: bool) -> Result<(), Error> {
    let account = airc_lib::machine_account_home(scope_home);
    // Both stores can hold durable work. Do not create absent stores or mutate
    // existing ones while evaluating whether the candidate can adopt them.
    for home in [account.as_path(), scope_home] {
        let path = home.join("events.sqlite");
        if !path.try_exists()? {
            continue;
        }
        if fresh_install {
            return Err(format!("direct installation refused existing store {}; use the supported airc update transaction to quiesce its writer; installed binary retained", path.display()).into());
        }
        let observer = airc_store::SqliteDurableSink::open_read_only_path(&path).await?;
        observer
            .check_classification_adoption()
            .await
            .map_err(|source| StoreRefusal { path, source })?;
        if account.as_path() == scope_home {
            break;
        }
    }
    Ok(())
}

pub async fn publish_fresh(home: &Path, destination: &Path) -> Result<(), Error> {
    run(home, true).await?;
    crate::update_rollback::publish_fresh(&std::env::current_exe()?, destination)
}

pub fn candidate(artifact: &Path, home: &Path) -> Result<(), Error> {
    let status = airc_core::process::background(artifact)
        .arg("--home")
        .arg(home)
        .arg("store-preflight")
        .status()?;
    if !status.success() {
        return Err(format!(
            "candidate store preflight refused ({status}); installed binary retained"
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_install_refuses_existing_state_without_opening_or_modifying_it() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("events.sqlite");
        let original = b"opaque retained state";
        std::fs::write(&path, original).expect("state");
        assert!(run(dir.path(), true).await.is_err());
        assert_eq!(std::fs::read(&path).expect("preserved"), original);
        assert_eq!(std::fs::read_dir(dir.path()).expect("directory").count(), 1);
    }

    #[tokio::test]
    async fn fresh_install_preflight_does_not_create_a_store() {
        let dir = tempfile::tempdir().expect("directory");
        run(dir.path(), true).await.expect("fresh admission");
        assert_eq!(std::fs::read_dir(dir.path()).expect("directory").count(), 0);
    }
}
