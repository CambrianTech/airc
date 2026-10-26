//! Parameterized SQLite evaluation of the shared header predicate algebra.
use airc_core::HeaderFilter;
use sea_orm::sea_query::{Expr, SimpleExpr};

/// Rows that are NOT this runtime's own sends: [`airc_core::RuntimeSelf`]
/// translated to the stores' header predicate and their `client_id` column.
pub(crate) fn not_runtime_self(
    rule: &airc_core::RuntimeSelf,
    client_id: impl sea_orm::sea_query::IntoColumnRef,
) -> SimpleExpr {
    let stamped = HeaderFilter::Has {
        key: airc_core::headers::HEADER_AIRC_CLIENT.to_string(),
    };
    let unstamped_self = predicate(&HeaderFilter::Not(Box::new(stamped)))
        .and(Expr::col(client_id).eq(rule.client_id.as_uuid()));
    let is_self = match &rule.runtime_client {
        Some(tag) => predicate(&HeaderFilter::Exact {
            key: airc_core::headers::HEADER_AIRC_CLIENT.to_string(),
            value: tag.clone(),
        })
        .or(unstamped_self),
        None => unstamped_self,
    };
    is_self.not()
}

pub(crate) fn predicate(filter: &HeaderFilter) -> SimpleExpr {
    match filter {
        HeaderFilter::Any => Expr::cust("1 = 1"),
        HeaderFilter::Has { key } => Expr::cust_with_values(
            "EXISTS (SELECT 1 FROM json_each(headers) AS hf WHERE hf.key = ?)",
            [key.clone()],
        ),
        HeaderFilter::Exact { key, value } => Expr::cust_with_values(
            "EXISTS (SELECT 1 FROM json_each(headers) AS hf WHERE hf.key = ? AND hf.value = ? COLLATE BINARY)",
            [key.clone(), value.clone()],
        ),
        HeaderFilter::Prefix { key, value_prefix } => Expr::cust_with_values(
            "EXISTS (SELECT 1 FROM json_each(headers) AS hf WHERE hf.key = ? AND substr(CAST(hf.value AS BLOB), 1, length(CAST(? AS BLOB))) = CAST(? AS BLOB))",
            [key.clone(), value_prefix.clone(), value_prefix.clone()],
        ),
        HeaderFilter::Not(inner) => predicate(inner).not(),
        HeaderFilter::All(items) => items.iter().fold(Expr::cust("1 = 1"), |acc, f| acc.and(predicate(f))),
        HeaderFilter::AnyOf(items) => items.iter().fold(Expr::cust("1 = 0"), |acc, f| acc.or(predicate(f))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::{Alias, Query, SqliteQueryBuilder};
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn sql_matches_header_algebra_with_literal_keys_and_unicode() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let mut headers = airc_core::Headers::new();
        headers.insert("a.b\"%_".into(), "🌍_%Case".into());
        let has = HeaderFilter::Has {
            key: "a.b\"%_".into(),
        };
        let missing = HeaderFilter::Has {
            key: "absent".into(),
        };
        let filters = vec![
            HeaderFilter::Any,
            has.clone(),
            missing.clone(),
            HeaderFilter::Not(Box::new(missing.clone())),
            HeaderFilter::Exact {
                key: "a.b\"%_".into(),
                value: "🌍_%Case".into(),
            },
            HeaderFilter::Exact {
                key: "a.b\"%_".into(),
                value: "🌍_%case".into(),
            },
            HeaderFilter::Prefix {
                key: "a.b\"%_".into(),
                value_prefix: "🌍_%".into(),
            },
            HeaderFilter::Prefix {
                key: "absent".into(),
                value_prefix: "".into(),
            },
            HeaderFilter::All(vec![]),
            HeaderFilter::AnyOf(vec![]),
            HeaderFilter::All(vec![has.clone(), HeaderFilter::Not(Box::new(missing))]),
            HeaderFilter::AnyOf(vec![has.clone(), HeaderFilter::Not(Box::new(has))]),
        ];
        for filter in filters {
            let input = Query::select()
                .expr_as(
                    Expr::val(serde_json::to_string(&headers).unwrap()),
                    Alias::new("headers"),
                )
                .to_owned();
            let (sql, values) = Query::select()
                .expr_as(predicate(&filter), Alias::new("matched"))
                .from_subquery(input, Alias::new("input"))
                .build(SqliteQueryBuilder);
            let row = db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    sql,
                    values,
                ))
                .await
                .unwrap()
                .unwrap();
            let matched: i64 = row.try_get("", "matched").unwrap();
            assert_eq!(matched != 0, filter.matches(&headers), "{filter:?}");
        }
    }
}
