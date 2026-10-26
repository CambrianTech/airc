use super::*;
use airc_bus::envelope::{DeliveryClass, Kind};
use airc_core::{ClientId, EventId, PeerId, RoomId};
use bytes::Bytes;
use uuid::Uuid;

/// Build a `Durable` envelope at a given `(epoch, counter)` with a
/// deterministic `event_id` derived from the position so tests can
/// assert exact identity and order. `event_id` is the PK, so (as with
/// real random UUIDs) it must be globally unique across channels —
/// the channel is mixed into the high half.
fn durable_at(channel: RoomId, epoch: u64, counter: u64) -> Envelope {
    durable_kind_at(channel, Kind::Message, epoch, counter)
}

/// [`durable_at`] with an explicit [`Kind`] — for the kinds-filtered
/// reverse page (continuum #297).
fn durable_kind_at(channel: RoomId, kind: Kind, epoch: u64, counter: u64) -> Envelope {
    let mut e = Envelope::new(
        channel,
        (PeerId::from_u128(0xa1), ClientId::from_u128(0xc1)),
        kind,
        DeliveryClass::Durable,
        Bytes::from(format!("payload-{epoch}-{counter}")),
    )
    .with_event_id(EventId::from_u128(
        // Distinct per (channel, epoch, counter): channel + epoch in
        // the high half, counter in the low half, +1 so no id is nil.
        ((channel.as_uuid().as_u128() ^ (u128::from(epoch) << 16)) << 64)
            | u128::from(counter).wrapping_add(1),
    ))
    .with_header("h", format!("{epoch}:{counter}"));
    e.seq = Seq::new(epoch, counter);
    e.occurred_at_ms = 1_700_000_000_000 + counter;
    e
}

#[tokio::test]
async fn append_then_page_round_trips_full_envelope() {
    // The minimum-viable proof: write one, read it back, every field
    // (incl. opaque payload + headers + target enum) intact.
    let ch = RoomId::from_u128(0xc0ffee);
    let e = durable_at(ch, 1, 0)
        .with_target(Target::Peer(PeerId::from_u128(0x99)))
        .with_correlation_id(Uuid::from_u128(0x1234))
        .with_coalesce_key("k");
    for url in [
        "sqlite::memory:",
        "sqlite://memory-pool-control?mode=memory&cache=private",
        "sqlite:",
        "sqlite:file::memory:",
    ] {
        let sink = SqliteDurableSink::open(url).await.unwrap();
        sink.append(&e).await.unwrap();
        let page = sink.page(ch, None, 100).await.unwrap();
        assert_eq!(page.len(), 1, "single database for {url}");
        assert_eq!(page[0], e, "full envelope round-trips through {url}");
    }
    // SeaORM rejects this URL before SQLx sees it; do not manufacture
    // support for an alias that the existing constructor never accepted.
    assert!(SqliteDurableSink::open("sqlite://:memory:").await.is_err());
}

#[tokio::test]
async fn read_only_owner_observation_sees_wal_and_refuses_writes_or_missing_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner # store.sqlite");
    assert!(SqliteDurableSink::open_read_only_path(&path).await.is_err());
    assert!(!path.exists(), "inspection must never create the store");
    let writer = SqliteDurableSink::open_path(&path).await.unwrap();
    let reader = SqliteDurableSink::open_read_only_path(&path).await.unwrap();
    let event = durable_at(RoomId::from_u128(7), 1, 0);
    assert!(!reader.contains(event.event_id).await.unwrap());
    writer.append(&event).await.unwrap();
    assert!(reader.contains(event.event_id).await.unwrap());
    let another = durable_at(event.channel, 1, 1);
    assert!(reader.append(&another).await.is_err());
    assert!(!writer.contains(another.event_id).await.unwrap());

    let empty = dir.path().join("uninitialized.sqlite");
    std::fs::File::create(&empty).unwrap();
    let uninitialized = SqliteDurableSink::open_read_only_path(&empty)
        .await
        .unwrap();
    assert!(uninitialized.contains(event.event_id).await.is_err());
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);
}

