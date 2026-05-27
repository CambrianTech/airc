//! Persisted generational epoch — the ORM-backed `airc_bus::EpochStore`
//! (§3.8 of `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! `airc_bus::EpochStore::bump_and_load` is **sync** and, by its contract,
//! called **exactly once per daemon start**; sea-orm is async. So the
//! atomic increment runs in async [`SqliteEpochStore::bump`] at startup
//! and the resulting epoch is handed to `SeqSource` through the sync
//! trait — faithful to the once-per-start contract, no blocking bridge.
//!
//! Why persisted at all: deliver-first (§3.3) can ack a `counter` the ORM
//! hasn't flushed; a crash loses that tail, and a counter rebuilt from the
//! durable max would *reissue* numbers live subscribers already saw.
//! Bumping a PERSISTED epoch every start makes post-restart events sort
//! strictly after pre-restart ones. `airc_bus::InMemoryEpochStore` does
//! not survive a restart, so the daemon must use this cell.

use airc_bus::{BusError, EpochStore};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

/// ORM-backed persisted epoch cell (the singleton `bus_epoch` row).
pub struct SqliteEpochStore {
    epoch: u64,
}

impl SqliteEpochStore {
    /// Atomically increment the persisted epoch and capture the new value.
    /// One round-trip; run once per daemon start. The first-ever bump
    /// yields `1` (epoch `0` is the "never started" sentinel reserved for
    /// unstamped envelopes). The single-writer daemon owns the store, and
    /// the UPSERT is atomic regardless.
    pub async fn bump(db: &DatabaseConnection) -> Result<Self, BusError> {
        let backend = db.get_database_backend();
        // Insert-or-increment in one statement. In SQLite's UPSERT, an
        // unqualified column in DO UPDATE refers to the existing row, so
        // `epoch = epoch + 1` increments; RETURNING hands back the new
        // value atomically.
        let sql = "INSERT INTO bus_epoch (id, epoch) VALUES (0, 1) \
                   ON CONFLICT(id) DO UPDATE SET epoch = epoch + 1 \
                   RETURNING epoch";
        let row = db
            .query_one(Statement::from_string(backend, sql.to_owned()))
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?
            .ok_or_else(|| BusError::Sink("bus_epoch bump returned no row".to_owned()))?;
        let epoch: i64 = row
            .try_get("", "epoch")
            .map_err(|e| BusError::Sink(e.to_string()))?;
        let epoch = u64::try_from(epoch)
            .map_err(|_| BusError::Sink(format!("bus_epoch out of u64 range: {epoch}")))?;
        Ok(Self { epoch })
    }

    /// The epoch captured at construction (for assertions / wiring).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl EpochStore for SqliteEpochStore {
    fn bump_and_load(&self) -> u64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::Migrator;
    use sea_orm::{ConnectOptions, Database};
    use sea_orm_migration::MigratorTrait;

    async fn migrated_in_memory() -> DatabaseConnection {
        let mut opts = ConnectOptions::new("sqlite::memory:".to_owned());
        opts.max_connections(1);
        let db = Database::connect(opts).await.expect("connect");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }

    #[tokio::test]
    async fn first_bump_yields_epoch_one() {
        let db = migrated_in_memory().await;
        let store = SqliteEpochStore::bump(&db).await.expect("bump");
        assert_eq!(
            store.epoch(),
            1,
            "epoch 0 is the sentinel; first start is 1"
        );
        assert_eq!(
            store.bump_and_load(),
            1,
            "trait hands back the captured epoch"
        );
    }

    #[tokio::test]
    async fn each_start_bumps_the_persisted_epoch() {
        // Two bumps against the SAME db model two daemon starts on the same
        // home — the epoch must climb monotonically (§3.8 crash-safety).
        let db = migrated_in_memory().await;
        assert_eq!(SqliteEpochStore::bump(&db).await.expect("1").epoch(), 1);
        assert_eq!(SqliteEpochStore::bump(&db).await.expect("2").epoch(), 2);
        assert_eq!(SqliteEpochStore::bump(&db).await.expect("3").epoch(), 3);
    }

    #[tokio::test]
    async fn epoch_persists_across_reopened_connections() {
        // A real restart reopens the file. Use a temp file so the second
        // connection sees the first's committed epoch (in-memory dbs are
        // per-connection, so this needs a path).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("epoch.sqlite");
        let url = format!("sqlite://{}?mode=rwc", path.to_string_lossy());

        let open = || async {
            let mut opts = ConnectOptions::new(url.clone());
            opts.max_connections(1);
            let db = Database::connect(opts).await.expect("connect");
            Migrator::up(&db, None).await.expect("migrate");
            db
        };

        let first = SqliteEpochStore::bump(&open().await).await.expect("first");
        assert_eq!(first.epoch(), 1);
        // Reopen (restart): the persisted cell must carry forward.
        let second = SqliteEpochStore::bump(&open().await).await.expect("second");
        assert_eq!(second.epoch(), 2, "persisted epoch survives reopen");
    }
}
