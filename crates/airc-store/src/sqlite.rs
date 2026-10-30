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
    peer_rotation_audit, peer_trust, refresh_lock, runtime_cursor, scoped_state, subscription,
};
use crate::error::StoreError;
use crate::local_identity::StoredLocalIdentity;
use crate::mesh_identity::StoredMeshIdentity;
use crate::peer_trust::{RotationAuditEntry, StoredPeer, TrustTier};
use crate::refresh_lock::StoredRefreshLockOutcome;
use crate::scoped_state::StoredScopedState;
use crate::store::EventStore;
use crate::subscriptions::StoredSubscription;

/// Deadlock backstop for the single-connection pool — NOT a load limit.
///
/// These pools are `max_connections(1)`: concurrent callers queue by
/// design, so a caller waiting is normal operation, not a fault. At the
/// previous 5s this timeout behaved as a load limiter and turned routine
/// queuing into a hard error — it took down `peer_add_with_tier_flag_
/// persists_explicit_tier` on windows CI (2026-08-12) with "pool timed
/// out while waiting for an open connection" during `airc init`, a
/// failure that has nothing to do with the code under test. A backstop
/// should only fire when something is genuinely stuck, so it is sized
/// for the slowest legitimate wait (loaded CI, cold Windows I/O),
/// leaving real deadlocks still bounded.
const POOL_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct SqliteEventStore {
    db: DatabaseConnection,
    /// How this store's writes are admitted ([`crate::custody`]): every write
    /// method takes a ticket for its own length.
    custody: crate::custody::WriteGate,
}

/// The one connection configuration for a store at `db_url` (`"sqlite::memory:"`, or
/// `"sqlite://<path>?mode=rwc"`): every open and the migration recovery use it.
fn connect_options(db_url: &str) -> ConnectOptions {
    let mut opts = ConnectOptions::new(db_url.to_owned());
    // Keep timeouts predictable for tests — long enough to absorb
    // a slow CI box, short enough to fail fast on a bad URL.
    opts.connect_timeout(POOL_ACQUIRE_TIMEOUT)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .max_connections(1);
    // Card 127816bd Phase 1.C — chat throughput.
    //
    // Default SQLite mode is journal=DELETE + synchronous=FULL,
    // which forces a rollback-journal fsync on EVERY append. On
    // macOS APFS that's ~3-15ms per insert; for chat publish (one
    // append per `.say()`) that single fsync IS the per-message
    // hot path — measured at 3.5 ms/op against the empty-WAL
    // baseline in `airc-lib/tests/chat_throughput.rs`.
    //
    // WAL + `synchronous=NORMAL` trades that per-commit fsync
    // for an fsync at WAL checkpoint (every ~1000 pages). Crash
    // semantics: only the un-checkpointed tail can be lost. For
    // airc this is correct: events are content-addressed
    // (event_id is the digest), the wire path replays gaps from
    // peers on reconnect, and the daemon's recover-from-cursor
    // protocol already handles short tails. Same trade `bus_sink`
    // makes for the same reasons.
    //
    // Typed builder, not literal SQL — `SqliteConnectOptions`
    // is sea-orm's ORM-level connection-config surface.
    opts.sqlx_logging(false).map_sqlx_sqlite_opts(|so| {
        use sea_orm::sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
        // A SECOND OPENER WAITS, IT DOES NOT FAIL (2026-09-14). Several processes
        // open one events.sqlite on a machine: the daemon, every attached scope, a
        // test's LAN-gateway handle. Opening runs migrations (DDL = a write lock);
        // sqlx's default busy timeout is 5 s, and on Windows CI with a dozen
        // daemons booting at once the lock outlives it — "database is locked" at
        // attach (airc #1419/#1420; the two-pools-on-one-file flake). WAL was
        // already set; the missing piece was telling SQLite to WAIT as long as
        // the pool itself waits — one bound, one clock.
        so.journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(POOL_ACQUIRE_TIMEOUT)
    });
    opts
}

