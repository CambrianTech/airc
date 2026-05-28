//! SeaORM-backed SQLite implementation of [`EventStore`].
//!
//! `open(db_url)` connects and applies pending migrations. Subsequent
//! `append` / `page_recent` / `resume_from` / `latest_cursor` calls
//! hit the same `sea_orm::DatabaseConnection`, which the SeaORM
//! connection pool serialises internally.
//!
//! Performance notes (SQLite specifics, deliberate):
//!   - Connection pool size is left at sea_orm's default (one writer
//!     for SQLite). Concurrent appenders queue at the driver layer
//!     rather than the application layer.
//!   - JSON values are stored as TEXT; SQLite is opinion-free about
//!     it. Postgres-backed deployments would promote to JSONB by
//!     changing only the `[features]` of sea-orm.

use std::path::Path;

use async_trait::async_trait;
use sea_orm::{
    sea_query::{Expr, OnConflict},
    ActiveValue, ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde_json::Value as JsonValue;

use airc_core::{
    identity::Identity,
    transcript::{MentionTarget, TranscriptKind},
    Body, ClientId, EventId, Headers, PeerId, RoomId, TranscriptCursor, TranscriptEvent,
};

use crate::account_registry::{StoredAccountRegistry, StoredAccountRegistryGistSentinel};
use crate::beacon::StoredBeacon;
use crate::entities::{
    account_registry, beacon, beacon_channel, event, local_identity, mesh_identity,
    peer_rotation_audit, peer_trust, refresh_lock, runtime_cursor, subscription,
};
use crate::error::StoreError;
use crate::local_identity::StoredLocalIdentity;
use crate::mesh_identity::StoredMeshIdentity;
use crate::peer_trust::{RotationAuditEntry, StoredPeer};
use crate::refresh_lock::StoredRefreshLockOutcome;
use crate::store::EventStore;
use crate::subscriptions::StoredSubscription;

pub struct SqliteEventStore {
    db: DatabaseConnection,
}

impl SqliteEventStore {
    /// Open an existing SQLite database (or create one) at `db_url`
    /// and run any pending migrations.
    ///
    /// For an in-memory store use `"sqlite::memory:"`; for a file
    /// store use `"sqlite://<path>?mode=rwc"` so SQLite creates the
    /// file if missing.
    pub async fn open(db_url: &str) -> Result<Self, StoreError> {
        let mut opts = ConnectOptions::new(db_url.to_owned());
        // Keep timeouts predictable for tests — long enough to absorb
        // a slow CI box, short enough to fail fast on a bad URL.
        opts.connect_timeout(std::time::Duration::from_secs(5))
            .acquire_timeout(std::time::Duration::from_secs(5))
            .max_connections(1);
        let db = Database::connect(opts).await?;
        // Forward-compatible BOTH directions: apply our pending
        // migrations, but tolerate a DB already migrated AHEAD of this
        // binary (version skew must not brick the store).
        crate::migration::apply_migrations(&db)
            .await
            .map_err(|err| StoreError::Migration(err.to_string()))?;
        Ok(Self { db })
    }

    /// Open a file-backed SQLite store from a filesystem path.
    ///
    /// This keeps platform path rules out of consumers. Windows
    /// paths must be converted to URI-style forward slashes before
    /// handing them to SQLx/SeaORM; callers should not build SQLite
    /// URLs with `Path::display()`.
    pub async fn open_path(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Self::open(&sqlite_file_url(path)).await
    }

    /// Open an ephemeral in-memory store. Convenience for tests.
    pub async fn in_memory() -> Result<Self, StoreError> {
        Self::open("sqlite::memory:").await
    }

    pub async fn try_acquire_refresh_lock(
        &self,
        mesh_identity: &str,
        now_ms: u64,
        refresh_interval_ms: u64,
        holder_pid: u32,
    ) -> Result<StoredRefreshLockOutcome, StoreError> {
        const MAX_ATTEMPTS: usize = 8;
        let held_at_ms = u64_to_i64("refresh_locks.held_at_ms", now_ms)?;
        let holder_pid = u64_to_i64("refresh_locks.holder_pid", holder_pid as u64)?;
        let active = refresh_lock::ActiveModel {
            mesh_identity: Set(mesh_identity.to_string()),
            held_at_ms: Set(held_at_ms),
            holder_pid: Set(holder_pid),
        };
        let inserted = refresh_lock::Entity::insert(active)
            .on_conflict(
                OnConflict::column(refresh_lock::Column::MeshIdentity)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await;
        match inserted {
            Ok(_) => return Ok(StoredRefreshLockOutcome::Acquired),
            Err(sea_orm::DbErr::RecordNotInserted) => {}
            Err(error) => return Err(StoreError::Database(error)),
        }

        for _ in 0..MAX_ATTEMPTS {
            let Some(existing) = refresh_lock::Entity::find_by_id(mesh_identity.to_string())
                .one(&self.db)
                .await?
            else {
                continue;
            };
            let existing_held_at = i64_to_u64("refresh_locks.held_at_ms", existing.held_at_ms)?;
            if now_ms.saturating_sub(existing_held_at) < refresh_interval_ms {
                return Ok(StoredRefreshLockOutcome::HeldFresh {
                    held_at_ms: existing_held_at,
                });
            }

            let result = refresh_lock::Entity::update_many()
                .col_expr(refresh_lock::Column::HeldAtMs, Expr::value(held_at_ms))
                .col_expr(refresh_lock::Column::HolderPid, Expr::value(holder_pid))
                .filter(refresh_lock::Column::MeshIdentity.eq(mesh_identity))
                .filter(refresh_lock::Column::HeldAtMs.eq(existing.held_at_ms))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 1 {
                return Ok(StoredRefreshLockOutcome::Acquired);
            }
        }

        Ok(StoredRefreshLockOutcome::HeldFresh { held_at_ms: now_ms })
    }

    pub async fn release_refresh_lock(&self, mesh_identity: &str) -> Result<(), StoreError> {
        refresh_lock::Entity::delete_by_id(mesh_identity.to_string())
            .exec(&self.db)
            .await?;
        Ok(())
    }

    pub async fn load_peers(&self) -> Result<Vec<StoredPeer>, StoreError> {
        let rows = peer_trust::Entity::find()
            .order_by_asc(peer_trust::Column::AddedAtMs)
            .order_by_asc(peer_trust::Column::PeerId)
            .all(&self.db)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredPeer {
                    peer_id: PeerId::from_uuid(row.peer_id),
                    pubkey_b64: row.pubkey_b64,
                    added_at_ms: i64_to_u64("peer_trust.added_at_ms", row.added_at_ms)?,
                })
            })
            .collect()
    }

    pub async fn add_peer_trust(
        &self,
        peer_id: PeerId,
        pubkey_b64: String,
        added_at_ms: u64,
    ) -> Result<StoredPeer, StoreError> {
        let active = peer_trust::ActiveModel {
            peer_id: Set(peer_id.as_uuid()),
            pubkey_b64: Set(pubkey_b64.clone()),
            added_at_ms: Set(u64_to_i64("peer_trust.added_at_ms", added_at_ms)?),
        };
        let insert = peer_trust::Entity::insert(active)
            .on_conflict(
                OnConflict::column(peer_trust::Column::PeerId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await;
        match insert {
            Ok(_) | Err(sea_orm::DbErr::RecordNotInserted) => {}
            Err(error) => return Err(StoreError::Database(error)),
        }

        let stored = peer_trust::Entity::find_by_id(peer_id.as_uuid())
            .one(&self.db)
            .await?
            .ok_or_else(|| {
                StoreError::Database(sea_orm::DbErr::Custom(
                    "peer_trust insert returned no stored row".to_string(),
                ))
            })?;
        if stored.pubkey_b64 != pubkey_b64 {
            return Err(StoreError::PeerPubkeyConflict {
                peer_id,
                stored_pubkey_b64: stored.pubkey_b64,
                attempted_pubkey_b64: pubkey_b64,
            });
        }
        Ok(StoredPeer {
            peer_id,
            pubkey_b64: stored.pubkey_b64,
            added_at_ms: i64_to_u64("peer_trust.added_at_ms", stored.added_at_ms)?,
        })
    }

    pub async fn replace_peer_trust(
        &self,
        peer_id: PeerId,
        pubkey_b64: String,
        added_at_ms: u64,
    ) -> Result<StoredPeer, StoreError> {
        let active = peer_trust::ActiveModel {
            peer_id: Set(peer_id.as_uuid()),
            pubkey_b64: Set(pubkey_b64.clone()),
            added_at_ms: Set(u64_to_i64("peer_trust.added_at_ms", added_at_ms)?),
        };
        peer_trust::Entity::insert(active)
            .on_conflict(
                OnConflict::column(peer_trust::Column::PeerId)
                    .update_columns([peer_trust::Column::PubkeyB64, peer_trust::Column::AddedAtMs])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(StoredPeer {
            peer_id,
            pubkey_b64,
            added_at_ms,
        })
    }

    pub async fn remove_peer_trust(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<StoredPeer>, StoreError> {
        let txn = self.db.begin().await?;
        let stored = peer_trust::Entity::find_by_id(peer_id.as_uuid())
            .one(&txn)
            .await?;
        let Some(stored) = stored else {
            txn.commit().await?;
            return Ok(None);
        };
        peer_trust::Entity::delete_by_id(peer_id.as_uuid())
            .exec(&txn)
            .await?;
        txn.commit().await?;
        Ok(Some(StoredPeer {
            peer_id,
            pubkey_b64: stored.pubkey_b64,
            added_at_ms: i64_to_u64("peer_trust.added_at_ms", stored.added_at_ms)?,
        }))
    }

    pub async fn append_peer_rotation_audit(
        &self,
        entry: RotationAuditEntry,
    ) -> Result<(), StoreError> {
        let active = peer_rotation_audit::ActiveModel {
            peer_id: Set(entry.peer_id.as_uuid()),
            sequence: Set(u64_to_i64("peer_rotation_audit.sequence", entry.sequence)?),
            prev_pubkey_b64: Set(entry.prev_pubkey_b64),
            next_pubkey_b64: Set(entry.next_pubkey_b64),
            rotated_at_ms: Set(u64_to_i64(
                "peer_rotation_audit.rotated_at_ms",
                entry.rotated_at_ms,
            )?),
            applied_at_ms: Set(u64_to_i64(
                "peer_rotation_audit.applied_at_ms",
                entry.applied_at_ms,
            )?),
        };
        peer_rotation_audit::Entity::insert(active)
            .exec(&self.db)
            .await?;
        Ok(())
    }

    pub async fn peer_rotation_audit(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<RotationAuditEntry>, StoreError> {
        let rows = peer_rotation_audit::Entity::find()
            .filter(peer_rotation_audit::Column::PeerId.eq(peer_id.as_uuid()))
            .order_by_asc(peer_rotation_audit::Column::Sequence)
            .all(&self.db)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RotationAuditEntry {
                    peer_id: PeerId::from_uuid(row.peer_id),
                    prev_pubkey_b64: row.prev_pubkey_b64,
                    next_pubkey_b64: row.next_pubkey_b64,
                    sequence: i64_to_u64("peer_rotation_audit.sequence", row.sequence)?,
                    rotated_at_ms: i64_to_u64(
                        "peer_rotation_audit.rotated_at_ms",
                        row.rotated_at_ms,
                    )?,
                    applied_at_ms: i64_to_u64(
                        "peer_rotation_audit.applied_at_ms",
                        row.applied_at_ms,
                    )?,
                })
            })
            .collect()
    }

    /// Load the singleton local-identity row, if it exists.
    ///
    /// Returns `Ok(None)` for a fresh database; the caller pairs this
    /// with the on-disk `identity.key` to decide whether to load,
    /// generate, or surface a partial-state error.
    pub async fn load_local_identity(&self) -> Result<Option<StoredLocalIdentity>, StoreError> {
        let row = local_identity::Entity::find_by_id(local_identity::SINGLETON_ID)
            .one(&self.db)
            .await?;
        row.map(|m| {
            Ok(StoredLocalIdentity {
                peer_id: PeerId::from_uuid(m.peer_id),
                client_id: ClientId::from_uuid(m.client_id),
                version: u32::try_from(m.version).map_err(|_| StoreError::InvalidStoredValue {
                    field: "local_identity.version",
                    value: m.version as i128,
                })?,
                created_at_ms: i64_to_u64("local_identity.created_at_ms", m.created_at_ms)?,
                identity: identity_from_row(&m),
            })
        })
        .transpose()
    }

    /// Persist the singleton local-identity row. Insert-only by
    /// design — there is no `replace_local_identity` because a
    /// peer_id / client_id change IS a new identity, not an update.
    /// Calling this with a row already present returns an error
    /// from the singleton CHECK + PK collision.
    pub async fn insert_local_identity(
        &self,
        identity: StoredLocalIdentity,
    ) -> Result<(), StoreError> {
        let active = local_identity::ActiveModel {
            id: Set(local_identity::SINGLETON_ID),
            peer_id: Set(identity.peer_id.as_uuid()),
            client_id: Set(identity.client_id.as_uuid()),
            version: Set(i32::try_from(identity.version).map_err(|_| {
                StoreError::InvalidStoredValue {
                    field: "local_identity.version",
                    value: identity.version as i128,
                }
            })?),
            created_at_ms: Set(u64_to_i64(
                "local_identity.created_at_ms",
                identity.created_at_ms,
            )?),
            name: Set(identity.identity.name),
            pronouns: Set(identity.identity.pronouns),
            role: Set(identity.identity.role),
            bio: Set(identity.identity.bio),
            status: Set(identity.identity.status),
            fingerprint: Set(identity.identity.fingerprint),
            integrations_json: Set(serde_json::to_value(identity.identity.integrations)?),
        };
        local_identity::Entity::insert(active)
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// Replace the user-facing identity card on the singleton row.
    /// Peer/client ids are not touched.
    pub async fn save_local_identity_card(&self, identity: Identity) -> Result<(), StoreError> {
        let row = local_identity::Entity::find_by_id(local_identity::SINGLETON_ID)
            .one(&self.db)
            .await?
            .ok_or(StoreError::NotFound("local_identity"))?;
        let mut active: local_identity::ActiveModel = row.into();
        active.name = Set(identity.name);
        active.pronouns = Set(identity.pronouns);
        active.role = Set(identity.role);
        active.bio = Set(identity.bio);
        active.status = Set(identity.status);
        active.fingerprint = Set(identity.fingerprint);
        active.integrations_json = Set(serde_json::to_value(identity.integrations)?);
        local_identity::Entity::update(active)
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// Load the cached account-registry document for a given mesh
    /// identity, if any. Returns `Ok(None)` when this scope has
    /// never published-or-refreshed a document for that identity.
    pub async fn load_account_registry(
        &self,
        mesh_identity: &str,
    ) -> Result<Option<StoredAccountRegistry>, StoreError> {
        let row = account_registry::document::Entity::find_by_id(mesh_identity.to_string())
            .one(&self.db)
            .await?;
        row.map(|m| {
            Ok(StoredAccountRegistry {
                mesh_identity: m.mesh_identity,
                schema_version: u16::try_from(m.schema_version).map_err(|_| {
                    StoreError::InvalidStoredValue {
                        field: "account_registry.schema_version",
                        value: m.schema_version as i128,
                    }
                })?,
                generated_at_ms: i64_to_u64("account_registry.generated_at_ms", m.generated_at_ms)?,
                document_json: m.document_json,
                updated_at_ms: i64_to_u64("account_registry.updated_at_ms", m.updated_at_ms)?,
            })
        })
        .transpose()
    }

    /// Upsert the cached account-registry document for a given mesh
    /// identity. Idempotent on the document body; the `updated_at_ms`
    /// stamp always reflects the most recent save.
    pub async fn save_account_registry(
        &self,
        document: StoredAccountRegistry,
    ) -> Result<(), StoreError> {
        let active = account_registry::document::ActiveModel {
            mesh_identity: Set(document.mesh_identity.clone()),
            schema_version: Set(i32::from(document.schema_version)),
            generated_at_ms: Set(u64_to_i64(
                "account_registry.generated_at_ms",
                document.generated_at_ms,
            )?),
            document_json: Set(document.document_json),
            updated_at_ms: Set(u64_to_i64(
                "account_registry.updated_at_ms",
                document.updated_at_ms,
            )?),
        };
        account_registry::document::Entity::insert(active)
            .on_conflict(
                OnConflict::column(account_registry::document::Column::MeshIdentity)
                    .update_columns([
                        account_registry::document::Column::SchemaVersion,
                        account_registry::document::Column::GeneratedAtMs,
                        account_registry::document::Column::DocumentJson,
                        account_registry::document::Column::UpdatedAtMs,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// Load the gh-gist sentinel for a given mesh identity, if any.
    pub async fn load_account_registry_gist_sentinel(
        &self,
        mesh_identity: &str,
    ) -> Result<Option<StoredAccountRegistryGistSentinel>, StoreError> {
        let row = account_registry::gist_sentinel::Entity::find_by_id(mesh_identity.to_string())
            .one(&self.db)
            .await?;
        row.map(|m| {
            Ok(StoredAccountRegistryGistSentinel {
                mesh_identity: m.mesh_identity,
                gist_id: m.gist_id,
                updated_at_ms: i64_to_u64(
                    "account_registry_gist_sentinel.updated_at_ms",
                    m.updated_at_ms,
                )?,
            })
        })
        .transpose()
    }

    /// Delete the gh-gist sentinel for a given mesh identity.
    /// Used when the remote gist was edited-or-deleted out of band
    /// and the local recording is stale.
    pub async fn clear_account_registry_gist_sentinel(
        &self,
        mesh_identity: &str,
    ) -> Result<(), StoreError> {
        account_registry::gist_sentinel::Entity::delete_by_id(mesh_identity.to_string())
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// Upsert the gh-gist sentinel for a given mesh identity.
    pub async fn save_account_registry_gist_sentinel(
        &self,
        sentinel: StoredAccountRegistryGistSentinel,
    ) -> Result<(), StoreError> {
        let active = account_registry::gist_sentinel::ActiveModel {
            mesh_identity: Set(sentinel.mesh_identity.clone()),
            gist_id: Set(sentinel.gist_id),
            updated_at_ms: Set(u64_to_i64(
                "account_registry_gist_sentinel.updated_at_ms",
                sentinel.updated_at_ms,
            )?),
        };
        account_registry::gist_sentinel::Entity::insert(active)
            .on_conflict(
                OnConflict::column(account_registry::gist_sentinel::Column::MeshIdentity)
                    .update_columns([
                        account_registry::gist_sentinel::Column::GistId,
                        account_registry::gist_sentinel::Column::UpdatedAtMs,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

fn identity_from_row(row: &local_identity::Model) -> Identity {
    Identity {
        name: row.name.clone(),
        pronouns: row.pronouns.clone(),
        role: row.role.clone(),
        bio: row.bio.clone(),
        status: row.status.clone(),
        fingerprint: row.fingerprint.clone(),
        integrations: serde_json::from_value(row.integrations_json.clone()).unwrap_or_default(),
    }
}

fn u64_to_i64(field: &'static str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidStoredValue {
        field,
        value: value as i128,
    })
}

fn i64_to_u64(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStoredValue {
        field,
        value: value as i128,
    })
}

fn sqlite_file_url(path: &Path) -> String {
    let raw = normalise_sqlite_path(path);
    if has_windows_drive_prefix(&raw) {
        format!("sqlite:{raw}?mode=rwc")
    } else {
        format!("sqlite://{raw}?mode=rwc")
    }
}

fn normalise_sqlite_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.strip_prefix("//?/").unwrap_or(&raw).to_string()
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

#[async_trait]
impl EventStore for SqliteEventStore {
    async fn load_local_identity(&self) -> Result<Option<StoredLocalIdentity>, StoreError> {
        SqliteEventStore::load_local_identity(self).await
    }

    async fn insert_local_identity(&self, identity: StoredLocalIdentity) -> Result<(), StoreError> {
        SqliteEventStore::insert_local_identity(self, identity).await
    }

    async fn save_local_identity_card(&self, identity: Identity) -> Result<(), StoreError> {
        SqliteEventStore::save_local_identity_card(self, identity).await
    }

    async fn append(&self, ev: TranscriptEvent) -> Result<(), StoreError> {
        let active = to_active_model(&ev)?;
        // Insert with explicit "do nothing on conflict" so a replay
        // surfaces as DuplicateEventId rather than a generic DbErr.
        let res = event::Entity::insert(active)
            .on_conflict(
                OnConflict::column(event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await;
        match res {
            Ok(_) => {
                // Distinguish a true insert from a no-op DO-NOTHING:
                // re-query by event_id and compare.
                let existing = event::Entity::find_by_id(ev.event_id.as_uuid())
                    .one(&self.db)
                    .await?;
                match existing {
                    Some(row) if row.lamport == ev.lamport as i64 => Ok(()),
                    Some(_) => Err(StoreError::DuplicateEventId(ev.event_id.as_uuid())),
                    None => Err(StoreError::Database(sea_orm::DbErr::Custom(
                        "post-insert lookup returned no row".to_string(),
                    ))),
                }
            }
            Err(sea_orm::DbErr::RecordNotInserted) => {
                Err(StoreError::DuplicateEventId(ev.event_id.as_uuid()))
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    async fn page_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        // "Newest N" = order DESC by (lamport, event_id), take N,
        // then reverse so the caller iterates oldest → newest.
        let mut query = event::Entity::find()
            .order_by_desc(event::Column::Lamport)
            .order_by_desc(event::Column::EventId)
            .limit(limit as u64);
        if let Some(room) = channel {
            query = query.filter(event::Column::RoomId.eq(room.as_uuid()));
        }
        let mut rows = query.all(&self.db).await?;
        rows.reverse();
        rows.into_iter().map(from_model).collect()
    }

    async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        // Strictly after the cursor: lamport > c.lamport
        //   OR (lamport == c.lamport AND event_id > c.event_id).
        // Two-key tiebreak so events with identical lamports remain
        // ordered deterministically.
        let cursor_lamport = cursor.lamport as i64;
        let cursor_event_id = cursor.event_id.as_uuid();
        let strictly_after = Expr::col(event::Column::Lamport)
            .gt(cursor_lamport)
            .or(Expr::col(event::Column::Lamport)
                .eq(cursor_lamport)
                .and(Expr::col(event::Column::EventId).gt(cursor_event_id)));
        let mut query = event::Entity::find()
            .filter(strictly_after)
            .order_by_asc(event::Column::Lamport)
            .order_by_asc(event::Column::EventId)
            .limit(limit as u64);
        if let Some(room) = channel {
            query = query.filter(event::Column::RoomId.eq(room.as_uuid()));
        }
        let rows = query.all(&self.db).await?;
        rows.into_iter().map(from_model).collect()
    }

    async fn latest_cursor(
        &self,
        channel: Option<RoomId>,
    ) -> Result<Option<TranscriptCursor>, StoreError> {
        let mut query = event::Entity::find()
            .order_by_desc(event::Column::Lamport)
            .order_by_desc(event::Column::EventId)
            .limit(1);
        if let Some(room) = channel {
            query = query.filter(event::Column::RoomId.eq(room.as_uuid()));
        }
        let row = query.one(&self.db).await?;
        Ok(row.map(|m| TranscriptCursor {
            lamport: m.lamport as u64,
            event_id: EventId::from_uuid(m.event_id),
        }))
    }

    async fn load_runtime_cursor(
        &self,
        consumer_id: &str,
    ) -> Result<Option<TranscriptCursor>, StoreError> {
        let row = runtime_cursor::Entity::find_by_id(consumer_id.to_string())
            .one(&self.db)
            .await?;
        Ok(row.map(|m| TranscriptCursor {
            lamport: m.lamport as u64,
            event_id: EventId::from_uuid(m.event_id),
        }))
    }

    async fn save_runtime_cursor(
        &self,
        consumer_id: &str,
        cursor: &TranscriptCursor,
        updated_at_ms: u64,
    ) -> Result<(), StoreError> {
        let active = runtime_cursor::ActiveModel {
            consumer_id: ActiveValue::Set(consumer_id.to_string()),
            lamport: ActiveValue::Set(cursor.lamport as i64),
            event_id: ActiveValue::Set(cursor.event_id.as_uuid()),
            updated_at_ms: ActiveValue::Set(updated_at_ms as i64),
        };
        runtime_cursor::Entity::insert(active)
            .on_conflict(
                OnConflict::column(runtime_cursor::Column::ConsumerId)
                    .update_columns([
                        runtime_cursor::Column::Lamport,
                        runtime_cursor::Column::EventId,
                        runtime_cursor::Column::UpdatedAtMs,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn load_subscriptions(&self) -> Result<Vec<StoredSubscription>, StoreError> {
        let rows = subscription::Entity::find()
            .order_by_asc(subscription::Column::ChannelName)
            .all(&self.db)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredSubscription {
                    channel_name: row.channel_name,
                    room_id: RoomId::from_uuid(row.room_id),
                    wire: row.wire,
                    joined_at_ms: i64_to_u64("subscriptions.joined_at_ms", row.joined_at_ms)?,
                    is_default: row.is_default,
                    parted: row.parted,
                })
            })
            .collect()
    }

    async fn replace_subscriptions(&self, rows: Vec<StoredSubscription>) -> Result<(), StoreError> {
        let txn = self.db.begin().await?;
        subscription::Entity::delete_many().exec(&txn).await?;
        for row in rows {
            let active = subscription::ActiveModel {
                channel_name: ActiveValue::Set(row.channel_name),
                room_id: ActiveValue::Set(row.room_id.as_uuid()),
                wire: ActiveValue::Set(row.wire),
                joined_at_ms: ActiveValue::Set(u64_to_i64(
                    "subscriptions.joined_at_ms",
                    row.joined_at_ms,
                )?),
                is_default: ActiveValue::Set(row.is_default),
                parted: ActiveValue::Set(row.parted),
            };
            subscription::Entity::insert(active).exec(&txn).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn load_mesh_identity(
        &self,
        scope: &str,
    ) -> Result<Option<StoredMeshIdentity>, StoreError> {
        let row = mesh_identity::Entity::find_by_id(scope.to_string())
            .one(&self.db)
            .await?;
        row.map(|row| {
            Ok(StoredMeshIdentity {
                scope: row.scope,
                identity: row.identity,
                source: row.source,
                resolved_at_ms: i64_to_u64("mesh_identity.resolved_at_ms", row.resolved_at_ms)?,
                ttl_ms: i64_to_u64("mesh_identity.ttl_ms", row.ttl_ms)?,
            })
        })
        .transpose()
    }

    async fn save_mesh_identity(&self, entry: StoredMeshIdentity) -> Result<(), StoreError> {
        let active = mesh_identity::ActiveModel {
            scope: ActiveValue::Set(entry.scope),
            identity: ActiveValue::Set(entry.identity),
            source: ActiveValue::Set(entry.source),
            resolved_at_ms: ActiveValue::Set(u64_to_i64(
                "mesh_identity.resolved_at_ms",
                entry.resolved_at_ms,
            )?),
            ttl_ms: ActiveValue::Set(u64_to_i64("mesh_identity.ttl_ms", entry.ttl_ms)?),
        };
        mesh_identity::Entity::insert(active)
            .on_conflict(
                OnConflict::column(mesh_identity::Column::Scope)
                    .update_columns([
                        mesh_identity::Column::Identity,
                        mesh_identity::Column::Source,
                        mesh_identity::Column::ResolvedAtMs,
                        mesh_identity::Column::TtlMs,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn load_beacon(
        &self,
        mesh_identity: &str,
        peer_id: PeerId,
    ) -> Result<Option<StoredBeacon>, StoreError> {
        let row = beacon::Entity::find()
            .filter(beacon::Column::MeshIdentity.eq(mesh_identity))
            .filter(beacon::Column::PeerId.eq(peer_id.as_uuid()))
            .one(&self.db)
            .await?;
        match row {
            Some(row) => Ok(Some(self.beacon_from_model(row).await?)),
            None => Ok(None),
        }
    }

    async fn list_beacons(&self, mesh_identity: &str) -> Result<Vec<StoredBeacon>, StoreError> {
        let rows = beacon::Entity::find()
            .filter(beacon::Column::MeshIdentity.eq(mesh_identity))
            .order_by_asc(beacon::Column::PeerId)
            .all(&self.db)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.beacon_from_model(row).await?);
        }
        Ok(out)
    }

    async fn save_beacon(&self, entry: StoredBeacon) -> Result<(), StoreError> {
        let txn = self.db.begin().await?;
        let peer_uuid = entry.peer_id.as_uuid();
        let active = beacon::ActiveModel {
            mesh_identity: ActiveValue::Set(entry.mesh_identity.clone()),
            peer_id: ActiveValue::Set(peer_uuid),
            scope_home: ActiveValue::Set(entry.scope_home),
            pid: ActiveValue::Set(i64::from(entry.pid)),
            published_at_ms: ActiveValue::Set(u64_to_i64(
                "beacons.published_at_ms",
                entry.published_at_ms,
            )?),
            heartbeat_at_ms: ActiveValue::Set(u64_to_i64(
                "beacons.heartbeat_at_ms",
                entry.heartbeat_at_ms,
            )?),
        };
        beacon::Entity::insert(active)
            .on_conflict(
                OnConflict::columns([beacon::Column::MeshIdentity, beacon::Column::PeerId])
                    .update_columns([
                        beacon::Column::ScopeHome,
                        beacon::Column::Pid,
                        beacon::Column::PublishedAtMs,
                        beacon::Column::HeartbeatAtMs,
                    ])
                    .to_owned(),
            )
            .exec(&txn)
            .await?;
        beacon_channel::Entity::delete_many()
            .filter(beacon_channel::Column::MeshIdentity.eq(&entry.mesh_identity))
            .filter(beacon_channel::Column::PeerId.eq(peer_uuid))
            .exec(&txn)
            .await?;
        let mut channels = entry.subscribed_channels;
        channels.sort();
        channels.dedup();
        for channel_name in channels {
            let active = beacon_channel::ActiveModel {
                mesh_identity: ActiveValue::Set(entry.mesh_identity.clone()),
                peer_id: ActiveValue::Set(peer_uuid),
                channel_name: ActiveValue::Set(channel_name),
            };
            beacon_channel::Entity::insert(active).exec(&txn).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn delete_beacons(
        &self,
        mesh_identity: &str,
        peer_ids: &[PeerId],
    ) -> Result<usize, StoreError> {
        let txn = self.db.begin().await?;
        let mut removed = 0;
        for peer_id in peer_ids {
            let peer_uuid = peer_id.as_uuid();
            beacon_channel::Entity::delete_many()
                .filter(beacon_channel::Column::MeshIdentity.eq(mesh_identity))
                .filter(beacon_channel::Column::PeerId.eq(peer_uuid))
                .exec(&txn)
                .await?;
            let result = beacon::Entity::delete_many()
                .filter(beacon::Column::MeshIdentity.eq(mesh_identity))
                .filter(beacon::Column::PeerId.eq(peer_uuid))
                .exec(&txn)
                .await?;
            removed += result.rows_affected as usize;
        }
        txn.commit().await?;
        Ok(removed)
    }

    async fn try_acquire_refresh_lock(
        &self,
        mesh_identity: &str,
        now_ms: u64,
        refresh_interval_ms: u64,
        holder_pid: u32,
    ) -> Result<StoredRefreshLockOutcome, StoreError> {
        SqliteEventStore::try_acquire_refresh_lock(
            self,
            mesh_identity,
            now_ms,
            refresh_interval_ms,
            holder_pid,
        )
        .await
    }

    async fn release_refresh_lock(&self, mesh_identity: &str) -> Result<(), StoreError> {
        SqliteEventStore::release_refresh_lock(self, mesh_identity).await
    }
}

impl SqliteEventStore {
    async fn beacon_from_model(&self, row: beacon::Model) -> Result<StoredBeacon, StoreError> {
        let channels = beacon_channel::Entity::find()
            .filter(beacon_channel::Column::MeshIdentity.eq(&row.mesh_identity))
            .filter(beacon_channel::Column::PeerId.eq(row.peer_id))
            .order_by_asc(beacon_channel::Column::ChannelName)
            .all(&self.db)
            .await?
            .into_iter()
            .map(|row| row.channel_name)
            .collect();
        Ok(StoredBeacon {
            mesh_identity: row.mesh_identity,
            peer_id: PeerId::from_uuid(row.peer_id),
            scope_home: row.scope_home,
            subscribed_channels: channels,
            pid: u32::try_from(row.pid).map_err(|_| StoreError::InvalidStoredValue {
                field: "beacons.pid",
                value: row.pid as i128,
            })?,
            published_at_ms: i64_to_u64("beacons.published_at_ms", row.published_at_ms)?,
            heartbeat_at_ms: i64_to_u64("beacons.heartbeat_at_ms", row.heartbeat_at_ms)?,
        })
    }
}

fn to_active_model(ev: &TranscriptEvent) -> Result<event::ActiveModel, StoreError> {
    // The kind ↔ str mapping lives on `TranscriptKind` itself
    // (single source of truth, kink 0cfcc8db). Adding a variant
    // here used to require a parallel edit; now the compiler
    // enforces it inside airc-core's own `as_wire_str` match, and
    // airc-core's `wire_str_round_trip_covers_every_variant` test
    // catches any drift before this codec is even invoked.
    let kind = ev.kind.as_wire_str();
    Ok(event::ActiveModel {
        event_id: ActiveValue::Set(ev.event_id.as_uuid()),
        room_id: ActiveValue::Set(ev.room_id.as_uuid()),
        peer_id: ActiveValue::Set(ev.peer_id.as_uuid()),
        client_id: ActiveValue::Set(ev.client_id.as_uuid()),
        kind: ActiveValue::Set(kind.to_owned()),
        occurred_at_ms: ActiveValue::Set(ev.occurred_at_ms as i64),
        lamport: ActiveValue::Set(ev.lamport as i64),
        target: ActiveValue::Set(serde_json::to_value(&ev.target)?),
        headers: ActiveValue::Set(serde_json::to_value(&ev.headers)?),
        body: ActiveValue::Set(match &ev.body {
            Some(b) => Some(serde_json::to_value(b)?),
            None => None,
        }),
        attachment: ActiveValue::Set(match &ev.attachment {
            Some(a) => Some(serde_json::to_value(a)?),
            None => None,
        }),
        receipt: ActiveValue::Set(match &ev.receipt {
            Some(r) => Some(serde_json::to_value(r)?),
            None => None,
        }),
        metadata: ActiveValue::Set(ev.metadata.clone()),
    })
}

fn from_model(m: event::Model) -> Result<TranscriptEvent, StoreError> {
    // Reverse of `to_active_model`'s `as_wire_str`. Unknown rows
    // surface as `UnknownTranscriptKind` so callers can distinguish
    // "store corruption / schema drift" from a normal decode.
    let kind = TranscriptKind::from_wire_str(&m.kind)
        .ok_or_else(|| StoreError::UnknownTranscriptKind(m.kind.clone()))?;
    let target: MentionTarget = serde_json::from_value(m.target)?;
    let headers: Headers = serde_json::from_value(m.headers)?;
    let body: Option<Body> = match m.body {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    };
    let attachment = match m.attachment {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    };
    let receipt = match m.receipt {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    };
    Ok(TranscriptEvent {
        event_id: EventId::from_uuid(m.event_id),
        room_id: RoomId::from_uuid(m.room_id),
        peer_id: PeerId::from_uuid(m.peer_id),
        client_id: ClientId::from_uuid(m.client_id),
        kind,
        occurred_at_ms: m.occurred_at_ms as u64,
        lamport: m.lamport as u64,
        target,
        headers,
        body,
        attachment,
        receipt,
        metadata: m.metadata,
    })
}

/// Silence the unused-import warning if `JsonValue` is needed later
/// without forcing an import-shuffle.
#[allow(dead_code)]
fn _json_value_keepalive(_: JsonValue) {}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::transcript::MentionTarget;
    use serde_json::json;
    use uuid::Uuid;

    fn make_event(lamport: u64, room: RoomId, body: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: room,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    fn model_with_kind(kind: &str) -> event::Model {
        event::Model {
            event_id: Uuid::new_v4(),
            room_id: Uuid::new_v4(),
            peer_id: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            kind: kind.to_string(),
            occurred_at_ms: 1_700_000_000_000,
            lamport: 1,
            target: json!(MentionTarget::All),
            headers: json!(Headers::new()),
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn unknown_transcript_kind_fails_closed() {
        let result = from_model(model_with_kind("future_kind"));

        assert!(
            matches!(result, Err(StoreError::UnknownTranscriptKind(ref kind)) if kind == "future_kind"),
            "expected UnknownTranscriptKind, got {result:?}"
        );
    }

    #[tokio::test]
    async fn append_then_page_recent_round_trips() {
        // The minimum-viable proof: write one, read one back, body
        // intact, identity preserved.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let ev = make_event(1, room, "hello");
        store.append(ev.clone()).await.unwrap();

        let page = store.page_recent(Some(room), 10).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0], ev);
    }

    #[tokio::test]
    async fn local_identity_card_round_trips_through_store_trait() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let store_api: &dyn EventStore = &store;
        let mut identity = Identity::new("alice");
        identity.pronouns = "they".into();
        identity.role = "substrate".into();
        identity
            .integrations
            .insert("continuum".into(), "clio".into());

        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa1),
                client_id: ClientId::from_u128(0xc1),
                version: 1,
                created_at_ms: 42,
                identity,
            })
            .await
            .unwrap();

        let mut updated = Identity::new("alice");
        updated.status = "working".into();
        updated
            .integrations
            .insert("openclaw".into(), "alice-ui".into());
        store_api
            .save_local_identity_card(updated.clone())
            .await
            .unwrap();

        let stored = store_api
            .load_local_identity()
            .await
            .unwrap()
            .expect("local identity row");
        assert_eq!(stored.peer_id, PeerId::from_u128(0xa1));
        assert_eq!(stored.client_id, ClientId::from_u128(0xc1));
        assert_eq!(stored.identity, updated);
    }

    #[tokio::test]
    async fn page_recent_returns_oldest_to_newest_with_limit() {
        // Substrate contract: page is ordered oldest → newest within
        // the returned slice, AND a limit smaller than the total set
        // keeps the newest end of the log.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        for i in 1..=5u64 {
            store
                .append(make_event(i, room, &format!("msg{i}")))
                .await
                .unwrap();
        }
        let page = store.page_recent(Some(room), 3).await.unwrap();
        assert_eq!(page.len(), 3);
        let lamports: Vec<u64> = page.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![3, 4, 5], "newest 3, oldest-first");
    }

    #[tokio::test]
    async fn resume_from_returns_strictly_after_cursor() {
        // The cursor semantics that grievance §7 requires: events AT
        // OR BEFORE the cursor are excluded; everything strictly
        // after is returned in transcript order.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let mut events = Vec::new();
        for i in 1..=5u64 {
            let ev = make_event(i, room, &format!("msg{i}"));
            events.push(ev.clone());
            store.append(ev).await.unwrap();
        }
        let cursor = events[2].cursor(); // lamport=3
        let after = store.resume_from(&cursor, Some(room), 10).await.unwrap();
        let lamports: Vec<u64> = after.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![4, 5]);
    }

    #[tokio::test]
    async fn channel_filter_isolates_rooms() {
        // page_recent / resume_from with channel = Some(room) must
        // not leak events from other rooms — even when the global
        // log interleaves them. Grievance §7: "no cross-room leakage
        // when wires are shared".
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room_a = RoomId::from_u128(0xaaaa);
        let room_b = RoomId::from_u128(0xbbbb);
        store.append(make_event(1, room_a, "a-1")).await.unwrap();
        store.append(make_event(2, room_b, "b-1")).await.unwrap();
        store.append(make_event(3, room_a, "a-2")).await.unwrap();
        store.append(make_event(4, room_b, "b-2")).await.unwrap();

        let room_a_page = store.page_recent(Some(room_a), 10).await.unwrap();
        let room_a_bodies: Vec<&str> = room_a_page
            .iter()
            .filter_map(|e| e.body.as_ref().and_then(Body::as_text))
            .collect();
        assert_eq!(room_a_bodies, vec!["a-1", "a-2"]);

        let room_b_page = store.page_recent(Some(room_b), 10).await.unwrap();
        let room_b_bodies: Vec<&str> = room_b_page
            .iter()
            .filter_map(|e| e.body.as_ref().and_then(Body::as_text))
            .collect();
        assert_eq!(room_b_bodies, vec!["b-1", "b-2"]);
    }

    #[tokio::test]
    async fn latest_cursor_reflects_newest_event_per_channel() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room_a = RoomId::from_u128(0xaaaa);
        let room_b = RoomId::from_u128(0xbbbb);
        assert!(store.latest_cursor(Some(room_a)).await.unwrap().is_none());

        let a1 = make_event(1, room_a, "a-1");
        let b1 = make_event(2, room_b, "b-1");
        let a2 = make_event(3, room_a, "a-2");
        store.append(a1.clone()).await.unwrap();
        store.append(b1.clone()).await.unwrap();
        store.append(a2.clone()).await.unwrap();

        let latest_a = store.latest_cursor(Some(room_a)).await.unwrap().unwrap();
        assert_eq!(latest_a, a2.cursor());
        let latest_b = store.latest_cursor(Some(room_b)).await.unwrap().unwrap();
        assert_eq!(latest_b, b1.cursor());
        let latest_global = store.latest_cursor(None).await.unwrap().unwrap();
        assert_eq!(latest_global, a2.cursor());
    }

    #[tokio::test]
    async fn runtime_cursor_upserts_by_consumer_id() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let first = TranscriptCursor {
            lamport: 7,
            event_id: EventId::from_u128(0x7),
        };
        let second = TranscriptCursor {
            lamport: 9,
            event_id: EventId::from_u128(0x9),
        };

        assert!(store
            .load_runtime_cursor("codex-hook:default")
            .await
            .unwrap()
            .is_none());

        store
            .save_runtime_cursor("codex-hook:default", &first, 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(
            store
                .load_runtime_cursor("codex-hook:default")
                .await
                .unwrap(),
            Some(first.clone())
        );

        store
            .save_runtime_cursor("codex-hook:default", &second, 1_700_000_000_100)
            .await
            .unwrap();
        assert_eq!(
            store
                .load_runtime_cursor("codex-hook:default")
                .await
                .unwrap(),
            Some(second)
        );
    }

    #[tokio::test]
    async fn runtime_cursors_are_isolated_by_consumer_id() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let hook = TranscriptCursor {
            lamport: 3,
            event_id: EventId::from_u128(0x3),
        };
        let join = TranscriptCursor {
            lamport: 4,
            event_id: EventId::from_u128(0x4),
        };

        store
            .save_runtime_cursor("codex-hook:default", &hook, 1)
            .await
            .unwrap();
        store
            .save_runtime_cursor("join-feed:codex:thread-1", &join, 2)
            .await
            .unwrap();

        assert_eq!(
            store
                .load_runtime_cursor("codex-hook:default")
                .await
                .unwrap(),
            Some(hook)
        );
        assert_eq!(
            store
                .load_runtime_cursor("join-feed:codex:thread-1")
                .await
                .unwrap(),
            Some(join)
        );
    }

    #[tokio::test]
    async fn subscriptions_round_trip_through_orm_table() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let rows = vec![
            StoredSubscription {
                channel_name: "general".to_string(),
                room_id: RoomId::from_u128(0x100),
                wire: "/tmp/airc/wires/general".to_string(),
                joined_at_ms: 1_700_000_000_000,
                is_default: false,
                parted: false,
            },
            StoredSubscription {
                channel_name: "cambriantech".to_string(),
                room_id: RoomId::from_u128(0x101),
                wire: "/tmp/airc/wires/cambriantech".to_string(),
                joined_at_ms: 1_700_000_000_010,
                is_default: true,
                parted: false,
            },
            StoredSubscription {
                channel_name: "old-room".to_string(),
                room_id: RoomId::from_u128(0x102),
                wire: String::new(),
                joined_at_ms: 0,
                is_default: false,
                parted: true,
            },
        ];

        let mut expected = rows.clone();
        expected.sort_by(|a, b| a.channel_name.cmp(&b.channel_name));
        store.replace_subscriptions(rows).await.unwrap();

        assert_eq!(store.load_subscriptions().await.unwrap(), expected);
    }

    #[tokio::test]
    async fn subscriptions_persist_across_store_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let rows = vec![StoredSubscription {
            channel_name: "general".to_string(),
            room_id: RoomId::from_u128(0x200),
            wire: dir
                .path()
                .join("wires")
                .join("general")
                .display()
                .to_string(),
            joined_at_ms: 1_700_000_000_000,
            is_default: true,
            parted: false,
        }];

        SqliteEventStore::open_path(&path)
            .await
            .unwrap()
            .replace_subscriptions(rows.clone())
            .await
            .unwrap();

        let reopened = SqliteEventStore::open_path(&path).await.unwrap();
        assert_eq!(reopened.load_subscriptions().await.unwrap(), rows);
    }

    #[tokio::test]
    async fn mesh_identity_upserts_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let first = StoredMeshIdentity {
            scope: "default".to_string(),
            identity: "alice".to_string(),
            source: "operator".to_string(),
            resolved_at_ms: 1,
            ttl_ms: 86_400_000,
        };
        let second = StoredMeshIdentity {
            identity: "bob".to_string(),
            resolved_at_ms: 2,
            ..first.clone()
        };

        let store = SqliteEventStore::open_path(&path).await.unwrap();
        assert!(store.load_mesh_identity("default").await.unwrap().is_none());
        store.save_mesh_identity(first).await.unwrap();
        store.save_mesh_identity(second.clone()).await.unwrap();

        let reopened = SqliteEventStore::open_path(&path).await.unwrap();
        assert_eq!(
            reopened.load_mesh_identity("default").await.unwrap(),
            Some(second)
        );
    }

    #[tokio::test]
    async fn beacons_replace_channels_and_drain_by_peer() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_a = PeerId::from_u128(0xa);
        let peer_b = PeerId::from_u128(0xb);
        let first = StoredBeacon {
            mesh_identity: "joelteply".to_string(),
            peer_id: peer_a,
            scope_home: "/home/joel/.airc".to_string(),
            subscribed_channels: vec!["general".to_string(), "cambriantech".to_string()],
            pid: 42,
            published_at_ms: 1_000,
            heartbeat_at_ms: 1_000,
        };
        let updated = StoredBeacon {
            subscribed_channels: vec!["general".to_string()],
            heartbeat_at_ms: 2_000,
            ..first.clone()
        };
        let other = StoredBeacon {
            mesh_identity: "joelteply".to_string(),
            peer_id: peer_b,
            scope_home: "/home/joel/project/.airc".to_string(),
            subscribed_channels: vec!["ideem".to_string()],
            pid: 43,
            published_at_ms: 1_500,
            heartbeat_at_ms: 1_500,
        };

        store.save_beacon(first).await.unwrap();
        store.save_beacon(updated.clone()).await.unwrap();
        store.save_beacon(other.clone()).await.unwrap();

        assert_eq!(
            store.load_beacon("joelteply", peer_a).await.unwrap(),
            Some(updated)
        );
        let listed = store.list_beacons("joelteply").await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            store.delete_beacons("joelteply", &[peer_a]).await.unwrap(),
            1
        );
        assert!(store
            .load_beacon("joelteply", peer_a)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store.load_beacon("joelteply", peer_b).await.unwrap(),
            Some(other)
        );
    }

    #[tokio::test]
    async fn duplicate_event_id_surfaces_as_typed_error() {
        // Replay-safety: appending the same event_id twice must
        // surface DuplicateEventId, not a generic DB error.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let ev = make_event(1, room, "once");
        store.append(ev.clone()).await.unwrap();

        let second = store.append(ev.clone()).await;
        assert!(
            matches!(second, Err(StoreError::DuplicateEventId(id)) if id == ev.event_id.as_uuid()),
            "expected DuplicateEventId({}), got {second:?}",
            ev.event_id.as_uuid()
        );
    }

    #[tokio::test]
    async fn cursor_tiebreaks_on_event_id_at_same_lamport() {
        // Two events with the same lamport must page in event_id
        // order deterministically. Grievance §7 explicitly named
        // this: "cursor = (lamport, event_id) … stronger event-store
        // cursor".
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let mut ev_a = make_event(7, room, "same-lamport-a");
        let mut ev_b = make_event(7, room, "same-lamport-b");
        // Force a deterministic event_id order so the test isn't
        // flaky against UUIDv4 chance.
        ev_a.event_id = EventId::from_u128(0x1);
        ev_b.event_id = EventId::from_u128(0x2);
        store.append(ev_a.clone()).await.unwrap();
        store.append(ev_b.clone()).await.unwrap();

        let page = store.page_recent(Some(room), 10).await.unwrap();
        assert_eq!(page[0].event_id, ev_a.event_id);
        assert_eq!(page[1].event_id, ev_b.event_id);

        // resume_from(ev_a) should return ev_b — strictly after at
        // the same lamport.
        let after = store
            .resume_from(&ev_a.cursor(), Some(room), 10)
            .await
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].event_id, ev_b.event_id);
    }

    #[test]
    fn sqlite_file_url_uses_uri_slashes_for_windows_paths() {
        let path = Path::new(r"C:\Users\agent\.airc\events.sqlite");

        assert_eq!(
            sqlite_file_url(path),
            "sqlite:C:/Users/agent/.airc/events.sqlite?mode=rwc"
        );
    }

    #[test]
    fn sqlite_file_url_strips_windows_verbatim_prefix() {
        let path = Path::new(r"\\?\C:\Users\agent\.airc\events.sqlite");

        assert_eq!(
            sqlite_file_url(path),
            "sqlite:C:/Users/agent/.airc/events.sqlite?mode=rwc"
        );
    }

    #[test]
    fn sqlite_file_url_preserves_unix_absolute_paths() {
        let path = Path::new("/tmp/airc/events.sqlite");

        assert_eq!(
            sqlite_file_url(path),
            "sqlite:///tmp/airc/events.sqlite?mode=rwc"
        );
    }
}
