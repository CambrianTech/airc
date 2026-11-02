use super::*;

impl SqliteEventStore {
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
                    endpoints_peer_id: row.endpoints_peer_id.map(PeerId::from_uuid),
                    endpoints_advertised_at_ms: stored_endpoints_stamp_ms(
                        row.endpoints_advertised_at_ms,
                    )?,
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
        let _write = self.admit_write().await?;
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
        let _write = self.admit_write().await?;
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
            // No endpoints yet, so no endpoint freshness either.
            endpoints_advertised_at_ms: Set(None),
            // …and no host mapping (mapping travels with endpoints).
            endpoints_peer_id: Set(None),
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
            endpoints_peer_id: stored.endpoints_peer_id.map(PeerId::from_uuid),
            endpoints_advertised_at_ms: stored_endpoints_stamp_ms(
                stored.endpoints_advertised_at_ms,
            )?,
        })
    }

    pub async fn replace_peer_trust(
        &self,
        peer_id: PeerId,
        pubkey_b64: String,
        added_at_ms: u64,
    ) -> Result<StoredPeer, StoreError> {
        let _write = self.admit_write().await?;
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
        // Endpoint freshness travels WITH the endpoint set — preserved
        // across rotation exactly like the endpoints themselves.
        let existing_endpoints_stamp = existing
            .as_ref()
            .and_then(|row| row.endpoints_advertised_at_ms);
        // The host mapping travels with the endpoint set — preserved
        // across rotation exactly like the endpoints and their stamp.
        let existing_endpoints_peer = existing.as_ref().and_then(|row| row.endpoints_peer_id);
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
            endpoints_advertised_at_ms: Set(existing_endpoints_stamp),
            endpoints_peer_id: Set(existing_endpoints_peer),
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
            endpoints_peer_id: existing_endpoints_peer.map(PeerId::from_uuid),
            endpoints_advertised_at_ms: stored_endpoints_stamp_ms(existing_endpoints_stamp)?,
        })
    }

    pub async fn remove_peer_trust(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<StoredPeer>, StoreError> {
        let _write = self.admit_write().await?;
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
            endpoints_peer_id: stored.endpoints_peer_id.map(PeerId::from_uuid),
            endpoints_advertised_at_ms: stored_endpoints_stamp_ms(
                stored.endpoints_advertised_at_ms,
            )?,
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
        let _write = self.admit_write().await?;
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
            endpoints_peer_id: existing.endpoints_peer_id.map(PeerId::from_uuid),
            endpoints_advertised_at_ms: stored_endpoints_stamp_ms(
                existing.endpoints_advertised_at_ms,
            )?,
        }))
    }

    /// Card 625abe6d slice 1 + self-healing join — replace the
    /// advertised endpoints on an existing peer row, ATOMICALLY with
    /// their freshness stamp. The payload is the serde JSON of
    /// `Vec<RouteEndpoint>` (typed at the airc-lib layer). `None`
    /// clears the record back to identity-only.
    ///
    /// `advertised_at_ms` is the epoch-ms instant of the advertisement
    /// the set came from. **Monotonic replace**: a write whose stamp is
    /// STALER than the stored stamp is refused (no-op, row returned
    /// unchanged) — endpoint addr+port+transport move as one value with
    /// their stamp, so an out-of-order merge/import can never mix a new
    /// IP with a dead port or resurrect an evicted set (the M5↔bigmama
    /// stale-port repro). An equal-or-fresher stamp fully replaces both
    /// the set and the stamp.
    ///
    /// Returns `Ok(None)` if the peer isn't enrolled — endpoints
    /// without a trust anchor are meaningless (nothing to cert-pin
    /// the dial against), so no row is inserted.
    ///
    /// Idempotent: re-writing the same JSON at the same stamp returns
    /// the row unchanged.
    pub async fn set_peer_trust_endpoints(
        &self,
        peer_id: PeerId,
        endpoints_json: Option<String>,
        advertised_at_ms: u64,
        endpoints_peer_id: Option<PeerId>,
    ) -> Result<Option<StoredPeer>, StoreError> {
        let _write = self.admit_write().await?;
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
        let stored_stamp = stored_endpoints_stamp_ms(existing.endpoints_advertised_at_ms)?;
        if advertised_at_ms < stored_stamp {
            // Staler advertisement than what we hold — refuse the whole
            // write (endpoints AND their host mapping move as one value
            // with the stamp). Returning the UNCHANGED row (not an
            // error) keeps import loops total: one stale beacon must
            // not abort the rest of a registry sync.
            txn.commit().await?;
            return Ok(Some(StoredPeer {
                peer_id,
                pubkey_b64: existing.pubkey_b64,
                added_at_ms,
                tier: stored_tier,
                endpoints_json: existing.endpoints_json,
                last_seen_ms: stored_last_seen_ms(existing.last_seen_ms, added_at_ms)?,
                endpoints_peer_id: existing.endpoints_peer_id.map(PeerId::from_uuid),
                endpoints_advertised_at_ms: stored_stamp,
            }));
        }
        let mut active: peer_trust::ActiveModel = existing.clone().into();
        active.endpoints_json = Set(endpoints_json.clone());
        active.endpoints_advertised_at_ms = Set(Some(u64_to_i64(
            "peer_trust.endpoints_advertised_at_ms",
            advertised_at_ms,
        )?));
        // Self-healing join (machine-vs-scope): the transport-host
        // mapping travels WITH the endpoint set, under the same stamp —
        // a fresher advertisement replaces both, a staler one was
        // refused above, and a mapping can never outlive the endpoints
        // it describes.
        active.endpoints_peer_id = Set(endpoints_peer_id.map(|peer| peer.as_uuid()));
        peer_trust::Entity::update(active).exec(&txn).await?;
        txn.commit().await?;
        Ok(Some(StoredPeer {
            peer_id,
            pubkey_b64: existing.pubkey_b64,
            added_at_ms,
            tier: stored_tier,
            endpoints_json,
            last_seen_ms: stored_last_seen_ms(existing.last_seen_ms, added_at_ms)?,
            endpoints_peer_id,
            endpoints_advertised_at_ms: advertised_at_ms,
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
        let _write = self.admit_write().await?;
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
            endpoints_peer_id: existing.endpoints_peer_id.map(PeerId::from_uuid),
            endpoints_advertised_at_ms: stored_endpoints_stamp_ms(
                existing.endpoints_advertised_at_ms,
            )?,
        }))
    }

    pub async fn append_peer_rotation_audit(
        &self,
        entry: RotationAuditEntry,
    ) -> Result<(), StoreError> {
        let _write = self.admit_write().await?;
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
}
