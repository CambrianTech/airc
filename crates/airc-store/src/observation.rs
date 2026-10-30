//! Read-only observation of existing store state. No creation, migration or derivation.
use crate::{entities::local_identity, StoreError};
use sea_orm::{
    ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter,
};
use std::path::Path;

pub(crate) async fn connect(path: &Path) -> Result<DatabaseConnection, sea_orm::DbErr> {
    let path = path.to_owned();
    let timeout = crate::sqlite::POOL_ACQUIRE_TIMEOUT;
    let mut opts = ConnectOptions::new("sqlite::memory:");
    opts.connect_timeout(timeout)
        .acquire_timeout(timeout)
        .max_connections(1)
        .sqlx_logging(false);
    // Native filename, so # and ? remain filename characters, not URI parameters.
    opts.map_sqlx_sqlite_opts(move |_| {
        sea_orm::sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(timeout)
    });
    Database::connect(opts).await
}

/// Observation capability: its public API exposes no writes.
pub struct StoreObservation {
    db: DatabaseConnection,
}

impl StoreObservation {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            db: connect(path).await?,
        })
    }

    /// Presence of the explicitly named identity. Missing schema remains an error.
    pub async fn has_identity(&self, agent_name: &str) -> Result<bool, StoreError> {
        Ok(local_identity::Entity::find()
            .filter(local_identity::Column::AgentName.eq(agent_name))
            .count(&self.db)
            .await?
            != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observation_never_creates_or_initializes_a_store(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("owner # store.sqlite");
        assert!(StoreObservation::open(&path).await.is_err());
        assert!(!path.exists());
        std::fs::File::create(&path)?;
        let observer = StoreObservation::open(&path).await?;
        assert!(observer
            .has_identity(crate::DEFAULT_AGENT_NAME)
            .await
            .is_err());
        drop(observer);
        assert_eq!(std::fs::metadata(path)?.len(), 0);
        Ok(())
    }
}
