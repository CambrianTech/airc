//! Classification is supplied by new writers. Existing rows remain NULL audit
//! data: neither payload inspection nor an inferred correspondence default is
//! permitted. The schema transition is atomic under apply_migrations.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
