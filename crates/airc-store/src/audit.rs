//! Bounded read-only retrieval of pre-classification records. These records are
//! evidence for upgrade planning, never live envelopes or replay input.
use crate::StoreError;
use sea_orm::{
    sea_query::{Alias, Expr, Order, Query},
    ConnectionTrait, DatabaseConnection, QueryResult,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum AuditTable {
    Transcript,
    Bus,
}
impl AuditTable {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Transcript => "events",
            Self::Bus => "bus_events",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuditRecord {
    pub event_id: Uuid,
    pub room_id: Uuid,
    pub peer_id: Uuid,
    pub client_id: Uuid,
    pub kind: String,
    pub occurred_at_ms: i64,
    pub target: serde_json::Value,
    pub headers: serde_json::Value,
    pub content: AuditContent,
}
#[derive(Debug, Serialize)]
#[serde(tag = "table", rename_all = "snake_case")]
pub enum AuditContent {
    Transcript {
        lamport: i64,
        body: Option<serde_json::Value>,
        attachment: Option<serde_json::Value>,
        receipt: Option<serde_json::Value>,
        metadata: serde_json::Value,
    },
    Bus {
        epoch: i64,
        counter: i64,
        delivery: String,
        correlation_id: Option<Uuid>,
        coalesce_key: Option<String>,
        payload: Vec<u8>,
    },
}

pub(crate) async fn page(
    db: &DatabaseConnection,
    table: AuditTable,
    after: Option<Uuid>,
    limit: std::num::NonZeroU32,
) -> Result<Vec<AuditRecord>, StoreError> {
    let mut columns = vec![
        "event_id",
        "room_id",
        "peer_id",
        "client_id",
        "kind",
        "occurred_at_ms",
        "target",
        "headers",
    ];
    columns.extend(match table {
        AuditTable::Transcript => vec!["lamport", "body", "attachment", "receipt", "metadata"],
        AuditTable::Bus => vec![
            "epoch",
            "counter",
            "delivery",
            "correlation_id",
            "coalesce_key",
            "payload",
        ],
    });
    let mut query = Query::select();
    query
        .columns(columns.into_iter().map(Alias::new))
        .from(Alias::new(table.name()))
        .order_by(Alias::new("event_id"), Order::Asc)
        .limit(u64::from(limit.get()));
    if let Some(after) = after {
        query.and_where(Expr::col(Alias::new("event_id")).gt(after));
    }
    let rows = db
        .query_all(db.get_database_backend().build(&query))
        .await?;
    rows.into_iter().map(|row| decode(row, table)).collect()
}
fn decode(row: QueryResult, table: AuditTable) -> Result<AuditRecord, StoreError> {
    Ok(AuditRecord {
        event_id: row.try_get("", "event_id")?,
        room_id: row.try_get("", "room_id")?,
        peer_id: row.try_get("", "peer_id")?,
        client_id: row.try_get("", "client_id")?,
        kind: row.try_get("", "kind")?,
        occurred_at_ms: row.try_get("", "occurred_at_ms")?,
        target: row.try_get("", "target")?,
        headers: row.try_get("", "headers")?,
        content: match table {
            AuditTable::Transcript => AuditContent::Transcript {
                lamport: row.try_get("", "lamport")?,
                body: row.try_get("", "body")?,
                attachment: row.try_get("", "attachment")?,
                receipt: row.try_get("", "receipt")?,
                metadata: row.try_get("", "metadata")?,
            },
            AuditTable::Bus => AuditContent::Bus {
                epoch: row.try_get("", "epoch")?,
                counter: row.try_get("", "counter")?,
                delivery: row.try_get("", "delivery")?,
                correlation_id: row.try_get("", "correlation_id")?,
                coalesce_key: row.try_get("", "coalesce_key")?,
                payload: row.try_get("", "payload")?,
            },
        },
    })
}