#[tokio::test]
async fn page_orders_by_epoch_counter_event_id() {
    // §3.8 crash-safe order: a post-crash event (higher epoch, lower
    // counter) sorts strictly AFTER a pre-crash event (lower epoch,
    // higher counter). Ordering must be (epoch, counter, event_id),
    // NOT a single lamport — append out of order, expect sorted.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(7);
    // epoch 2 counter 0 appended FIRST but must sort LAST after
    // epoch 1's counters; epoch 1 counter 2 appended before counter 1.
    sink.append(&durable_at(ch, 2, 0)).await.unwrap();
    sink.append(&durable_at(ch, 1, 2)).await.unwrap();
    sink.append(&durable_at(ch, 1, 0)).await.unwrap();
    sink.append(&durable_at(ch, 1, 1)).await.unwrap();

    let page = sink.page(ch, None, 100).await.unwrap();
    let order: Vec<(u64, u64)> = page.iter().map(|e| (e.seq.epoch, e.seq.counter)).collect();
    assert_eq!(
        order,
        vec![(1, 0), (1, 1), (1, 2), (2, 0)],
        "epoch dominates; post-crash epoch sorts after pre-crash even with lower counter"
    );
}

#[tokio::test]
async fn append_is_idempotent_on_event_id() {
    // §3.3: a replay / re-inject of the same event_id is a no-op,
    // never a duplicate row, never an error.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(1);
    let e = durable_at(ch, 1, 0);
    sink.append(&e).await.unwrap();
    sink.append(&e).await.unwrap();
    sink.append(&e).await.unwrap();

    let page = sink.page(ch, None, 100).await.unwrap();
    assert_eq!(page.len(), 1, "same event_id => exactly one row");
}

#[tokio::test]
async fn page_returns_strictly_after_cursor_and_respects_limit() {
    // Cursor pagination (§3.5): tail then page deeper. Stable, no dup
    // across pages, respects `limit`.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0x5);
    for c in 0..10u64 {
        sink.append(&durable_at(ch, 1, c)).await.unwrap();
    }

    // First page of 4 from the beginning.
    let page1 = sink.page(ch, None, 4).await.unwrap();
    assert_eq!(page1.len(), 4, "limit honored");
    let c1: Vec<u64> = page1.iter().map(|e| e.seq.counter).collect();
    assert_eq!(c1, vec![0, 1, 2, 3]);

    // Page deeper from the last cursor of page1 — strictly after, no dup.
    let cursor = page1[3].cursor();
    let page2 = sink.page(ch, Some(cursor), 4).await.unwrap();
    let c2: Vec<u64> = page2.iter().map(|e| e.seq.counter).collect();
    assert_eq!(c2, vec![4, 5, 6, 7], "strictly after, no dup across pages");

    // Final page exhausts the tail.
    let cursor = page2[3].cursor();
    let page3 = sink.page(ch, Some(cursor), 4).await.unwrap();
    let c3: Vec<u64> = page3.iter().map(|e| e.seq.counter).collect();
    assert_eq!(c3, vec![8, 9], "tail shorter than limit");

    // No event appears in two pages.
    let mut all: Vec<u64> = c1.into_iter().chain(c2).chain(c3).collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 10, "every event delivered exactly once");
}

#[tokio::test]
async fn page_strictly_after_at_same_seq_tiebreaks_on_event_id() {
    // Two events sharing an exact (epoch, counter) page in event_id
    // order; a cursor at the lower one returns only the higher.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0x1234);
    let mut lo = durable_at(ch, 5, 5);
    let mut hi = durable_at(ch, 5, 5);
    lo = lo.with_event_id(EventId::from_u128(0x1));
    hi = hi.with_event_id(EventId::from_u128(0x2));
    sink.append(&lo).await.unwrap();
    sink.append(&hi).await.unwrap();

    let page = sink.page(ch, None, 10).await.unwrap();
    assert_eq!(page[0].event_id, lo.event_id);
    assert_eq!(page[1].event_id, hi.event_id);

    let after = sink.page(ch, Some(lo.cursor()), 10).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].event_id, hi.event_id);
}

