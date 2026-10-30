#![cfg(test)]

use super::*;
use crate::custody::{Admission, MAINTENANCE_LOCK};
use crate::SqliteDurableSink;
use airc_bus::{DurableSink, Envelope};
use fs2::FileExt;
use std::error::Error;

fn is_ahead(error: &(dyn Error + 'static)) -> bool {
    let mut cursor = Some(error);
    while let Some(error) = cursor {
        if matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::AheadOfBinary { .. })
        ) {
            return true;
        }
        cursor = error.source();
    }
    false
}

fn event() -> Envelope {
    use airc_bus::envelope::{DeliveryClass, Kind};
    Envelope::new(
        RoomId::new(),
        (PeerId::new(), ClientId::new()),
        Kind::Message,
        airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
        DeliveryClass::Durable,
        bytes::Bytes::from_static(b"test"),
    )
}

#[tokio::test]
async fn maintenance_refuses_open_before_either_database_is_created() -> Result<(), Box<dyn Error>>
{
    let dir = tempfile::tempdir()?;
    let lock = crate::custody::lock_file(dir.path(), MAINTENANCE_LOCK)?;
    lock.lock_exclusive()?;
    let admission = || Admission::Writer {
        owner: dir.path().to_owned(),
    };
    let path = dir.path().join("events.sqlite");
    let result = SqliteEventStore::open(&sqlite_file_url(&path), admission()).await;
    let Err(error) = result else {
        panic!("maintenance admitted event store creation")
    };
    assert!(crate::custody::is_maintenance_in_progress(&error));
    assert!(!path.exists());
    let path = dir.path().join("bus.sqlite");
    let result = SqliteDurableSink::open(&sqlite_file_url(&path), admission()).await;
    let Err(error) = result else {
        panic!("maintenance admitted bus store creation")
    };
    assert!(crate::custody::is_maintenance_in_progress(&error));
    assert!(!path.exists());
    Ok(())
}

#[tokio::test]
async fn sink_write_retains_maintenance_cause_and_does_not_append() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let sink = SqliteDurableSink::open(
        &sqlite_file_url(&dir.path().join("bus.sqlite")),
        Admission::Writer {
            owner: dir.path().to_owned(),
        },
    )
    .await?;
    let lock = crate::custody::lock_file(dir.path(), MAINTENANCE_LOCK)?;
    lock.lock_exclusive()?;
    let event = event();
    let Err(error) = sink.append(&event).await else {
        panic!("maintenance admitted append")
    };
    assert!(crate::custody::is_maintenance_in_progress(&error));
    assert!(!sink.contains(event.event_id).await?);
    Ok(())
}

#[tokio::test]
async fn sink_open_and_write_retain_schema_refusal() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("bus.sqlite");
    let url = sqlite_file_url(&path);
    let sink = SqliteDurableSink::open(&url, Admission::Unmanaged).await?;
    let store = SqliteEventStore::open(&url, Admission::Unmanaged).await?;
    sea_orm_migration::seaql_migrations::Entity::insert(
        sea_orm_migration::seaql_migrations::ActiveModel {
            version: Set("future_contract".into()),
            applied_at: Set(1),
        },
    )
    .exec(&store.db)
    .await?;
    let event = event();
    let Err(error) = sink.append(&event).await else {
        panic!("ahead schema admitted append")
    };
    assert!(is_ahead(&error));
    assert!(!sink.contains(event.event_id).await?);
    let result = SqliteDurableSink::open(&url, Admission::Unmanaged).await;
    let Err(error) = result else {
        panic!("ahead schema admitted open")
    };
    assert!(is_ahead(&error));
    Ok(())
}
