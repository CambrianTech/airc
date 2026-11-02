#![cfg(test)]
use super::*;
use airc_core::transcript::MentionTarget;
use sea_orm::QueryTrait;
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
        addressed_peer: None,
        leading_mention: false,
        derived_version: crate::derive::DERIVED_VERSION,
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

// what this catches: the embedded addressee scan reads the addressing
// columns derived at append, so a typed-target and an `@`-led request come
// back as the newest candidates under many newer unaddressed events, and a
// mid-sentence `@` stays out.
#[tokio::test]
async fn addressee_scan_selects_candidates_by_indexed_columns() {
    let store = SqliteEventStore::in_memory().await.unwrap();
    let room = RoomId::from_u128(0xadd);
    let me = PeerId::from_u128(0x3e);
    let mut typed = make_event(1, room, "no names");
    typed.target = MentionTarget::Peer(me);
    let led = make_event(2, room, "\t@Fable review");
    store.append(typed.clone()).await.unwrap();
    store.append(led.clone()).await.unwrap();
    store
        .append(make_event(3, room, "ask @Fable later"))
        .await
        .unwrap();
    for lamport in 4..600 {
        store
            .append(make_event(lamport, room, "ambient"))
            .await
            .unwrap();
    }
    let origin = TranscriptCursor {
        lamport: 0,
        event_id: EventId::from_u128(0),
    };
    let through = TranscriptCursor {
        lamport: u64::MAX >> 1,
        event_id: EventId::from_u128(u128::MAX),
    };
    let rule = airc_core::HistoryMatch {
        headers: &airc_core::HeaderFilter::Any,
        addressed_to: Some(airc_core::AddresseeFilter { peer: me }),
    };
    let page = store
        .matching_history(&origin, &through, &[room], rule, 10, true)
        .await
        .unwrap();
    assert_eq!(page, [typed, led]);
}