#[tokio::test]
async fn page_tail_returns_newest_n_ascending() {
    // Card 8428ae8c: the reverse page returns the LAST n in
    // ascending order — one DESC-indexed query, reversed in memory.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0x7a11);
    for c in 0..10u64 {
        sink.append(&durable_at(ch, 1, c)).await.unwrap();
    }

    let tail = sink.page_tail(ch, None, 4).await.unwrap();
    let counters: Vec<u64> = tail.iter().map(|e| e.seq.counter).collect();
    assert_eq!(counters, vec![6, 7, 8, 9], "newest 4, ascending");

    // n beyond the channel: the whole channel.
    let all = sink.page_tail(ch, None, 100).await.unwrap();
    let counters: Vec<u64> = all.iter().map(|e| e.seq.counter).collect();
    assert_eq!(counters, (0..10).collect::<Vec<u64>>());

    // Empty channel: empty tail.
    assert!(sink
        .page_tail(RoomId::from_u128(0xd0), None, 5)
        .await
        .unwrap()
        .is_empty());

    // Channel isolation: another channel's events never ride along.
    let other = RoomId::from_u128(0x7a12);
    sink.append(&durable_at(other, 1, 99)).await.unwrap();
    let tail = sink.page_tail(ch, None, 100).await.unwrap();
    assert_eq!(tail.len(), 10, "tail is per-channel");
}

#[tokio::test]
async fn page_tail_strictly_before_cursor_across_epochs() {
    // The `before` bound mirrors `page`'s strictly-after predicate
    // in generational order: epoch dominates counter, event_id
    // tiebreaks — and the row AT the cursor is excluded.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0xe9);
    // Appended out of order on purpose; total order is
    // (1,0) (1,1) (1,2) (2,0) (2,1).
    sink.append(&durable_at(ch, 2, 1)).await.unwrap();
    sink.append(&durable_at(ch, 1, 2)).await.unwrap();
    sink.append(&durable_at(ch, 1, 0)).await.unwrap();
    sink.append(&durable_at(ch, 2, 0)).await.unwrap();
    sink.append(&durable_at(ch, 1, 1)).await.unwrap();

    let before = durable_at(ch, 2, 0).cursor();
    let tail = sink.page_tail(ch, Some(before), 2).await.unwrap();
    let order: Vec<(u64, u64)> = tail.iter().map(|e| (e.seq.epoch, e.seq.counter)).collect();
    assert_eq!(
        order,
        vec![(1, 1), (1, 2)],
        "newest 2 strictly before (2,0): the cursor row is excluded, epoch dominates"
    );

    // A cursor before everything: empty tail.
    let before = durable_at(ch, 1, 0).cursor();
    assert!(sink
        .page_tail(ch, Some(before), 10)
        .await
        .unwrap()
        .is_empty());
}

