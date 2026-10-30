//! Refuse populated pre-classification stores: replay supplies durable work
//! obligations, so hiding unclassified history would silently erase the board.
//! A clean store may adopt the schema. No body inference or default class.
use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        for table in ["events", "bus_events"] {
            let query = Query::select()
                .column(Alias::new("event_id"))
                .from(Alias::new(table))
                .limit(1)
                .to_owned();
            if connection
                .query_one(connection.get_database_backend().build(&query))
                .await?
                .is_some()
            {
                return Err(DbErr::Migration(format!(
                    "classification upgrade refused: {table} contains pre-classification history; preserve this store and running binary; export audit records and resolve durable work obligations before adoption"
                )));
            }
        }
        for table in ["events", "bus_events"] {
            for column in [
                ColumnDef::new(Alias::new("class"))
                    .string()
                    .null()
                    .to_owned(),
                ColumnDef::new(Alias::new("class_family"))
                    .integer()
                    .null()
                    .to_owned(),
            ] {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
            for discriminator in ["class", "class_family"] {
                let mut index = Index::create();
                index
                    .name(format!("idx_{table}_room_{discriminator}_order"))
                    .table(Alias::new(table))
                    .col(Alias::new("room_id"))
                    .col(Alias::new(discriminator));
                for column in if table == "events" {
                    vec!["lamport", "event_id"]
                } else {
                    vec!["epoch", "counter", "event_id"]
                } {
                    index.col(Alias::new(column));
                }
                manager.create_index(index.to_owned()).await?;
            }
        }
        Ok(())
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "authenticated classification cannot be downgraded".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audit::{AuditContent, AuditTable},
        entities::{bus_event, event},
        migration::{apply_migrations, Migrator},
    };
    use sea_orm::{Database, EntityTrait, PaginatorTrait, Set};
    use sea_orm_migration::seaql_migrations;
    use uuid::Uuid;

    #[tokio::test]
    async fn populated_preclassification_stores_refuse_without_schema_version_or_row_changes() {
        for table in [AuditTable::Transcript, AuditTable::Bus] {
            let dir = tempfile::tempdir().expect("directory");
            let path = dir.path().join("history.sqlite");
            let db = Database::connect(crate::sqlite::sqlite_file_url(&path))
                .await
                .expect("database");
            Migrator::up(&db, Some(22))
                .await
                .expect("pre-classification schema");
            let id = Uuid::from_u128(1);
            let common = serde_json::json!({"retained":"exact evidence"});
            match table {
                AuditTable::Transcript => {
                    event::Entity::insert(event::ActiveModel {
                        event_id: Set(id),
                        room_id: Set(id),
                        peer_id: Set(id),
                        client_id: Set(id),
                        kind: Set("system".into()),
                        occurred_at_ms: Set(1),
                        lamport: Set(1),
                        target: Set(serde_json::json!("all")),
                        headers: Set(common.clone()),
                        body: Set(Some(common.clone())),
                        metadata: Set(common.clone()),
                        ..Default::default()
                    })
                    .exec(&db)
                    .await
                    .expect("old transcript");
                }
                AuditTable::Bus => {
                    bus_event::Entity::insert(bus_event::ActiveModel {
                        event_id: Set(id),
                        room_id: Set(id),
                        peer_id: Set(id),
                        client_id: Set(id),
                        kind: Set("event".into()),
                        occurred_at_ms: Set(1),
                        epoch: Set(1),
                        counter: Set(1),
                        delivery: Set("durable".into()),
                        target: Set(serde_json::json!("all")),
                        headers: Set(common.clone()),
                        payload: Set(vec![255, 0, 128]),
                        ..Default::default()
                    })
                    .exec(&db)
                    .await
                    .expect("old bus event");
                }
            }
            let observer = crate::SqliteDurableSink::open_read_only_path(&path)
                .await
                .expect("observer");
            let limit = std::num::NonZeroU32::new(1).expect("bound");
            let before = observer
                .audit_page(table, None, limit)
                .await
                .expect("old-schema audit");
            assert_eq!(before.len(), 1);
            let error = apply_migrations(&db).await.expect_err("must refuse");
            assert!(
                error.to_string().contains("classification upgrade refused"),
                "{error}"
            );
            for name in ["events", "bus_events"] {
                assert!(!SchemaManager::new(&db)
                    .has_column(name, "class")
                    .await
                    .expect("schema"));
                assert!(!SchemaManager::new(&db)
                    .has_column(name, "class_family")
                    .await
                    .expect("schema"));
            }
            assert_eq!(
                seaql_migrations::Entity::find()
                    .count(&db)
                    .await
                    .expect("versions"),
                22
            );
            let after = observer
                .audit_page(table, None, limit)
                .await
                .expect("audit after refusal");
            assert_eq!(
                serde_json::to_value(&before).expect("serialize"),
                serde_json::to_value(&after).expect("serialize")
            );
            assert!(observer
                .audit_page(table, Some(id), limit)
                .await
                .expect("bounded cursor")
                .is_empty());
            match &after[0].content {
                AuditContent::Transcript { body, .. } => assert_eq!(body.as_ref(), Some(&common)),
                AuditContent::Bus { payload, .. } => assert_eq!(payload, &[255, 0, 128]),
            }
        }
    }

    #[tokio::test]
    async fn empty_preclassification_store_can_adopt() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        Migrator::up(&db, Some(22)).await.expect("old schema");
        apply_migrations(&db).await.expect("empty adoption");
        assert!(SchemaManager::new(&db)
            .has_column("events", "class")
            .await
            .expect("schema"));
        assert!(SchemaManager::new(&db)
            .has_column("bus_events", "class")
            .await
            .expect("schema"));
    }
}
