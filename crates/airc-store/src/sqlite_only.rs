//! The ONE place a SQLite-specific statement with no ORM expression is
//! written. Everything else in the store goes through sea_orm entities and
//! typed sea_query expressions; the raw-SQL ratchet allows SQL text only in
//! [`sqlite_only`].

use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement};

use crate::error::StoreError;

/// A SQLite-specific operation the ORM cannot express.
pub(crate) enum SqliteOnly {
    /// `EXPLAIN QUERY PLAN` of an ORM-built statement: how SQLite will run it
    /// (which index a scan searches). One row per plan step, column `detail`.
    QueryPlan(Statement),
}

/// Run one [`SqliteOnly`] operation and return its rows.
pub(crate) async fn sqlite_only(
    db: &impl ConnectionTrait,
    op: SqliteOnly,
) -> Result<Vec<QueryResult>, StoreError> {
    let statement = match op {
        SqliteOnly::QueryPlan(built) => Statement {
            sql: format!("EXPLAIN QUERY PLAN {}", built.sql),
            values: built.values,
            db_backend: DbBackend::Sqlite,
        },
    };
    Ok(db.query_all(statement).await?)
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