/// Continuum #297: the kind predicate runs in the DATABASE, before
/// `LIMIT` — the store leg of "newest N events OF THESE KINDS".
// what this catches: `page_tail_of_kinds` regressing to a raw DESC
// page truncated before the kind filter — under a StreamChunk flood
// the newest-N raw rows are all chunks, so the filtered page would
// come back empty ("crumbs") and personas go deaf to direction.
#[tokio::test]
async fn page_tail_of_kinds_filters_in_the_database_before_limit() {
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0x297);
    // 3 messages, then 200 newer chunk-kind durable rows.
    for c in 0..3u64 {
        sink.append(&durable_at(ch, 1, c)).await.unwrap();
    }
    for c in 3..203u64 {
        sink.append(&durable_kind_at(ch, Kind::StreamChunk, 1, c))
            .await
            .unwrap();
    }

    // Newest 50 of kind Message: exactly the 3 buried messages,
    // ascending — a raw newest-50 contains zero of them.
    let tail = sink
        .page_tail_of_kinds(ch, None, &[Kind::Message], 50)
        .await
        .unwrap();
    let counters: Vec<u64> = tail.iter().map(|e| e.seq.counter).collect();
    assert_eq!(counters, vec![0, 1, 2], "newest N OF KIND, ascending");
    assert!(tail.iter().all(|e| e.kind == Kind::Message));

    // `limit` still caps the matches.
    let capped = sink
        .page_tail_of_kinds(ch, None, &[Kind::Message], 2)
        .await
        .unwrap();
    let counters: Vec<u64> = capped.iter().map(|e| e.seq.counter).collect();
    assert_eq!(counters, vec![1, 2], "newest 2 of the kind");

    // `before` composes with the kind predicate (same
    // strictly-before contract as `page_tail`).
    let before = durable_at(ch, 1, 2).cursor();
    let older = sink
        .page_tail_of_kinds(ch, Some(before), &[Kind::Message], 50)
        .await
        .unwrap();
    let counters: Vec<u64> = older.iter().map(|e| e.seq.counter).collect();
    assert_eq!(counters, vec![0, 1], "strictly before the cursor");

    // A filter admitting every stored kind == the raw reverse page.
    let both = sink
        .page_tail_of_kinds(ch, None, &[Kind::Message, Kind::StreamChunk], 5)
        .await
        .unwrap();
    let raw = sink.page_tail(ch, None, 5).await.unwrap();
    assert_eq!(
        both.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        raw.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        "all-kinds filter behaves exactly like page_tail"
    );
}

