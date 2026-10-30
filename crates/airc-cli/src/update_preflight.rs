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

pub async fn run(scope_home: &Path) -> Result<(), Error> {
    let account = airc_lib::machine_account_home(scope_home);
    // Both stores can hold durable work. Do not create absent stores or mutate
    // existing ones while evaluating whether the candidate can adopt them.
    for home in [account.as_path(), scope_home] {
        let path = home.join("events.sqlite");
        if !path.try_exists()? {
            continue;
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