impl SqliteEventStore {
    /// Open an existing SQLite database (or create one) at `db_url`
    /// under `admission`, and run any pending migrations (one admitted write).
    ///
    /// For an in-memory store use `"sqlite::memory:"`; for a file
    /// store use `"sqlite://<path>?mode=rwc"` so SQLite creates the
    /// file if missing.
    async fn open(db_url: &str, admission: crate::custody::Admission) -> Result<Self, StoreError> {
        let custody = crate::custody::WriteGate::new(admission)?;
        let write = custody.admit()?;
        let db = Database::connect(connect_options(db_url)).await?;
        // Migrate forward atomically, derive what rows owe, and refuse a DB
        // migrated AHEAD of this binary (`StoreError::AheadOfBinary`).
        crate::migration::apply_migrations(&db).await?;
        drop(write);
        Ok(Self { db, custody })
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
        let admission = crate::custody::resolve(path)?;
        Self::open(&sqlite_file_url(path), admission).await
    }

    /// Retire `version` (a migration this binary does not have, applied by a branch or
    /// unpublished build) from the store at `path`, then migrate it forward and open it.
    /// The recovery for a store refused [`StoreError::AheadOfBinary`] whose version no
    /// published airc has (`airc store retire-migration`).
    pub async fn retire_migration_at(path: &Path, version: &str) -> Result<Self, StoreError> {
        let custody = crate::custody::WriteGate::new(crate::custody::resolve(path)?)?;
        let write = custody.admit()?;
        let db = Database::connect(connect_options(&sqlite_file_url(path))).await?;
        crate::migration::retire(&db, version).await?;
        crate::migration::apply_migrations(&db).await?;
        drop(write);
        Ok(Self { db, custody })
    }

    /// Open an ephemeral in-memory store. Convenience for tests.
    pub async fn in_memory() -> Result<Self, StoreError> {
        Self::open("sqlite::memory:", crate::custody::Admission::Unmanaged).await
    }