#[tokio::test]
async fn empty_channel_pages_empty() {
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let empty = RoomId::from_u128(0xdead);
    assert!(sink.page(empty, None, 100).await.unwrap().is_empty());
    let cursor = Cursor::new(Seq::new(1, 1), EventId::from_u128(1));
    assert!(sink
        .page(empty, Some(cursor), 100)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn page_isolates_channels() {
    // A page on one channel never leaks another channel's events.
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let a = RoomId::from_u128(0xaaaa);
    let b = RoomId::from_u128(0xbbbb);
    sink.append(&durable_at(a, 1, 0)).await.unwrap();
    sink.append(&durable_at(b, 1, 0)).await.unwrap();
    sink.append(&durable_at(a, 1, 1)).await.unwrap();

    assert_eq!(sink.page(a, None, 100).await.unwrap().len(), 2);
    assert_eq!(sink.page(b, None, 100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn persists_across_handles_on_disk() {
    use sea_orm::TransactionTrait;
    use std::time::Duration;

    // Real WAL isolation: exhaust the bounded reader pool, commit through
    // the reserved writer, release the reader, then reopen the same file.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bus_events.sqlite");
    let ch = RoomId::from_u128(0x42);
    let e = durable_at(ch, 1, 0);
    let sink = SqliteDurableSink::open_path(&path).await.unwrap();
    sink.append(&e).await.unwrap();
    let read_lease = sink.readers.begin().await.unwrap();
    assert_eq!(
        bus_event::Entity::find()
            .all(&read_lease)
            .await
            .unwrap()
            .len(),
        1
    );
    let mut queued_page = Box::pin(sink.page(ch, None, 100));
    assert!(futures::poll!(&mut queued_page).is_pending());

    let batch_a = durable_at(ch, 1, 1);
    let batch_b = durable_at(ch, 1, 2);
    let single = durable_at(ch, 1, 3);
    tokio::time::timeout(Duration::from_secs(5), async {
        sink.append_batch(&[&batch_a, &batch_b]).await.unwrap();
        sink.append(&single).await.unwrap();
        sink.bump_epoch().await.unwrap();
    })
    .await
    .expect("a held reader and queued replay must not consume the writer connection");
    assert_eq!(
        bus_event::Entity::find()
            .all(&read_lease)
            .await
            .unwrap()
            .len(),
        1,
        "the held read snapshot remains stable while writes commit"
    );
    assert!(futures::poll!(&mut queued_page).is_pending());
    read_lease.rollback().await.unwrap();
    let expected = vec![e, batch_a, batch_b, single];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), queued_page)
            .await
            .unwrap()
            .unwrap(),
        expected,
        "the queued replay sees all committed IDs once the reader is released"
    );
    assert_eq!(
        sink.head_cursor(ch).await.unwrap(),
        Some(expected[3].cursor())
    );
    assert!(sink.contains(expected[3].event_id).await.unwrap());
    assert_eq!(sink.page_tail(ch, None, 2).await.unwrap(), expected[2..]);
    assert_eq!(
        sink.page_tail_of_kinds(ch, None, &[Kind::Message], 100)
            .await
            .unwrap(),
        expected
    );
    let forbidden = durable_at(ch, 1, 4);
    assert!(
        bus_event::Entity::insert(to_active_model(&forbidden).unwrap())
            .exec(&sink.readers)
            .await
            .is_err(),
        "the replay pool is read-only"
    );
    let SqliteDurableSink { db, readers } = sink;
    readers.close().await.unwrap();
    db.close().await.unwrap();
    let reopened = SqliteDurableSink::open_path(&path).await.unwrap();
    let page = reopened.page(ch, None, 100).await.unwrap();
    assert_eq!(
        page, expected,
        "all committed events survive closing both pools"
    );
    reopened.readers.close().await.unwrap();
    reopened.db.close().await.unwrap();

    // An explicitly shared SQLite cache keeps the old serialized pool;
    // adding a second connection would introduce table-lock contention.
    let shared = SqliteDurableSink::open(&format!("{}&cache=shared", sqlite_file_url(&path)))
        .await
        .unwrap();
    let read_lease = shared.readers.begin().await.unwrap();
    assert!(shared
        .db
        .get_sqlite_connection_pool()
        .try_acquire()
        .is_none());
    read_lease.rollback().await.unwrap();
    shared.append(&forbidden).await.unwrap();
    assert!(shared.contains(forbidden.event_id).await.unwrap());
}

#[test]
fn kind_and_delivery_text_round_trip_every_variant() {
    // The enum<->text mapping must cover every variant both ways so a
    // future variant can't silently corrupt the durable tier.
    for kind in [
        Kind::Message,
        Kind::Event,
        Kind::Command,
        Kind::CommandResult,
        Kind::Signal,
        Kind::StreamChunk,
        Kind::Control,
    ] {
        assert_eq!(kind_from_text(kind_to_text(kind)).unwrap(), kind);
    }
    for delivery in [
        DeliveryClass::Durable,
        DeliveryClass::EphemeralLatest,
        DeliveryClass::EphemeralWindow,
        DeliveryClass::RequestResponse,
        DeliveryClass::StreamChunk,
    ] {
        assert_eq!(
            delivery_from_text(delivery_to_text(delivery)).unwrap(),
            delivery
        );
    }
}

/// THE load-bearing group-commit test (durability blocker): a batch that
/// MIXES a duplicate with new events must still persist EVERY new event.
/// This mechanically proves the one fact the write-behind group-commit
/// rests on — that `insert_many(...).on_conflict(do_nothing)` inserts the
/// new rows even when a conflicting row is present, and that mapping
/// `RecordNotInserted` → Ok cannot drop new events. If this failed, the
/// router would unpin un-persisted events = silent data loss.
#[tokio::test]
async fn append_batch_mixed_persists_every_new_event() {
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0xba7c8);
    let a = durable_at(ch, 1, 0);
    let b = durable_at(ch, 1, 1);
    let c = durable_at(ch, 1, 2);

    // A is already persisted; the batch re-includes it (duplicate) plus two
    // genuinely-new events.
    sink.append(&a).await.unwrap();
    sink.append_batch(&[&a, &b, &c]).await.unwrap();

    let page = sink.page(ch, None, 100).await.unwrap();
    assert_eq!(
        page.len(),
        3,
        "mixed batch must persist A(once)+B+C, not drop new rows"
    );
    assert_eq!(page[0], a);
    assert_eq!(page[1], b);
    assert_eq!(page[2], c);
}

/// All-duplicate batch is idempotent success (the `RecordNotInserted` arm):
/// re-appending already-persisted events neither errors nor double-stores.
#[tokio::test]
async fn append_batch_all_duplicates_is_idempotent_ok() {
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0xd00b);
    let a = durable_at(ch, 1, 0);
    let b = durable_at(ch, 1, 1);
    sink.append(&a).await.unwrap();
    sink.append(&b).await.unwrap();

    sink.append_batch(&[&a, &b]).await.unwrap(); // all dupes → Ok, no dup rows

    let page = sink.page(ch, None, 100).await.unwrap();
    assert_eq!(page.len(), 2, "all-duplicate batch must not double-store");
}

