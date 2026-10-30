use super::*;

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
        let _write = self.admit_write().await?;
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
        //
        // The header rows (`crate::header_query`) commit with the event in one
        // transaction; a duplicate rolls both back.
        let txn = self.db.begin().await?;
        match event::Entity::insert(active)
            .on_conflict(
                OnConflict::column(event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&txn)
            .await
        {
            Ok(_) => {}
            Err(sea_orm::DbErr::RecordNotInserted) => {
                return Err(StoreError::DuplicateEventId(ev.event_id.as_uuid()));
            }
            Err(err) => return Err(StoreError::Database(err)),
        }
        crate::header_query::write_header_rows::<event::Entity>(
            &txn,
            [(ev.event_id.as_uuid(), &ev.headers)],
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn page_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        // "Newest N" = order DESC by (lamport, event_id), take N,
        // then reverse so the caller iterates oldest → newest.
        let mut query = event::Entity::find()
            .filter(event::Column::Class.is_not_null())
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

    async fn matching_history(
        &self,
        cursor: &TranscriptCursor,
        through: &TranscriptCursor,
        channels: &[RoomId],
        rule: airc_core::HistoryMatch<'_>,
        limit: usize,
        tail: bool,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        let mut rows = crate::event_query::matching(cursor, through, channels, rule, limit, tail)?
            .all(&self.db)
            .await?;
        if tail {
            rows.reverse();
        }
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
            .filter(event::Column::Class.is_not_null())
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
            .filter(event::Column::Class.is_not_null())
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
        let _write = self.admit_write().await?;
        let lamport = cursor.lamport as i64;
        // The existing signed column round-trips all u64 lamports by cast.
        // Negative stored values are the UPPER unsigned half, not older than
        // positive ones. Preserve transcript order across that boundary too.
        let older_lamport = if lamport < 0 {
            Expr::col(runtime_cursor::Column::Lamport)
                .gte(0)
                .or(Expr::col(runtime_cursor::Column::Lamport).lt(lamport))
        } else {
            Expr::col(runtime_cursor::Column::Lamport)
                .gte(0)
                .and(Expr::col(runtime_cursor::Column::Lamport).lt(lamport))
        };
        let active = runtime_cursor::ActiveModel {
            consumer_id: ActiveValue::Set(consumer_id.to_string()),
            lamport: ActiveValue::Set(lamport),
            event_id: ActiveValue::Set(cursor.event_id.as_uuid()),
            updated_at_ms: ActiveValue::Set(updated_at_ms as i64),
        };
        // Compare inside the upsert, not in a load-then-save window: two
        // handles can checkpoint this consumer concurrently after reconnect.
        match runtime_cursor::Entity::insert(active)
            .on_conflict(
                OnConflict::column(runtime_cursor::Column::ConsumerId)
                    .update_columns([
                        runtime_cursor::Column::Lamport,
                        runtime_cursor::Column::EventId,
                        runtime_cursor::Column::UpdatedAtMs,
                    ])
                    .action_and_where(
                        older_lamport.or(Expr::col(runtime_cursor::Column::Lamport)
                            .eq(lamport)
                            .and(
                                Expr::col(runtime_cursor::Column::EventId)
                                    .lt(cursor.event_id.as_uuid()),
                            )),
                    )
                    .to_owned(),
            )
            .exec(&self.db)
            .await
        {
            Ok(_) | Err(sea_orm::DbErr::RecordNotInserted) => Ok(()),
            Err(error) => Err(error.into()),
        }
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
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
                OnConflict::columns([scoped_state::Column::ScopeKey, scoped_state::Column::Key])
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

    async fn delete_scoped_state(&self, scope_key: &str, key: &str) -> Result<(), StoreError> {
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
