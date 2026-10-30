#![cfg(test)]
use super::*;

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
    assert!(store
        .get_scoped_state(room, "plan")
        .await
        .unwrap()
        .is_none());

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
    assert!(store
        .get_scoped_state(room, "plan")
        .await
        .unwrap()
        .is_none());
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
        .set_peer_trust_endpoints(
            peer_id,
            Some(json.to_string()),
            150,
            Some(PeerId::from_u128(0xda_e0)),
        )
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
    assert_eq!(
        rotated.endpoints_advertised_at_ms, 150,
        "rotation must carry the endpoint freshness stamp with the endpoints"
    );
    assert_eq!(
        rotated.endpoints_peer_id,
        Some(PeerId::from_u128(0xda_e0)),
        "rotation must carry the transport-host mapping with the endpoints"
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
        .set_peer_trust_endpoints(peer_id, Some("[]".to_string()), 100, None)
        .await
        .unwrap();
    assert!(result.is_none());
    assert!(store.load_peers().await.unwrap().is_empty());
}

/// what this catches (self-healing join, M5↔bigmama stale-port
/// repro): endpoint replacement must be ATOMIC + stamped. A record
/// holding old (ip1, port1) that receives a FRESHER advertisement
/// (ip2, port2) must end up holding EXACTLY (ip2, port2) — never a
/// field-merged (ip2, port1) — and a STALER advertisement must be
/// refused whole, never resurrecting a dead endpoint.
/// Mutation check: dropping the `advertised_at_ms < stored_stamp`
/// guard fails the stale-refusal half; writing endpoints without
/// the stamp fails the stamp assertions.
#[tokio::test]
async fn fresher_advertisement_atomically_replaces_endpoints_and_staler_is_refused() {
    let store = SqliteEventStore::in_memory().await.unwrap();
    let peer_id = PeerId::from_u128(0x0a0a_1b1b);
    store
        .add_peer_trust(peer_id, "AAAA".repeat(11), 100)
        .await
        .unwrap();

    let old = r#"[{"kind":"lan_tcp","addr":"192.168.1.249:58842"}]"#;
    let fresh = r#"[{"kind":"lan_tcp","addr":"192.168.1.250:57958"}]"#;

    store
        .set_peer_trust_endpoints(
            peer_id,
            Some(old.to_string()),
            1_000,
            Some(PeerId::from_u128(0xaa)),
        )
        .await
        .unwrap()
        .expect("peer enrolled");

    // Fresher advertisement: full replace — addr AND port together.
    let replaced = store
        .set_peer_trust_endpoints(
            peer_id,
            Some(fresh.to_string()),
            2_000,
            Some(PeerId::from_u128(0xbb)),
        )
        .await
        .unwrap()
        .expect("peer enrolled");
    assert_eq!(replaced.endpoints_json.as_deref(), Some(fresh));
    assert_eq!(replaced.endpoints_advertised_at_ms, 2_000);
    assert_eq!(
        replaced.endpoints_peer_id,
        Some(PeerId::from_u128(0xbb)),
        "a fresher advertisement replaces the host mapping WITH the endpoints"
    );

    // Staler advertisement (an out-of-order re-sync carrying the
    // dead port): refused whole, row unchanged.
    let refused = store
        .set_peer_trust_endpoints(
            peer_id,
            Some(old.to_string()),
            1_500,
            Some(PeerId::from_u128(0xaa)),
        )
        .await
        .unwrap()
        .expect("peer enrolled");
    assert_eq!(
        refused.endpoints_json.as_deref(),
        Some(fresh),
        "a staler advertisement must never resurrect a dead endpoint"
    );
    assert_eq!(refused.endpoints_advertised_at_ms, 2_000);
    assert_eq!(
        refused.endpoints_peer_id,
        Some(PeerId::from_u128(0xbb)),
        "a refused staler advertisement must not touch the host mapping either"
    );

    let loaded = store.load_peers().await.unwrap();
    assert_eq!(loaded[0].endpoints_json.as_deref(), Some(fresh));
    assert_eq!(loaded[0].endpoints_advertised_at_ms, 2_000);
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
            .add_peer_trust_with_tier(peer_id, format!("PUB{i}").repeat(11), 100 + i as u64, tier)
            .await
            .unwrap();
        assert_eq!(stored.tier, tier, "round-trip: insert tier survives");
    }
    let loaded = store.load_peers().await.unwrap();
    assert_eq!(loaded.len(), TrustTier::ALL_VARIANTS.len());
    let observed: std::collections::HashSet<_> = loaded.iter().map(|p| p.tier).collect();
    let expected: std::collections::HashSet<_> = TrustTier::ALL_VARIANTS.iter().copied().collect();
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