    /// Admit one write: this process's custody for it ([`crate::custody`]),
    /// then the database must not have been migrated past this binary since it
    /// was opened ([`StoreError::AheadOfBinary`]). Held until the write ends.
    pub(crate) async fn admit_write(&self) -> Result<crate::custody::WriteTicket<'_>, StoreError> {
        let ticket = self.custody.admit()?;
        crate::migration::refuse_if_ahead(&self.db).await?;
        Ok(ticket)
    }

    /// The one ORM, for the sibling modules of this crate that run statements the
    /// entity API does not express (`retention`: windowed deletes, pragmas).
    pub(crate) fn connection(&self) -> &DatabaseConnection {
        &self.db
    }

    pub async fn try_acquire_refresh_lock(
        &self,
        mesh_identity: &str,
        now_ms: u64,
        refresh_interval_ms: u64,
        holder_pid: u32,
    ) -> Result<StoredRefreshLockOutcome, StoreError> {
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
        refresh_lock::Entity::delete_by_id(mesh_identity.to_string())
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// Load the singleton local-identity row, if it exists.
    ///
    /// Returns `Ok(None)` for a fresh database; the caller pairs this
    /// with the on-disk `identity.key` to decide whether to load,
    /// generate, or surface a partial-state error.
    pub async fn load_local_identity(&self) -> Result<Option<StoredLocalIdentity>, StoreError> {
        // Backwards-compat path: the default-agent row. Card 8384cc18
        // Sub-C added a by-name variant; this one resolves to the
        // legacy/default discriminator so pre-Sub-D callers keep
        // working unchanged.
        self.load_local_identity_by_agent_name(local_identity::DEFAULT_AGENT_NAME)
            .await
    }

    /// Look up a `local_identity` row by its `agent_name`
    /// discriminator. Card 8384cc18 Sub-C — multi-agent read API.
    ///
    /// `agent_name` is the natural read key now that Sub-B dropped
    /// the singleton CHECK; the unique index added in
    /// `m20260528_000014` makes this an O(log n) point lookup.
    pub async fn load_local_identity_by_agent_name(
        &self,
        agent_name: &str,
    ) -> Result<Option<StoredLocalIdentity>, StoreError> {
        let row = local_identity::Entity::find()
            .filter(local_identity::Column::AgentName.eq(agent_name))
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
                agent_name: m.agent_name,
            })
        })
        .transpose()
    }

    /// Return whether any local-identity row exists.
    ///
    /// Used by `airc-identity` to distinguish key-only partial state
    /// from an already-initialized scope adding a second agent row.
    pub async fn has_local_identity_rows(&self) -> Result<bool, StoreError> {
        Ok(local_identity::Entity::find()
            .one(&self.db)
            .await?
            .is_some())
    }

    /// Persist a local-identity row. Insert-only by design — there
    /// is no `replace_local_identity` because a peer_id / client_id
    /// change IS a new identity, not an update. Calling this with an
    /// existing `agent_name` returns an error from the unique index.
    pub async fn insert_local_identity(
        &self,
        identity: StoredLocalIdentity,
    ) -> Result<(), StoreError> {
        let _write = self.admit_write().await?;
        let active = local_identity::ActiveModel {
            id: ActiveValue::NotSet,
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
            agent_name: Set(identity.agent_name),
        };
        local_identity::Entity::insert(active)
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// Replace the user-facing identity card on the singleton row.
    /// Peer/client ids are not touched.
    pub async fn save_local_identity_card(&self, identity: Identity) -> Result<(), StoreError> {
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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

/// Map a `scoped_state` row into its DTO. No numeric coercion — the
/// DTO mirrors the columns' i64 storage so the u64/i64 dance the
/// time-based tables need does not apply here.
fn scoped_state_row_to_stored(row: scoped_state::Model) -> StoredScopedState {
    StoredScopedState {
        scope_key: row.scope_key,
        key: row.key,
        value_json: row.value_json,
        version: row.version,
        updated_at_ms: row.updated_at_ms,
        updated_by: row.updated_by,
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

/// Seam #3.2 — resolve a stored `last_seen_ms` column into the concrete
/// recency floor [`StoredPeer::last_seen_ms`] exposes. A NULL column
/// (never touched since enrolment, incl. every pre-migration row) floors
/// to `added_at_ms`: enrolment is the oldest defensible "we had contact"
/// instant, so the peer never reads as older than it actually is.
fn stored_last_seen_ms(column: Option<i64>, added_at_ms: u64) -> Result<u64, StoreError> {
    match column {
        Some(value) => i64_to_u64("peer_trust.last_seen_ms", value),
        None => Ok(added_at_ms),
    }
}

/// Self-healing join — resolve a stored `endpoints_advertised_at_ms`
/// column into the concrete freshness stamp
/// [`StoredPeer::endpoints_advertised_at_ms`] exposes. A NULL column
/// (pre-migration row / endpoints written before stamping existed)
/// floors to 0: "freshness unknown", so ANY stamped advertisement
/// outranks the legacy set.
fn stored_endpoints_stamp_ms(column: Option<i64>) -> Result<u64, StoreError> {
    match column {
        Some(value) => i64_to_u64("peer_trust.endpoints_advertised_at_ms", value),
        None => Ok(0),
    }
}

pub(crate) fn sqlite_file_url(path: &Path) -> String {
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
    let addressing = airc_core::Addressing::of_event(ev);
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
        addressed_peer: ActiveValue::Set(addressing.peer.map(|peer| peer.as_uuid())),
        leading_mention: ActiveValue::Set(addressing.leading_mention),
        derived_version: ActiveValue::Set(crate::derive::DERIVED_VERSION),
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

mod event_store;
#[cfg(test)]
mod tests;
mod trust_ops;

#[cfg(test)]
mod custody_tests;
