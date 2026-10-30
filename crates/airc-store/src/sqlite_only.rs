//! The ONE place a SQLite-specific statement with no ORM expression is
//! written: pragmas, the vacuum family and `EXPLAIN QUERY PLAN`. Everything
//! else in the store goes through sea_orm entities and typed sea_query
//! expressions; the raw-SQL ratchet (`tests/no_raw_sql.rs`) allows SQL text
//! only inside [`sqlite_only`].

use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement};

use crate::error::StoreError;

/// An integer pragma the store reads.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Pragma {
    PageSize,
    PageCount,
    FreelistCount,
    AutoVacuum,
}

/// A SQLite-specific operation the ORM cannot express.
pub(crate) enum SqliteOnly {
    /// Read one integer pragma (one row, one column).
    Read(Pragma),
    /// `auto_vacuum = INCREMENTAL`; takes effect at the next full vacuum.
    AutoVacuumIncremental,
    /// A full `VACUUM` (rebuilds the file).
    Vacuum,
    /// `wal_checkpoint(TRUNCATE)`: checkpoint and truncate the WAL file.
    WalCheckpointTruncate,
    /// `incremental_vacuum(pages)`: return up to `pages` freelist pages.
    IncrementalVacuum(u64),
    /// `EXPLAIN QUERY PLAN` of an ORM-built statement: how SQLite will run it
    /// (which index a scan searches). One row per plan step, column `detail`.
    #[cfg(test)]
    QueryPlan(Statement),
}

/// Run one [`SqliteOnly`] operation and return its rows.
pub(crate) async fn sqlite_only(
    db: &impl ConnectionTrait,
    op: SqliteOnly,
) -> Result<Vec<QueryResult>, StoreError> {
    let sql = |text: String| Statement::from_string(DbBackend::Sqlite, text);
    let statement = match op {
        SqliteOnly::Read(pragma) => sql(format!(
            "PRAGMA {}",
            match pragma {
                Pragma::PageSize => "page_size",
                Pragma::PageCount => "page_count",
                Pragma::FreelistCount => "freelist_count",
                Pragma::AutoVacuum => "auto_vacuum",
            }
        )),
        SqliteOnly::AutoVacuumIncremental => sql("PRAGMA auto_vacuum = INCREMENTAL".into()),
        SqliteOnly::Vacuum => sql("VACUUM".into()),
        SqliteOnly::WalCheckpointTruncate => sql("PRAGMA wal_checkpoint(TRUNCATE)".into()),
        SqliteOnly::IncrementalVacuum(pages) => sql(format!("PRAGMA incremental_vacuum({pages})")),
        #[cfg(test)]
        SqliteOnly::QueryPlan(built) => Statement {
            sql: format!("EXPLAIN QUERY PLAN {}", built.sql),
            values: built.values,
            db_backend: DbBackend::Sqlite,
        },
    };
    Ok(db.query_all(statement).await?)
}

/// The value of one integer pragma.
pub(crate) async fn pragma_u64(
    db: &impl ConnectionTrait,
    pragma: Pragma,
) -> Result<u64, StoreError> {
    let rows = sqlite_only(db, SqliteOnly::Read(pragma)).await?;
    let row = rows
        .first()
        .ok_or_else(|| StoreError::Migration(format!("pragma {pragma:?} returned no row")))?;
    let value: i64 = row.try_get_by_index(0)?;
    u64::try_from(value)
        .map_err(|_| StoreError::Migration(format!("pragma {pragma:?} was negative: {value}")))
}

/// The plan steps of an ORM-built statement, for index evidence in tests.
#[cfg(test)]
pub(crate) async fn query_plan(
    db: &impl ConnectionTrait,
    built: Statement,
) -> Result<Vec<String>, StoreError> {
    sqlite_only(db, SqliteOnly::QueryPlan(built))
        .await?
        .iter()
        .map(|row| {
            row.try_get::<String>("", "detail")
                .map_err(StoreError::from)
        })
        .collect()
}