// what this catches: the addressee scan must SEARCH the addressing indexes
// (rows touched = candidates), never walk the room in cursor order; over
// 20k unaddressed events it returns the three requests, in bounded time.
#[tokio::test]
async fn addressee_scan_searches_its_indexes_over_twenty_thousand_events() {
    let store = SqliteEventStore::in_memory().await.unwrap();
    let room = RoomId::from_u128(0xadd2);
    let me = PeerId::from_u128(0x3e);
    let mut requests = Vec::new();
    for lamport in 1..=20_003u64 {
        let mut ev = make_event(lamport, room, "ambient");
        if lamport % 6_000 == 0 {
            ev.target = MentionTarget::Peer(me);
            requests.push(ev.clone());
        }
        store.append(ev).await.unwrap();
    }
    let origin = TranscriptCursor {
        lamport: 0,
        event_id: EventId::from_u128(0),
    };
    let through = TranscriptCursor {
        lamport: u64::MAX >> 1,
        event_id: EventId::from_u128(u128::MAX),
    };
    let rule = airc_core::HistoryMatch {
        headers: &airc_core::HeaderFilter::Any,
        addressed_to: Some(airc_core::AddresseeFilter { peer: me }),
    };
    let branches = crate::event_query::matching(&origin, &through, &[room], rule, 10, true);
    let indexes = [
        "idx_events_room_addressed_peer",
        "idx_events_room_leading_mention",
    ];
    assert_eq!(branches.len(), indexes.len(), "one select per branch");
    for (query, index) in branches.into_iter().zip(indexes) {
        let built = query.build(sea_orm::DbBackend::Sqlite);
        let plan = crate::sqlite_only::query_plan(&store.db, built)
            .await
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.starts_with("SEARCH") && step.contains(index)),
            "{index} searched: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("SCAN")),
            "no table walk: {plan:?}"
        );
    }
    let started = std::time::Instant::now();
    let page = store
        .matching_history(&origin, &through, &[room], rule, 10, true)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(page, requests);
    // Two index searches and a merge of three rows; a walk of 20k rows
    // is the regression this bound would catch.
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "{elapsed:?}"
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let store = SqliteEventStore::open_path(&path).await.unwrap();
    let memory = crate::memory::InMemoryEventStore::new();
    let first = TranscriptCursor {
        lamport: 7,
        event_id: EventId::from_u128(0x7),
    };
    let second = TranscriptCursor {
        lamport: 9,
        event_id: EventId::from_u128(0x9),
    };
    let tied = TranscriptCursor {
        lamport: second.lamport,
        event_id: EventId::from_u128(0xa),
    };

    // One contract scenario exercises the actual ORM and its in-memory
    // adapter, including equal and backwards full-tuple checkpoints.
    for backend in [&store as &dyn EventStore, &memory as &dyn EventStore] {
        assert!(backend
            .load_runtime_cursor("codex-hook:default")
            .await
            .unwrap()
            .is_none());
        backend
            .save_runtime_cursor("join-feed:codex:thread-1", &first, 1)
            .await
            .unwrap();
        for (saved, expected) in [
            (&first, &first),
            (&second, &second),
            (&first, &second),
            (&second, &second),
            (&tied, &tied),
            (&second, &tied),
        ] {
            backend
                .save_runtime_cursor("codex-hook:default", saved, 2)
                .await
                .unwrap();
            assert_eq!(
                backend
                    .load_runtime_cursor("codex-hook:default")
                    .await
                    .unwrap(),
                Some(expected.clone())
            );
        }
        assert_eq!(
            backend
                .load_runtime_cursor("join-feed:codex:thread-1")
                .await
                .unwrap(),
            Some(first.clone()),
            "another consumer's progress is independent"
        );
    }

    // Distinct pools race on the same durable row. The database must own
    // the comparison so an older or equal-lamport save cannot win last.
    let other_handle = SqliteEventStore::open_path(&path).await.unwrap();
    let newest = TranscriptCursor {
        lamport: 10,
        event_id: EventId::from_u128(0xc),
    };
    let older_tie = TranscriptCursor {
        lamport: newest.lamport,
        event_id: EventId::from_u128(0xb),
    };
    let (high, low, tie) = tokio::join!(
        store.save_runtime_cursor("codex-hook:default", &newest, 3),
        other_handle.save_runtime_cursor("codex-hook:default", &first, 4),
        other_handle.save_runtime_cursor("codex-hook:default", &older_tie, 5),
    );
    high.unwrap();
    low.unwrap();
    tie.unwrap();
    drop(other_handle);
    drop(store);
    let reopened = SqliteEventStore::open_path(&path).await.unwrap();
    assert_eq!(
        reopened
            .load_runtime_cursor("codex-hook:default")
            .await
            .unwrap(),
        Some(newest),
        "reopening retains the maximum tuple despite concurrent stale saves"
    );
    assert_eq!(
        reopened
            .load_runtime_cursor("join-feed:codex:thread-1")
            .await
            .unwrap(),
        Some(first)
    );
    let before_signed_boundary = TranscriptCursor {
        lamport: i64::MAX as u64,
        event_id: EventId::from_u128(0xf0),
    };
    let after_signed_boundary = TranscriptCursor {
        lamport: i64::MAX as u64 + 1,
        event_id: EventId::from_u128(0xf1),
    };
    let upper = TranscriptCursor {
        lamport: u64::MAX,
        event_id: EventId::from_u128(0xf2),
    };
    for backend in [&reopened as &dyn EventStore, &memory as &dyn EventStore] {
        for (saved, expected) in [
            (&before_signed_boundary, &before_signed_boundary),
            (&after_signed_boundary, &after_signed_boundary),
            (&before_signed_boundary, &after_signed_boundary),
            (&upper, &upper),
            (&after_signed_boundary, &upper),
        ] {
            backend
                .save_runtime_cursor("codex-hook:default", saved, 6)
                .await
                .unwrap();
            assert_eq!(
                backend
                    .load_runtime_cursor("codex-hook:default")
                    .await
                    .unwrap(),
                Some(expected.clone()),
                "the signed column must retain unsigned transcript order"
            );
        }
    }
}

mod peer_state;
