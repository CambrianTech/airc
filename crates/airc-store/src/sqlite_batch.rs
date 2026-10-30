//! SQLite statement batching, shared by event and derived-header writers.
//! The engine reports its compiled bind ceiling; entity columns bound each
//! row's maximum bind use. Callers retain the transaction across every chunk.
use sea_orm::{
    sea_query::{Alias, OnConflict, Query},
    ConnectionTrait, DbBackend, DbErr, EntityTrait, Iterable,
};

pub(crate) async fn bind_capacity(db: &impl ConnectionTrait) -> Result<usize, DbErr> {
    if db.get_database_backend() != DbBackend::Sqlite {
        return Err(DbErr::Custom(
            "SQLite batch writer requires a SQLite connection".into(),
        ));
    }
    let query = Query::select()
        .column(Alias::new("compile_options"))
        .from(Alias::new("pragma_compile_options"))
        .to_owned();
    for row in db
        .query_all(db.get_database_backend().build(&query))
        .await?
    {
        let option: String = row.try_get("", "compile_options")?;
        if let Some(bound) = option.strip_prefix("MAX_VARIABLE_NUMBER=") {
            return bound
                .parse::<std::num::NonZeroUsize>()
                .map(|n| n.get())
                .map_err(|error| DbErr::Custom(format!("invalid SQLite bind capacity: {error}")));
        }
    }
    Err(DbErr::Custom(
        "SQLite did not report MAX_VARIABLE_NUMBER; batch capacity cannot be established".into(),
    ))
}

/// No transaction is opened here: one caller-owned transaction covers event
/// chunks and header chunks, including rollback of an error in a later chunk.
/// Conflict handling is deliberately limited to idempotent DO NOTHING, which
/// binds no extra values beyond the rows.
pub(crate) async fn insert_ignoring<E: EntityTrait>(
    db: &impl ConnectionTrait,
    rows: Vec<E::ActiveModel>,
    unique_columns: impl IntoIterator<Item = E::Column>,
) -> Result<(), DbErr>
where
    E::ActiveModel: Send,
{
    if rows.is_empty() {
        return Ok(());
    }
    let columns = E::Column::iter().count();
    let capacity = bind_capacity(db).await?;
    let rows_per_insert = capacity
        .checked_div(columns)
        .filter(|count| *count != 0)
        .ok_or_else(|| {
            DbErr::Custom(format!(
                "SQLite bind capacity {capacity} cannot hold an entity row of {columns} columns"
            ))
        })?;
    let conflict = OnConflict::columns(unique_columns).do_nothing().to_owned();
    for chunk in rows.chunks(rows_per_insert) {
        match E::insert_many(chunk.to_vec())
            .on_conflict(conflict.clone())
            .exec(db)
            .await
        {
            Ok(_) | Err(DbErr::RecordNotInserted) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