/// All-new batch persists everything (the happy path) + empty batch no-ops.
#[tokio::test]
async fn append_batch_all_new_and_empty() {
    let sink = SqliteDurableSink::in_memory().await.unwrap();
    let ch = RoomId::from_u128(0xfeed);
    sink.append_batch(&[]).await.unwrap(); // empty = no-op, no error
    let a = durable_at(ch, 1, 0);
    let b = durable_at(ch, 1, 1);
    let c = durable_at(ch, 1, 2);
    sink.append_batch(&[&a, &b, &c]).await.unwrap();

    let page = sink.page(ch, None, 100).await.unwrap();
    assert_eq!(page.len(), 3, "all-new batch persists every event");
}
#[tokio::test]
async fn cold_header_filtered_page_and_count_keep_system_events_and_cursor_ties() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cold.sqlite");
    let sink = SqliteDurableSink::open_path(&path).await.unwrap();
    let ch = RoomId::from_u128(0x777);
    let mut rows: Vec<_> = (1..=512)
        .map(|n| {
            let mut e = durable_kind_at(ch, Kind::Event, 1, n);
            e.headers.insert("airc.stream.id".into(), "legacy".into());
            e
        })
        .collect();
    let first = durable_kind_at(ch, Kind::Event, 1, 513);
    let mut second = first.clone();
    second.event_id = EventId::from_u128(u128::MAX);
    rows.push(first.clone());
    rows.push(second.clone());
    sink.append_batch(&rows.iter().collect::<Vec<_>>())
        .await
        .unwrap();
    drop(sink);
    let sink = SqliteDurableSink::open_path(&path).await.unwrap();
    let filter = airc_core::HeaderFilter::Not(Box::new(airc_core::HeaderFilter::Has {
        key: "airc.stream.id".into(),
    }));
    let origin = Cursor::new(Seq::new(0, 0), EventId::from_u128(0));
    let page = sink
        .page_matching(ch, origin, second.cursor(), &filter, 1, false)
        .await
        .unwrap();
    assert_eq!(page[0].event_id, first.event_id);
    let next = sink
        .page_matching(ch, first.cursor(), second.cursor(), &filter, 1, false)
        .await
        .unwrap();
    assert_eq!(next[0].event_id, second.event_id);
    assert_eq!(sink.page(ch, None, 600).await.unwrap().len(), 514);
    // The count is the same range, counted in the store: it agrees with the page.
    let routed = sink
        .count_matching(ch, origin, second.cursor(), &filter, None)
        .await
        .unwrap();
    assert_eq!(routed.count, 2);
    let all = sink
        .count_matching(
            ch,
            origin,
            second.cursor(),
            &airc_core::HeaderFilter::Any,
            None,
        )
        .await
        .unwrap();
    assert_eq!(all.count, 514);
    assert!(all.oldest_ms <= all.newest_ms);
    let empty = sink
        .count_matching(ch, second.cursor(), second.cursor(), &filter, None)
        .await
        .unwrap();
    assert_eq!(empty, airc_core::HistoryCount::default());
}
