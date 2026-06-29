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
            so.journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
        });
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
                // Card 34942ec1 Sub-A: parse the stored tier wire
                // string into TrustTier. An unknown string surfaces
                // honestly via InvalidStoredValue so a forwards-
                // version-skew bug doesn't silently downgrade trust
                // (a newer binary writing "own_account" being read
                // by an older binary that doesn't know that variant
                // is a real condition we'd want to see).
                let tier = TrustTier::from_wire_str(&row.tier).ok_or_else(|| {
                    StoreError::InvalidStoredEnumString {
                        column: "peer_trust.tier",
                        value: row.tier.clone(),
                    }
                })?;
                let added_at_ms = i64_to_u64("peer_trust.added_at_ms", row.added_at_ms)?;
                Ok(StoredPeer {
                    peer_id: PeerId::from_uuid(row.peer_id),
                    pubkey_b64: row.pubkey_b64,
                    added_at_ms,
                    tier,
                    endpoints_json: row.endpoints_json,
                    last_seen_ms: stored_last_seen_ms(row.last_seen_ms, added_at_ms)?,
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
        // Card 34942ec1 Sub-A: defaults to the safe-conservative
        // tier (Untrusted). Sub-B detection and explicit-enrolment
        // CLI paths land via [`set_peer_trust_tier`] in a follow-up
        // PR; this one keeps the existing add() contract identical
        // (no API churn for current callers).
        self.add_peer_trust_with_tier(
            peer_id,
            pubkey_b64,
            added_at_ms,
            TrustTier::default_for_new_peer(),
        )
        .await
    }

    /// Card 34942ec1 Sub-A — variant of [`Self::add_peer_trust`] that
    /// lets the caller set an explicit tier at insert time. For new
    /// rows only; doesn't change an existing row's tier (an existing
    /// row keeps its stored tier — promoting/demoting goes through
    /// the separate tier-update path Sub-B introduces). Same
    /// pubkey-conflict semantics as add().
    pub async fn add_peer_trust_with_tier(
        &self,
        peer_id: PeerId,
        pubkey_b64: String,
        added_at_ms: u64,
        tier: TrustTier,
    ) -> Result<StoredPeer, StoreError> {
        let active = peer_trust::ActiveModel {
            peer_id: Set(peer_id.as_uuid()),
            pubkey_b64: Set(pubkey_b64.clone()),
            added_at_ms: Set(u64_to_i64("peer_trust.added_at_ms", added_at_ms)?),
            tier: Set(tier.as_wire_str().to_string()),
            endpoints_json: Set(None),
            // Seam #3.2: a freshly enrolled peer was, by definition, just
            // seen — seed last_seen to the enrolment instant rather than
            // leaving it NULL, so a never-touched-again peer ages from
            // enrolment forward without relying on the read-time floor.
            last_seen_ms: Set(Some(u64_to_i64("peer_trust.last_seen_ms", added_at_ms)?)),
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
        let stored_tier = TrustTier::from_wire_str(&stored.tier).ok_or_else(|| {
            StoreError::InvalidStoredEnumString {
                column: "peer_trust.tier",
                value: stored.tier.clone(),
            }
        })?;
        let added_at_ms = i64_to_u64("peer_trust.added_at_ms", stored.added_at_ms)?;
        Ok(StoredPeer {
            peer_id,
            pubkey_b64: stored.pubkey_b64,
            added_at_ms,
            tier: stored_tier,
            endpoints_json: stored.endpoints_json,
            last_seen_ms: stored_last_seen_ms(stored.last_seen_ms, added_at_ms)?,
        })
    }

    pub async fn replace_peer_trust(
        &self,
        peer_id: PeerId,
        pubkey_b64: String,
        added_at_ms: u64,
    ) -> Result<StoredPeer, StoreError> {
        // Card 34942ec1 Sub-A: replace preserves the existing tier
        // (a rotation is a key-material event; trust gradient is
        // orthogonal — losing it on rotate would silently demote a
        // Friend back to Untrusted, which would be a security
        // regression). For a fresh insert, default to Untrusted.
        // Card 625abe6d: endpoints are preserved for the same reason —
        // a key rotation doesn't move the peer's machines.
        let existing = peer_trust::Entity::find_by_id(peer_id.as_uuid())
            .one(&self.db)
            .await?;
        let existing_tier = existing
            .as_ref()
            .and_then(|row| TrustTier::from_wire_str(&row.tier))
            .unwrap_or_else(TrustTier::default_for_new_peer);
        let existing_endpoints = existing.as_ref().and_then(|row| row.endpoints_json.clone());
        // Seam #3.2: a rotation is the same machine with new key material,
        // not new contact — preserve last_seen exactly like tier/endpoints
        // (the conflict update below does NOT list LastSeenMs, so the
        // stored value survives). For a fresh insert there is no prior
        // contact, so seed it to the enrolment instant.
        let existing_last_seen = existing.as_ref().and_then(|row| row.last_seen_ms);
        let active = peer_trust::ActiveModel {
            peer_id: Set(peer_id.as_uuid()),
            pubkey_b64: Set(pubkey_b64.clone()),
            added_at_ms: Set(u64_to_i64("peer_trust.added_at_ms", added_at_ms)?),
            tier: Set(existing_tier.as_wire_str().to_string()),
            endpoints_json: Set(existing_endpoints.clone()),
            last_seen_ms: Set(Some(match existing_last_seen {
                Some(value) => value,
                None => u64_to_i64("peer_trust.last_seen_ms", added_at_ms)?,
            })),
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
            tier: existing_tier,
            endpoints_json: existing_endpoints,
            last_seen_ms: stored_last_seen_ms(existing_last_seen, added_at_ms)?,
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
        let removed_tier = TrustTier::from_wire_str(&stored.tier).ok_or_else(|| {
            StoreError::InvalidStoredEnumString {
                column: "peer_trust.tier",
                value: stored.tier.clone(),
            }
        })?;
        let added_at_ms = i64_to_u64("peer_trust.added_at_ms", stored.added_at_ms)?;
        Ok(Some(StoredPeer {
            peer_id,
            pubkey_b64: stored.pubkey_b64,
            added_at_ms,
            tier: removed_tier,
            endpoints_json: stored.endpoints_json,
            last_seen_ms: stored_last_seen_ms(stored.last_seen_ms, added_at_ms)?,
        }))
    }

    /// Card 34942ec1 Sub-B — update the trust tier on an existing
    /// peer row. Lets the substrate elevate a peer to OwnMachine /
    /// OwnAccount after detection runs (Sub-B), or lets the operator
    /// set Friend via the Sub-C CLI surface, without touching the
    /// peer's pubkey or added_at_ms.
    ///
    /// Returns `Ok(None)` if the peer isn't enrolled — the caller
    /// decides whether that's a structural bug (substrate calling
    /// set_tier on an unknown peer) or a benign race (peer was
    /// removed concurrent with the tier write). No row is inserted.
    ///
    /// Idempotent: setting the same tier the row already has returns
    /// the row unchanged.
    pub async fn set_peer_trust_tier(
        &self,
        peer_id: PeerId,
        tier: TrustTier,
    ) -> Result<Option<StoredPeer>, StoreError> {
        let txn = self.db.begin().await?;
        let Some(existing) = peer_trust::Entity::find_by_id(peer_id.as_uuid())
            .one(&txn)
            .await?
        else {
            txn.commit().await?;
            return Ok(None);
        };
        let mut active: peer_trust::ActiveModel = existing.clone().into();
        active.tier = Set(tier.as_wire_str().to_string());
        peer_trust::Entity::update(active).exec(&txn).await?;
        txn.commit().await?;
        let added_at_ms = i64_to_u64("peer_trust.added_at_ms", existing.added_at_ms)?;
        Ok(Some(StoredPeer {
            peer_id,
            pubkey_b64: existing.pubkey_b64,
            added_at_ms,
            tier,
            endpoints_json: existing.endpoints_json,
            last_seen_ms: stored_last_seen_ms(existing.last_seen_ms, added_at_ms)?,
        }))
    }

    /// Card 625abe6d slice 1 — replace the advertised endpoints on an
    /// existing peer row. The payload is the serde JSON of
    /// `Vec<RouteEndpoint>` (typed at the airc-lib layer). `None`
    /// clears the record back to identity-only.
    ///
    /// Returns `Ok(None)` if the peer isn't enrolled — endpoints
    /// without a trust anchor are meaningless (nothing to cert-pin
    /// the dial against), so no row is inserted.
    ///
    /// Idempotent: writing the same JSON returns the row unchanged.
    pub async fn set_peer_trust_endpoints(
        &self,
        peer_id: PeerId,
        endpoints_json: Option<String>,
    ) -> Result<Option<StoredPeer>, StoreError> {
        let txn = self.db.begin().await?;
        let Some(existing) = peer_trust::Entity::find_by_id(peer_id.as_uuid())
            .one(&txn)
            .await?
        else {
            txn.commit().await?;
            return Ok(None);
        };
        let stored_tier = TrustTier::from_wire_str(&existing.tier).ok_or_else(|| {
            StoreError::InvalidStoredEnumString {
                column: "peer_trust.tier",
                value: existing.tier.clone(),
            }
        })?;
        let mut active: peer_trust::ActiveModel = existing.clone().into();
        active.endpoints_json = Set(endpoints_json.clone());
        peer_trust::Entity::update(active).exec(&txn).await?;
        txn.commit().await?;
        let added_at_ms = i64_to_u64("peer_trust.added_at_ms", existing.added_at_ms)?;
        Ok(Some(StoredPeer {
            peer_id,
            pubkey_b64: existing.pubkey_b64,
            added_at_ms,
            tier: stored_tier,
            endpoints_json,
            last_seen_ms: stored_last_seen_ms(existing.last_seen_ms, added_at_ms)?,
        }))
    }

    /// Seam #3.2 (liveness) — record fresh contact with an enrolled
    /// peer. The beacon-import / successful-dial paths call this with
    /// the contact instant; it bumps `last_seen_ms` so the age-based
    /// eviction classifier (a follow-up slice) can tell a live friend
    /// from a stale enrolment.
    ///
    /// Returns `Ok(None)` if the peer isn't enrolled — recency without
    /// a trust anchor is meaningless (we only age peers we've recorded),
    /// so no row is inserted. **Monotonic**: a touch carrying an OLDER
    /// timestamp than the stored value is a no-op and the row is
    /// returned unchanged. Clock skew between machines and out-of-order
    /// beacon import must never rewind a peer's recency — that would
    /// make a live peer look stale.
    pub async fn touch_peer_last_seen(
        &self,
        peer_id: PeerId,
        seen_at_ms: u64,
    ) -> Result<Option<StoredPeer>, StoreError> {
        let txn = self.db.begin().await?;
        let Some(existing) = peer_trust::Entity::find_by_id(peer_id.as_uuid())
            .one(&txn)
            .await?
        else {
            txn.commit().await?;
            return Ok(None);
        };
        let stored_tier = TrustTier::from_wire_str(&existing.tier).ok_or_else(|| {
            StoreError::InvalidStoredEnumString {
                column: "peer_trust.tier",
                value: existing.tier.clone(),
            }
        })?;
        let added_at_ms = i64_to_u64("peer_trust.added_at_ms", existing.added_at_ms)?;
        let current = stored_last_seen_ms(existing.last_seen_ms, added_at_ms)?;
        let next = current.max(seen_at_ms);
        if next != current {
            let mut active: peer_trust::ActiveModel = existing.clone().into();
            active.last_seen_ms = Set(Some(u64_to_i64("peer_trust.last_seen_ms", next)?));
            peer_trust::Entity::update(active).exec(&txn).await?;
        }
        txn.commit().await?;
        Ok(Some(StoredPeer {
            peer_id,
            pubkey_b64: existing.pubkey_b64,
            added_at_ms,
            tier: stored_tier,
            endpoints_json: existing.endpoints_json,
            last_seen_ms: next,
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

    async fn load_local_identity_by_agent_name(
        &self,
        agent_name: &str,
    ) -> Result<Option<StoredLocalIdentity>, StoreError> {
        SqliteEventStore::load_local_identity_by_agent_name(self, agent_name).await
    }

    async fn insert_local_identity(&self, identity: StoredLocalIdentity) -> Result<(), StoreError> {
        SqliteEventStore::insert_local_identity(self, identity).await
    }

    async fn save_local_identity_card(&self, identity: Identity) -> Result<(), StoreError> {
        SqliteEventStore::save_local_identity_card(self, identity).await
    }

    async fn append(&self, ev: TranscriptEvent) -> Result<(), StoreError> {
        let active = to_active_model(&ev)?;
        // Card 127816bd Phase 1.C — eliminate the post-insert SELECT.
        //
        // Sea-ORM's `ON CONFLICT DO NOTHING` returns `Ok(_)` on a
        // true insert and `Err(DbErr::RecordNotInserted)` on a
        // do-nothing — the duplicate signal is in the error path,
        // no second query needed. The previous re-query by event_id
        // was over-cautious; event_id is a UUIDv4 + content-derived
        // and the (event_id, lamport) pair is invariant for a given
        // event, so a lamport-mismatch on the existing row can never
        // legitimately occur — only a race between two concurrent
        // appenders writing the same event_id, which the do-nothing
        // path is exactly designed to handle.
        //
        // Removes one full SELECT (planner + index hit) per `.say()`
        // — a measurable share of the 3.5ms/op baseline measured
        // in `airc-lib/tests/chat_throughput.rs`.
        match event::Entity::insert(active)
            .on_conflict(
                OnConflict::column(event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await
        {
            Ok(_) => Ok(()),
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

    async fn get_scoped_state(
        &self,
        scope_key: &str,
        key: &str,
    ) -> Result<Option<StoredScopedState>, StoreError> {
        let row = scoped_state::Entity::find_by_id((scope_key.to_string(), key.to_string()))
            .one(&self.db)
            .await?;
        Ok(row.map(scoped_state_row_to_stored))
    }

    async fn set_scoped_state(&self, entry: StoredScopedState) -> Result<(), StoreError> {
        let active = scoped_state::ActiveModel {
            scope_key: ActiveValue::Set(entry.scope_key),
            key: ActiveValue::Set(entry.key),
            value_json: ActiveValue::Set(entry.value_json),
            version: ActiveValue::Set(entry.version),
            updated_at_ms: ActiveValue::Set(entry.updated_at_ms),
            updated_by: ActiveValue::Set(entry.updated_by),
        };
        scoped_state::Entity::insert(active)
            .on_conflict(
                OnConflict::columns([
                    scoped_state::Column::ScopeKey,
                    scoped_state::Column::Key,
                ])
                .update_columns([
                    scoped_state::Column::ValueJson,
                    scoped_state::Column::Version,
                    scoped_state::Column::UpdatedAtMs,
                    scoped_state::Column::UpdatedBy,
                ])
                .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn list_scoped_state(
        &self,
        scope_key: &str,
    ) -> Result<Vec<StoredScopedState>, StoreError> {
        let rows = scoped_state::Entity::find()
            .filter(scoped_state::Column::ScopeKey.eq(scope_key))
            .order_by_asc(scoped_state::Column::Key)
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(scoped_state_row_to_stored).collect())
    }

    async fn delete_scoped_state(
        &self,
        scope_key: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        scoped_state::Entity::delete_by_id((scope_key.to_string(), key.to_string()))
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
                agent_name: crate::DEFAULT_AGENT_NAME.to_string(),
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
        // Card 8384cc18 Sub-A: every row carries an agent_name; the
        // 2026-05-28+ default for pre-multi-agent installs is
        // "default", set by the column default. The DTO surfaces it.
        assert_eq!(
            stored.agent_name,
            crate::DEFAULT_AGENT_NAME,
            "Sub-A: row inserted without an explicit agent_name still surfaces \
             the documented default through the DTO"
        );
    }

    /// Card 8384cc18 Sub-A — explicit `agent_name` round-trips
    /// through the insert / load path. Even though Sub-A doesn't
    /// yet allow multiple rows (the singleton CHECK is dropped in
    /// Sub-B), the column has to actually carry whatever the caller
    /// supplies, not silently force the default — otherwise Sub-B
    /// would have to re-validate this and would carry the risk of
    /// silently re-defaulting.
    #[tokio::test]
    async fn explicit_agent_name_round_trips_through_local_identity() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let store_api: &dyn EventStore = &store;
        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa2),
                client_id: ClientId::from_u128(0xc2),
                version: 1,
                created_at_ms: 100,
                identity: Identity::new("bob"),
                agent_name: "claude-tab-2".to_string(),
            })
            .await
            .unwrap();

        // Card 8384cc18 Sub-C: `load_local_identity()` is now
        // explicitly the default-agent path; round-tripping a
        // non-default name uses the by-name lookup. This is the
        // change in contract Sub-C ships — the test still proves
        // the original intent (Sub-A's "agent_name actually
        // persists, not silently re-defaults") but routes through
        // the API surface that multi-agent callers will use.
        let stored = store_api
            .load_local_identity_by_agent_name("claude-tab-2")
            .await
            .unwrap()
            .expect("local identity row");
        assert_eq!(stored.agent_name, "claude-tab-2");
        // The rest of the DTO is unaffected — Sub-A is purely
        // additive on this field. (Pinning lets a future Sub-B
        // table-recreate migration catch a regression where it
        // forgets to preserve agent_name during the
        // INSERT … SELECT step.)
        assert_eq!(stored.peer_id, PeerId::from_u128(0xa2));
        assert_eq!(stored.client_id, ClientId::from_u128(0xc2));

        // And the legacy `load_local_identity()` path (which now
        // means "default agent") correctly returns None when no
        // default-agent row exists in the table.
        let legacy_default = store_api.load_local_identity().await.unwrap();
        assert!(
            legacy_default.is_none(),
            "load_local_identity() now resolves to the default agent only; \
             a table holding only `claude-tab-2` must surface None",
        );
    }

    /// Card 8384cc18 Sub-D — the API write path
    /// (`insert_local_identity`) accepts a second agent row, not just
    /// raw ActiveModel inserts. Sister of
    /// `local_identity_schema_accepts_a_second_agent_row` below
    /// (Sub-B), which proves the SCHEMA accepts via raw insert; this
    /// proves the API path round-trips end-to-end.
    #[tokio::test]
    async fn local_identity_api_inserts_a_second_agent_row() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let store_api: &dyn EventStore = &store;
        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa3),
                client_id: ClientId::from_u128(0xc3),
                version: 1,
                created_at_ms: 100,
                identity: Identity::new("default-agent"),
                agent_name: crate::DEFAULT_AGENT_NAME.to_string(),
            })
            .await
            .unwrap();

        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa4),
                client_id: ClientId::from_u128(0xc4),
                version: 1,
                created_at_ms: 101,
                identity: Identity::new("codex"),
                agent_name: "codex".to_string(),
            })
            .await
            .expect("Sub-D write path inserts a non-singleton local_identity row");

        let rows = local_identity::Entity::find()
            .order_by_asc(local_identity::Column::Id)
            .all(&store.db)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, local_identity::SINGLETON_ID);
        assert_eq!(rows[0].agent_name, crate::DEFAULT_AGENT_NAME);
        assert_eq!(rows[1].id, 2);
        assert_eq!(rows[1].agent_name, "codex");
    }

    /// Card 8384cc18 Sub-C — `load_local_identity_by_agent_name`
    /// resolves the row whose `agent_name` matches. With two distinct
    /// agents in the table, looking up "default" returns the original
    /// row and "codex" returns the second; the legacy
    /// `load_local_identity()` keeps returning the default-agent row
    /// (backwards-compat: every pre-Sub-C caller is implicitly
    /// asking for that one).
    #[tokio::test]
    async fn load_local_identity_by_agent_name_disambiguates_rows() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let store_api: &dyn EventStore = &store;

        // Default-agent row via the API (id auto-resolved to
        // SINGLETON_ID by insert_local_identity).
        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xb1),
                client_id: ClientId::from_u128(0xd1),
                version: 1,
                created_at_ms: 200,
                identity: Identity::new("primary"),
                agent_name: crate::DEFAULT_AGENT_NAME.to_string(),
            })
            .await
            .unwrap();

        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xb2),
                client_id: ClientId::from_u128(0xd2),
                version: 1,
                created_at_ms: 201,
                identity: Identity::new("codex"),
                agent_name: "codex".to_string(),
            })
            .await
            .unwrap();

        // By-name lookup: default vs codex resolves to distinct rows.
        let default_row = store_api
            .load_local_identity_by_agent_name(crate::DEFAULT_AGENT_NAME)
            .await
            .unwrap()
            .expect("default-agent row present");
        assert_eq!(default_row.peer_id, PeerId::from_u128(0xb1));
        assert_eq!(default_row.identity.name, "primary");

        let codex_row = store_api
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .expect("codex-agent row present");
        assert_eq!(codex_row.peer_id, PeerId::from_u128(0xb2));
        assert_eq!(codex_row.identity.name, "codex");
        assert_ne!(default_row.peer_id, codex_row.peer_id);

        // Missing agent_name resolves to None — the substrate contract
        // for "this scope has never been initialized for that agent."
        let missing = store_api
            .load_local_identity_by_agent_name("hermes")
            .await
            .unwrap();
        assert!(missing.is_none());

        // Backwards-compat: legacy load_local_identity() still
        // returns the default-agent row. Sub-D wires the override.
        let legacy = store_api.load_local_identity().await.unwrap().unwrap();
        assert_eq!(legacy.peer_id, default_row.peer_id);
        assert_eq!(legacy.agent_name, crate::DEFAULT_AGENT_NAME);
    }

    /// Card 8384cc18 Sub-B — the final schema no longer enforces the
    /// singleton `CHECK (id = 1)`. Store APIs still load the default row
    /// until Sub-C/Sub-D add agent-name lookup and init surfaces, but
    /// the database must now accept a second agent row so those slices
    /// have a schema to target.
    #[tokio::test]
    async fn local_identity_schema_accepts_a_second_agent_row() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let store_api: &dyn EventStore = &store;
        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa3),
                client_id: ClientId::from_u128(0xc3),
                version: 1,
                created_at_ms: 100,
                identity: Identity::new("default-agent"),
                agent_name: crate::DEFAULT_AGENT_NAME.to_string(),
            })
            .await
            .unwrap();

        local_identity::Entity::insert(local_identity::ActiveModel {
            id: Set(2),
            peer_id: Set(PeerId::from_u128(0xa4).as_uuid()),
            client_id: Set(ClientId::from_u128(0xc4).as_uuid()),
            version: Set(1),
            created_at_ms: Set(101),
            name: Set("codex".to_string()),
            pronouns: Set("".to_string()),
            role: Set("agent".to_string()),
            bio: Set("".to_string()),
            status: Set("active".to_string()),
            fingerprint: Set("".to_string()),
            integrations_json: Set(json!({})),
            agent_name: Set("codex".to_string()),
        })
        .exec(&store.db)
        .await
        .expect("Sub-B schema accepts a non-singleton local_identity row");

        let rows = local_identity::Entity::find()
            .order_by_asc(local_identity::Column::Id)
            .all(&store.db)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, local_identity::SINGLETON_ID);
        assert_eq!(rows[0].agent_name, crate::DEFAULT_AGENT_NAME);
        assert_eq!(rows[1].id, 2);
        assert_eq!(rows[1].agent_name, "codex");
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

    // what this catches: the generic scoped-state store's full
    // round-trip on real SQLite — set upserts on the (scope_key, key)
    // composite PK (second write wins), get reads it back, list filters
    // to one scope and returns rows key-ascending, and delete is
    // idempotent. Regresses the wiring of migration #18 + the
    // OnConflict::columns([ScopeKey, Key]) upsert.
    #[tokio::test]
    async fn scoped_state_upserts_lists_and_deletes() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = "room:general";

        // Missing key reads as None.
        assert!(store.get_scoped_state(room, "plan").await.unwrap().is_none());

        // First write, then an upsert of the SAME (scope_key, key).
        store
            .set_scoped_state(StoredScopedState {
                scope_key: room.to_string(),
                key: "plan".to_string(),
                value_json: r#"{"v":1}"#.to_string(),
                version: 1,
                updated_at_ms: 100,
                updated_by: Some("peer:a".to_string()),
            })
            .await
            .unwrap();
        store
            .set_scoped_state(StoredScopedState {
                scope_key: room.to_string(),
                key: "plan".to_string(),
                value_json: r#"{"v":2}"#.to_string(),
                version: 2,
                updated_at_ms: 200,
                updated_by: Some("peer:b".to_string()),
            })
            .await
            .unwrap();

        let got = store.get_scoped_state(room, "plan").await.unwrap().unwrap();
        assert_eq!(got.value_json, r#"{"v":2}"#, "upsert: second write wins");
        assert_eq!(got.version, 2);
        assert_eq!(got.updated_by.as_deref(), Some("peer:b"));

        // A second key in the same scope, plus a key in a DIFFERENT
        // scope that list must NOT return.
        store
            .set_scoped_state(StoredScopedState {
                scope_key: room.to_string(),
                key: "instructions".to_string(),
                value_json: "\"be terse\"".to_string(),
                version: 1,
                updated_at_ms: 150,
                updated_by: None,
            })
            .await
            .unwrap();
        store
            .set_scoped_state(StoredScopedState {
                scope_key: "user:peer-a".to_string(),
                key: "prefs".to_string(),
                value_json: "{}".to_string(),
                version: 1,
                updated_at_ms: 50,
                updated_by: None,
            })
            .await
            .unwrap();

        let listed = store.list_scoped_state(room).await.unwrap();
        let keys: Vec<&str> = listed.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["instructions", "plan"],
            "list is scope-isolated and key-ascending"
        );

        // Delete is idempotent — deleting twice is not an error.
        store.delete_scoped_state(room, "plan").await.unwrap();
        store.delete_scoped_state(room, "plan").await.unwrap();
        assert!(store.get_scoped_state(room, "plan").await.unwrap().is_none());
        assert_eq!(store.list_scoped_state(room).await.unwrap().len(), 1);
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

    // ---------------------------------------------------------------
    // Card 34942ec1 Sub-A — peer_trust tier round-trip
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn add_peer_trust_defaults_to_untrusted_tier() {
        // Sub-A migration sets the column default = 'untrusted'.
        // The existing add_peer_trust() contract stays identical
        // (peer_id, pubkey, timestamp) — callers don't have to pick
        // a tier just to enrol. Pinning that the stored tier is
        // Untrusted, not whatever the enum's Default::default()
        // happens to be (we don't impl Default on TrustTier on
        // purpose: every insert site must decide explicitly).
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0x1234_abcd);
        let stored = store
            .add_peer_trust(peer_id, "AAAA".repeat(11), 100)
            .await
            .unwrap();
        assert_eq!(stored.tier, TrustTier::Untrusted);

        let loaded = store.load_peers().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tier, TrustTier::Untrusted);
    }

    /// Card 625abe6d / #1120 sentinel mutation M1 pin: a key
    /// rotation (replace_peer_trust) must PRESERVE stored endpoints —
    /// rotating key material does not move the peer's machines.
    /// Without this pin, `endpoints_json: Set(None)` in replace
    /// survives the whole suite.
    #[tokio::test]
    async fn replace_peer_trust_preserves_stored_endpoints() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0x5151_e0e0);
        store
            .add_peer_trust(peer_id, "AAAA".repeat(11), 100)
            .await
            .unwrap();
        let json = r#"[{"kind":"lan_tcp","addr":"192.168.1.232:7474"}]"#;
        store
            .set_peer_trust_endpoints(peer_id, Some(json.to_string()))
            .await
            .unwrap()
            .expect("peer enrolled");

        let rotated = store
            .replace_peer_trust(peer_id, "BBBB".repeat(11), 200)
            .await
            .unwrap();
        assert_eq!(
            rotated.endpoints_json.as_deref(),
            Some(json),
            "rotation must carry endpoints forward"
        );
        let loaded = store.load_peers().await.unwrap();
        assert_eq!(loaded[0].endpoints_json.as_deref(), Some(json));
        assert_eq!(loaded[0].pubkey_b64, "BBBB".repeat(11));
    }

    /// Card 625abe6d: endpoint set on an unknown peer is a refused
    /// no-op (Ok(None)), never an implicit insert — endpoints without
    /// a pubkey to cert-pin the dial against are meaningless.
    #[tokio::test]
    async fn set_endpoints_on_unknown_peer_returns_none_no_row_inserted() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0xdead_beef);
        let result = store
            .set_peer_trust_endpoints(peer_id, Some("[]".to_string()))
            .await
            .unwrap();
        assert!(result.is_none());
        assert!(store.load_peers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn add_peer_trust_with_tier_round_trips_explicit_tier() {
        // The with_tier variant lets Sub-B detection (UDS sibling →
        // OwnMachine) or explicit-enrolment CLI (`airc peer add
        // --tier=friend`) pin the tier at insert. Pin that every
        // declared variant round-trips end-to-end through write +
        // reload — a forgotten match arm in load_peers (post-
        // migration drift) would only surface in CI when an actual
        // OwnMachine peer was enrolled, but this test catches it at
        // PR review time.
        let store = SqliteEventStore::in_memory().await.unwrap();
        for (i, tier) in TrustTier::ALL_VARIANTS.iter().copied().enumerate() {
            let peer_id = PeerId::from_u128(0x4000 + i as u128);
            let stored = store
                .add_peer_trust_with_tier(
                    peer_id,
                    format!("PUB{i}").repeat(11),
                    100 + i as u64,
                    tier,
                )
                .await
                .unwrap();
            assert_eq!(stored.tier, tier, "round-trip: insert tier survives");
        }
        let loaded = store.load_peers().await.unwrap();
        assert_eq!(loaded.len(), TrustTier::ALL_VARIANTS.len());
        let observed: std::collections::HashSet<_> = loaded.iter().map(|p| p.tier).collect();
        let expected: std::collections::HashSet<_> =
            TrustTier::ALL_VARIANTS.iter().copied().collect();
        assert_eq!(observed, expected, "every variant survives the round-trip");
    }

    #[tokio::test]
    async fn replace_peer_trust_preserves_existing_tier() {
        // Card 34942ec1 Sub-A explicit invariant: a key-material
        // rotation (replace_peer_trust) must NOT silently demote a
        // Friend back to Untrusted. The tier is orthogonal to the
        // pubkey; losing it on rotate would be a security
        // regression that this test catches.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0xfeed_face);

        // Enrol as Friend explicitly.
        let initial = store
            .add_peer_trust_with_tier(peer_id, "A".repeat(43), 100, TrustTier::Friend)
            .await
            .unwrap();
        assert_eq!(initial.tier, TrustTier::Friend);

        // Rotate to a new pubkey via replace_peer_trust.
        let rotated = store
            .replace_peer_trust(peer_id, "B".repeat(43), 200)
            .await
            .unwrap();
        assert_eq!(
            rotated.tier,
            TrustTier::Friend,
            "rotate must preserve the existing tier"
        );

        // Confirm the durable row, not just the return value.
        let reloaded = store.load_peers().await.unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].tier, TrustTier::Friend);
    }

    // ---------------------------------------------------------------
    // Seam #3.2 (IDENTITY-SCOPE-PEER-LIVENESS-MODEL) — last_seen_ms
    // ---------------------------------------------------------------

    /// what this catches: a fresh enrolment must seed `last_seen_ms` to
    /// the enrolment instant, not leave it NULL/0. A 0 floor would make
    /// every just-added peer read as maximally stale and the age-based
    /// classifier would evict peers the instant they enrol.
    #[tokio::test]
    async fn add_peer_trust_seeds_last_seen_to_added_at() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0x3201_0001);
        let stored = store
            .add_peer_trust(peer_id, "AAAA".repeat(11), 4242)
            .await
            .unwrap();
        assert_eq!(stored.last_seen_ms, 4242, "enrolment seeds last_seen");

        let loaded = store.load_peers().await.unwrap();
        assert_eq!(loaded[0].last_seen_ms, 4242, "durable, not just returned");
    }

    /// what this catches: `touch_peer_last_seen` must advance recency on
    /// fresh contact AND must be monotonic — a touch carrying an OLDER
    /// timestamp (clock skew, out-of-order beacon import) must NOT
    /// rewind the stored value, which would make a live peer look stale.
    #[tokio::test]
    async fn touch_peer_last_seen_advances_and_is_monotonic() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0x3201_0002);
        store
            .add_peer_trust(peer_id, "AAAA".repeat(11), 1000)
            .await
            .unwrap();

        // Fresh contact advances.
        let touched = store
            .touch_peer_last_seen(peer_id, 5000)
            .await
            .unwrap()
            .expect("peer enrolled");
        assert_eq!(touched.last_seen_ms, 5000);

        // Older timestamp is ignored — recency never rewinds.
        let stale = store
            .touch_peer_last_seen(peer_id, 2000)
            .await
            .unwrap()
            .expect("peer enrolled");
        assert_eq!(
            stale.last_seen_ms, 5000,
            "monotonic: older touch is a no-op"
        );

        let loaded = store.load_peers().await.unwrap();
        assert_eq!(loaded[0].last_seen_ms, 5000, "durable max, not last write");
    }

    /// what this catches: touching an un-enrolled peer must be a refused
    /// no-op (Ok(None)), never an implicit insert — recency without a
    /// trust anchor is meaningless (mirrors the endpoints contract).
    #[tokio::test]
    async fn touch_peer_last_seen_unknown_peer_returns_none_no_row_inserted() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0x3201_0003);
        let result = store.touch_peer_last_seen(peer_id, 9000).await.unwrap();
        assert!(result.is_none());
        assert!(store.load_peers().await.unwrap().is_empty());
    }

    /// what this catches: a key rotation must carry `last_seen_ms`
    /// forward — rotating key material is not fresh contact, so it must
    /// neither reset recency to the new added_at nor drop it. Same
    /// preservation invariant as tier and endpoints across rotate.
    #[tokio::test]
    async fn replace_peer_trust_preserves_last_seen() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let peer_id = PeerId::from_u128(0x3201_0004);
        store
            .add_peer_trust(peer_id, "AAAA".repeat(11), 1000)
            .await
            .unwrap();
        store
            .touch_peer_last_seen(peer_id, 7777)
            .await
            .unwrap()
            .expect("peer enrolled");

        let rotated = store
            .replace_peer_trust(peer_id, "BBBB".repeat(11), 9999)
            .await
            .unwrap();
        assert_eq!(
            rotated.last_seen_ms, 7777,
            "rotation carries recency forward, not reset to new added_at"
        );
        let loaded = store.load_peers().await.unwrap();
        assert_eq!(loaded[0].last_seen_ms, 7777);
    }

    /// what this catches: the read-time floor. A stored NULL column
    /// (every pre-migration row) must resolve to `added_at_ms`, never 0
    /// — so a friend enrolled before this migration never reads as
    /// instantly stale. Pure unit test of the floor helper; no IO.
    #[test]
    fn stored_last_seen_ms_floors_null_to_added_at() {
        assert_eq!(
            stored_last_seen_ms(None, 12345).unwrap(),
            12345,
            "NULL column floors to added_at"
        );
        assert_eq!(
            stored_last_seen_ms(Some(20000), 12345).unwrap(),
            20000,
            "present column is honoured as-is"
        );
    }

    #[test]
    fn trust_tier_wire_str_round_trips_every_variant() {
        // The consumer-sync guard (same pattern as
        // TranscriptKind::wire_str_round_trip_covers_every_variant).
        // Forgetting to extend as_wire_str / from_wire_str /
        // ALL_VARIANTS when adding a variant is exactly the kink
        // 0cfcc8db pattern that motivated the convention.
        for &tier in TrustTier::ALL_VARIANTS {
            let s = tier.as_wire_str();
            let back = TrustTier::from_wire_str(s);
            assert_eq!(
                back,
                Some(tier),
                "{tier:?} must round-trip through wire string"
            );
        }
        // Unknown strings honestly fail.
        assert_eq!(TrustTier::from_wire_str(""), None);
        assert_eq!(TrustTier::from_wire_str("verified"), None);
        assert_eq!(TrustTier::from_wire_str("OwnMachine"), None);
    }
}
