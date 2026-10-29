//! Index who each stored event could be addressed to (card 6fb2ea16).
//!
//! An agent's "requests addressed to you" search scanned bounded tails and
//! filtered in Rust, so under sustained ambient traffic an older request was
//! never reached. Both event tables gain the two derived columns an addressee
//! scan selects on, [`airc_core::Addressing`]: `addressed_peer` (the peer a
//! typed target names) and `leading_mention` (the body text opens with an
//! `@name`). The store writes them at append; the stored event and the signed
//! envelope bytes are untouched.
//!
//! Existing rows are backfilled through the entities, a keyset page at a time:
//! read `(event_id, target, body|payload)`, derive the addressing in Rust with
//! the same function append uses, and update only the rows whose values differ
//! from the column defaults, one `UPDATE .. WHERE event_id IN (..)` per value.

use std::collections::BTreeMap;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use sea_orm_migration::prelude::*;
use uuid::Uuid;

use crate::entities::{bus_event, event};

/// Rows read per backfill page.
const PAGE: u64 = 1000;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [Events::Table.into_iden(), BusEvents::Table.into_iden()] {
            manager
                .alter_table(
                    Table::alter()
                        .table(table.clone())
                        .add_column(ColumnDef::new(Addressing::AddressedPeer).uuid().null())
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(table)
                        .add_column(
                            ColumnDef::new(Addressing::LeadingMention)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }
        // An addressee scan is `room AND (addressed_peer = me OR leading_mention)`
        // in cursor order: one index per OR arm, each in the table's cursor order.
        for (name, column) in [
            ("idx_events_room_addressed_peer", Addressing::AddressedPeer),
            (
                "idx_events_room_leading_mention",
                Addressing::LeadingMention,
            ),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(Events::Table)
                        .col(Events::RoomId)
                        .col(column)
                        .col(Events::Lamport)
                        .col(Events::EventId)
                        .to_owned(),
                )
                .await?;
        }
        for (name, column) in [
            (
                "idx_bus_events_room_addressed_peer",
                Addressing::AddressedPeer,
            ),
            (
                "idx_bus_events_room_leading_mention",
                Addressing::LeadingMention,
            ),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(BusEvents::Table)
                        .col(BusEvents::RoomId)
                        .col(column)
                        .col(BusEvents::Epoch)
                        .col(BusEvents::Counter)
                        .col(BusEvents::EventId)
                        .to_owned(),
                )
                .await?;
        }
        let db = manager.get_connection();
        backfill_events(db).await?;
        backfill_bus_events(db).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for name in [
            "idx_events_room_addressed_peer",
            "idx_events_room_leading_mention",
            "idx_bus_events_room_addressed_peer",
            "idx_bus_events_room_leading_mention",
        ] {
            manager
                .drop_index(Index::drop().name(name).to_owned())
                .await?;
        }
        // SQLite takes one alter option per statement.
        for table in [Events::Table.into_iden(), BusEvents::Table.into_iden()] {
            for column in [Addressing::AddressedPeer, Addressing::LeadingMention] {
                manager
                    .alter_table(
                        Table::alter()
                            .table(table.clone())
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

fn decode(what: &str, error: impl std::fmt::Display) -> DbErr {
    DbErr::Custom(format!("addressing backfill: {what}: {error}"))
}

/// The non-default addressing of one page, grouped by value: the rows whose
/// typed target names each peer, and the rows that open with an `@name`.
#[derive(Default)]
struct Derived {
    peers: BTreeMap<Uuid, Vec<Uuid>>,
    leading: Vec<Uuid>,
}

impl Derived {
    fn add(&mut self, id: Uuid, addressing: airc_core::Addressing) {
        if let Some(peer) = addressing.peer {
            self.peers.entry(peer.as_uuid()).or_default().push(id);
        }
        if addressing.leading_mention {
            self.leading.push(id);
        }
    }
}

async fn backfill_events(db: &SchemaManagerConnection<'_>) -> Result<(), DbErr> {
    let mut last: Option<Uuid> = None;
    loop {
        let mut page = event::Entity::find()
            .select_only()
            .column(event::Column::EventId)
            .column(event::Column::Target)
            .column(event::Column::Body)
            .order_by_asc(event::Column::EventId)
            .limit(PAGE);
        if let Some(after) = last {
            page = page.filter(event::Column::EventId.gt(after));
        }
        let rows: Vec<(Uuid, serde_json::Value, Option<serde_json::Value>)> =
            page.into_tuple().all(db).await?;
        let Some((tail, _, _)) = rows.last() else {
            return Ok(());
        };
        last = Some(*tail);
        let mut derived = Derived::default();
        for (id, target, body) in rows {
            let target: airc_core::MentionTarget =
                serde_json::from_value(target).map_err(|e| decode("events.target", e))?;
            let body: Option<airc_core::Body> = body
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| decode("events.body", e))?;
            derived.add(id, airc_core::Addressing::of_target(&target, body.as_ref()));
        }
        for (peer, ids) in derived.peers {
            event::Entity::update_many()
                .col_expr(event::Column::AddressedPeer, Expr::value(peer))
                .filter(event::Column::EventId.is_in(ids))
                .exec(db)
                .await?;
        }
        if !derived.leading.is_empty() {
            event::Entity::update_many()
                .col_expr(event::Column::LeadingMention, Expr::value(true))
                .filter(event::Column::EventId.is_in(derived.leading))
                .exec(db)
                .await?;
        }
    }
}

async fn backfill_bus_events(db: &SchemaManagerConnection<'_>) -> Result<(), DbErr> {
    let mut last: Option<Uuid> = None;
    loop {
        let mut page = bus_event::Entity::find()
            .select_only()
            .column(bus_event::Column::EventId)
            .column(bus_event::Column::Target)
            .column(bus_event::Column::Payload)
            .order_by_asc(bus_event::Column::EventId)
            .limit(PAGE);
        if let Some(after) = last {
            page = page.filter(bus_event::Column::EventId.gt(after));
        }
        let rows: Vec<(Uuid, serde_json::Value, Vec<u8>)> = page.into_tuple().all(db).await?;
        let Some((tail, _, _)) = rows.last() else {
            return Ok(());
        };
        last = Some(*tail);
        let mut derived = Derived::default();
        for (id, target, payload) in rows {
            let target: airc_bus::Target =
                serde_json::from_value(target).map_err(|e| decode("bus_events.target", e))?;
            derived.add(id, airc_bus::addressing_of(&target, &payload));
        }
        for (peer, ids) in derived.peers {
            bus_event::Entity::update_many()
                .col_expr(bus_event::Column::AddressedPeer, Expr::value(peer))
                .filter(bus_event::Column::EventId.is_in(ids))
                .exec(db)
                .await?;
        }
        if !derived.leading.is_empty() {
            bus_event::Entity::update_many()
                .col_expr(bus_event::Column::LeadingMention, Expr::value(true))
                .filter(bus_event::Column::EventId.is_in(derived.leading))
                .exec(db)
                .await?;
        }
    }
}

#[derive(DeriveIden)]
enum Events {
    Table,
    RoomId,
    Lamport,
    EventId,
}

#[derive(DeriveIden)]
enum BusEvents {
    Table,
    RoomId,
    Epoch,
    Counter,
    EventId,
}

#[derive(DeriveIden)]
enum Addressing {
    AddressedPeer,
    LeadingMention,
}
