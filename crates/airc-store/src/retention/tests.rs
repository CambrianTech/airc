#![cfg(test)]
use super::*;
use crate::bus_sink::SqliteDurableSink;
use crate::store::EventStore;
use airc_bus::envelope::{DeliveryClass, Envelope, Kind};
use airc_bus::{DurableSink, Seq};
use airc_core::{
    Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
    TranscriptKind,
};
use sea_orm::PaginatorTrait;

fn bus_event(channel: RoomId, counter: u64, headers: &[(&str, &str)]) -> Envelope {
    let mut e = Envelope::new(
        channel,
        (PeerId::from_u128(0xa1), ClientId::from_u128(0xc1)),
        Kind::Event,
        DeliveryClass::Durable,
        bytes::Bytes::from_static(b"payload"),
    );
    e = e.with_event_id(EventId::from_u128(0x5000 + u128::from(counter)));
    for (k, v) in headers {
        e = e.with_header(*k, *v);
    }
    e.seq = Seq::new(1, counter);
    e.occurred_at_ms = 1_700_000_000_000 + counter;
    e
}

fn transcript_event(room: RoomId, lamport: u64, body: serde_json::Value) -> TranscriptEvent {
    transcript_event_with(room, lamport, body, &[])
}

fn transcript_event_with(
    room: RoomId,
    lamport: u64,
    body: serde_json::Value,
    headers: &[(&str, &str)],
) -> TranscriptEvent {
    let mut h = Headers::new();
    for (k, v) in headers {
        h.insert((*k).to_owned(), (*v).to_owned());
    }
    TranscriptEvent {
        event_id: EventId::new(),
        room_id: room,
        peer_id: PeerId::from_u128(0xa1),
        client_id: ClientId::from_u128(0xc1),
        kind: TranscriptKind::Message,
        occurred_at_ms: 1_700_000_000_000 + lamport,
        lamport,
        target: MentionTarget::All,
        headers: h,
        body: Some(Body::Json(body)),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}

// regression for card 6781d7e9 (BigMama, 2026-09-26: 2.65 GB store, daemon
// acquires > 2 s, node dark 7 h behind it).
// what this catches: the drain removes exactly the two dead classes — a
// heartbeat row and a paging frame go, a durable event and a message with a
// body stay — and reports a footprint the file's length cannot lie about. The
// paging frame is picked by its HEADER class (`request_response`, as the
// backfill exchange stamps it), and a message stamped `durable` explicitly
// stays beside a header-less one: absent means durable.
#[tokio::test]
async fn the_drain_removes_the_two_dead_classes_and_nothing_else() {
    let dir = tempfile::tempdir().expect("test: tempdir");
    let path = dir.path().join("events.sqlite");
    // Both writers on the ONE file, as the daemon has them.
    let store = SqliteEventStore::open_path(&path)
        .await
        .expect("test: store");
    let sink = SqliteDurableSink::open_path(&path)
        .await
        .expect("test: sink");
    let ch = RoomId::from_u128(0xc0ffee);
    sink.append(&bus_event(
        ch,
        0,
        &[
            ("airc.heartbeat.kind", "alive"),
            ("airc.heartbeat.runtime", "agent"),
        ],
    ))
    .await
    .expect("test: heartbeat row");
    sink.append(&bus_event(ch, 1, &[("h", "durable")]))
        .await
        .expect("test: durable row");
    store
        .append(transcript_event_with(
            ch,
            1,
            serde_json::json!({"before": null, "channel": ch.to_string(), "cursor_paging": true, "limit": 200}),
            &[("airc.delivery_class", "request_response")],
        ))
        .await
        .expect("test: paging frame");
    store
        .append(transcript_event(
            ch,
            2,
            serde_json::json!({"text": "a real message"}),
        ))
        .await
        .expect("test: message");
    store
        .append(transcript_event_with(
            ch,
            3,
            serde_json::json!({"text": "an explicitly durable message"}),
            &[("airc.delivery_class", "durable")],
        ))
        .await
        .expect("test: explicit durable message");
    // A non-durable row that is NOT a paging frame: only the header class can
    // tell it is dead (a body test for `cursor_paging` would keep it forever).
    store
        .append(transcript_event_with(
            ch,
            4,
            serde_json::json!({"typing": true}),
            &[("airc.delivery_class", "ephemeral_latest")],
        ))
        .await
        .expect("test: pre-gate ephemeral row");

    let report = store.drain().await.expect("test: drain");
    assert_eq!(
        (report.heartbeat_rows, report.non_durable_rows),
        (1, 2),
        "exactly the dead classes"
    );
    assert!(
        report.after.file_bytes > 0,
        "the footprint is SQLite's own accounting"
    );

    let bus_left = sink.page(ch, None, 100).await.expect("test: page");
    assert_eq!(bus_left.len(), 1, "the durable event stays");
    assert!(
        !bus_left[0].headers.contains_key("airc.heartbeat.kind"),
        "the heartbeat is gone"
    );
    let events_left = store
        .page_recent(Some(ch), 100)
        .await
        .expect("test: recent");
    assert_eq!(events_left.len(), 2, "both durable messages stay");
    assert!(
        events_left
            .iter()
            .any(|e| matches!(&e.body, Some(Body::Json(v)) if v["text"] == "a real message")),
        "the header-less message stays intact"
    );

    // A second pass finds nothing: the drain is idempotent.
    let again = store.drain().await.expect("test: drain again");
    assert_eq!((again.heartbeat_rows, again.non_durable_rows), (0, 0));
}

// what this catches (2026-11-03, IntelMac: 2,205,371 of a resident store's rows were identity
// cards and 1,626,658 of a project store's were cursor advances): the one drain reading each
// kind's declared retention. A dead-on-arrival kind goes whole; a newest-per-key kind keeps
// exactly its newest row per key (room and peer, or room); history stays; the header rows of
// a deleted event go with it; windows smaller than the table cover it; a second pass finds
// nothing.
#[tokio::test]
async fn the_drain_removes_what_each_kind_declares_dead_and_keeps_the_newest_per_key() {
    let dir = tempfile::tempdir().expect("test: tempdir");
    let store = SqliteEventStore::open_path(&dir.path().join("events.sqlite"))
        .await
        .expect("test: store");
    let (room, other_room) = (RoomId::from_u128(0xa), RoomId::from_u128(0xb));
    let (ada, bo) = (PeerId::from_u128(0xad), PeerId::from_u128(0xb0));
    let row = |kind, room, peer, lamport| {
        let mut event = transcript_event_with(room, lamport, serde_json::json!({}), &[("k", "v")]);
        event.kind = kind;
        event.peer_id = peer;
        event
    };
    use TranscriptKind::{DoctrinePublished, IdentityPublished, Message, SubscriptionAdvanced};
    let rows = [
        row(IdentityPublished, room, ada, 1),
        row(IdentityPublished, room, ada, 2),
        row(IdentityPublished, room, ada, 3),
        row(IdentityPublished, room, bo, 4),
        row(IdentityPublished, other_room, ada, 5),
        row(SubscriptionAdvanced, room, ada, 6),
        row(SubscriptionAdvanced, room, bo, 7),
        row(DoctrinePublished, room, ada, 8),
        row(DoctrinePublished, room, bo, 9),
        row(Message, room, ada, 10),
    ];
    for event in rows.iter().cloned() {
        store.append(event).await.expect("test: append");
    }

    let report = store
        .drain_windowed(Window::fixed(3), Pace::never())
        .await
        .expect("test: drain");
    assert_eq!(
        report.declared_rows, 5,
        "two old cards, two cursor advances, one old doctrine"
    );

    let mut left: Vec<u64> = store
        .page_recent(None, 100)
        .await
        .expect("test: recent")
        .iter()
        .map(|event| event.lamport)
        .collect();
    left.sort_unstable();
    assert_eq!(
        left,
        [3, 4, 5, 9, 10],
        "the newest card per room and peer, the newest doctrine per room, the message"
    );
    let headers = crate::entities::event_header::Entity::find()
        .count(store.connection())
        .await
        .expect("test: header rows");
    assert_eq!(headers, 5, "a deleted event's header rows went with it");

    let again = store
        .drain_windowed(Window::fixed(3), Pace::never())
        .await
        .expect("test: drain again");
    assert_eq!(again.declared_rows, 0, "idempotent");
}

// what this catches (Fable on #1653: a fixed rest is a constant duty, and the drain gets stuck
// at it): a pace that does not move both ways. Over the bar the rest doubles from one budget; back
// under it, the rest shrinks one budget at a time to zero; never below zero.
#[test]
fn the_pace_grows_on_a_latency_excursion_and_shrinks_back_after() {
    let baseline = std::time::Duration::from_millis(2);
    let mut pace = Pace::against(baseline);
    let bar = baseline + STATEMENT_BUDGET;
    assert_eq!(pace.bar(), bar);
    assert_eq!(
        pace.rest(),
        std::time::Duration::ZERO,
        "no rest before any excursion"
    );
    let over = Some(bar + std::time::Duration::from_millis(10));
    pace.observe(over);
    assert_eq!(
        pace.rest(),
        STATEMENT_BUDGET,
        "the first excursion rests one budget"
    );
    pace.observe(over);
    pace.observe(over);
    assert_eq!(
        pace.rest(),
        STATEMENT_BUDGET * 4,
        "it doubles while over the bar"
    );
    pace.observe(None);
    pace.observe(None);
    assert_eq!(
        pace.rest(),
        STATEMENT_BUDGET * 4,
        "no foreground write, no evidence: the rest holds"
    );
    for left in [3, 2, 1, 0] {
        pace.observe(Some(baseline));
        assert_eq!(
            pace.rest(),
            STATEMENT_BUDGET * left,
            "under the bar it shrinks one budget a step"
        );
    }
    pace.observe(Some(baseline));
    assert_eq!(pace.rest(), std::time::Duration::ZERO, "never below zero");
    let mut never = Pace::never();
    never.observe(over);
    assert_eq!(
        never.rest(),
        std::time::Duration::ZERO,
        "a pace with no bar never rests"
    );
}

// what this catches (2026-11-03: a fixed 10,000-row window held the store's one connection
// ~0.7 s a statement and the newest-page read beside it took 1.2 s p50): a drain statement
// sized by a count instead of by its cost. The next span scales toward the budget, never
// more than doubles, never drops below one row; a fixed window never moves.
#[test]
fn a_window_is_sized_from_what_the_last_statement_cost() {
    let mut window = Window::adaptive();
    let first = window.rows();
    window.after(STATEMENT_BUDGET * 4);
    assert_eq!(
        window.rows(),
        first / 4,
        "four times over budget: a quarter the span"
    );
    let shrunk = window.rows();
    window.after(std::time::Duration::ZERO);
    assert_eq!(window.rows(), shrunk * 2, "free: at most double");
    window.after(STATEMENT_BUDGET / 100);
    assert_eq!(
        window.rows(),
        shrunk * 4,
        "far under budget: still at most double a step"
    );
    window.after(std::time::Duration::from_secs(60));
    assert_eq!(window.rows(), 1, "never below one row");
    let mut fixed = Window::fixed(3);
    fixed.after(std::time::Duration::from_secs(60));
    assert_eq!(fixed.rows(), 3, "a fixed window never moves");
}

// what this catches: the part a DELETE cannot do. The freelist a drain leaves
// behind is only returned by a vacuum; the full pass shrinks the file and
// enables incremental reclaim for every pass after it.

// what this catches (the M5, 2026-11-03, card e719540d): a WAL that never shrinks because
// nothing checkpoints it outside a full vacuum, and a reader that pins it, unreported. An
// open read transaction on another connection makes the checkpoint report PINNED without
// waiting on it; once the reader ends, the next checkpoint copies every frame and truncates
// the file to zero.
#[tokio::test]
async fn an_open_reader_pins_the_wal_and_a_checkpoint_says_so_then_truncates() {
    use sea_orm::{EntityTrait, TransactionTrait};
    let dir = tempfile::tempdir().expect("test: tempdir");
    let path = dir.path().join("events.sqlite");
    let store = SqliteEventStore::open_path(&path)
        .await
        .expect("test: store");
    let sink = SqliteDurableSink::open_path(&path)
        .await
        .expect("test: sink");
    let ch = RoomId::from_u128(0xcafe);
    for counter in 0..200u64 {
        sink.append(&bus_event(ch, counter, &[]))
            .await
            .expect("test: row");
    }
    // A second connection holds a read snapshot open, as the M5's supervisor join did.
    let reader = sea_orm::Database::connect(format!("sqlite://{}?mode=ro", path.display()))
        .await
        .expect("test: reader");
    let snapshot = reader.begin().await.expect("test: read txn");
    crate::entities::bus_event::Entity::find()
        .one(&snapshot)
        .await
        .expect("test: snapshot read");
    // Writes after the snapshot stay in the log past it.
    for counter in 200..400u64 {
        sink.append(&bus_event(ch, counter, &[]))
            .await
            .expect("test: row");
    }
    let started = std::time::Instant::now();
    let pinned = store.checkpoint_wal().await.expect("test: checkpoint");
    assert!(pinned.pinned(), "an open reader holds the log: {pinned:?}");
    assert!(
        started.elapsed() < crate::sqlite::POOL_ACQUIRE_TIMEOUT / 2,
        "a pinned checkpoint returns at once, never waiting out the busy timeout behind the reader"
    );
    assert!(
        !pinned.truncated,
        "a pinned log is never truncated (no wait behind the reader)"
    );

    snapshot.rollback().await.expect("test: end the read");
    drop(reader);
    let clear = store.checkpoint_wal().await.expect("test: checkpoint");
    assert!(
        !clear.pinned(),
        "the reader gone, every frame is checkpointed: {clear:?}"
    );
    assert!(clear.truncated, "and the file is given back");
    let wal = std::fs::metadata(dir.path().join("events.sqlite-wal"))
        .map(|m| m.len())
        .unwrap_or(0); // unwrap_or: a missing -wal file is an empty log
    assert_eq!(wal, 0, "the WAL file is truncated on disk");
}

/// Heartbeat rows enough to occupy pages a drain will free.
async fn heartbeats(sink: &SqliteDurableSink, ch: RoomId, counters: std::ops::Range<u64>) {
    for counter in counters {
        sink.append(&bus_event(ch, counter, &[("airc.heartbeat.kind", "alive")]))
            .await
            .expect("test: heartbeat row");
    }
}

// what this catches (Fable on #1653: stores were born auto_vacuum=0, so every one would one day
// need the full rebuild that holds every write): a new store that is not incremental from
// birth. A drain's freed pages come back in bounded steps, the file shrinks on disk, and no
// rebuild runs. Windows far below the row count cover the whole table.
#[tokio::test]
async fn a_new_store_is_incremental_and_a_drain_gives_its_pages_back_in_steps() {
    let dir = tempfile::tempdir().expect("test: tempdir");
    let path = dir.path().join("events.sqlite");
    let store = SqliteEventStore::open_path(&path)
        .await
        .expect("test: store");
    let sink = SqliteDurableSink::open_path(&path)
        .await
        .expect("test: sink");
    assert!(
        store.is_incremental().await.expect("test: mode"),
        "born incremental"
    );
    heartbeats(&sink, RoomId::from_u128(0xbeef), 0..2_000).await;
    let before = store.footprint().await.expect("test: footprint");
    let drained = store
        .drain_windowed(Window::fixed(64), Pace::never())
        .await
        .expect("test: drain");
    assert_eq!(drained.heartbeat_rows, 2_000);
    assert!(
        drained.after.free_bytes > 0,
        "a DELETE leaves pages on the freelist"
    );
    assert_eq!(
        run::reclaim_for(drained.after, true),
        Some(Reclaim::Incremental)
    );
    let after = store
        .reclaim(Reclaim::Incremental)
        .await
        .expect("test: steps");
    assert_eq!(
        after.free_bytes, 0,
        "the bounded steps returned the freelist"
    );
    assert!(
        after.file_bytes < before.file_bytes,
        "{before:?} -> {after:?}"
    );
    store.checkpoint_wal().await.expect("test: checkpoint");
    let on_disk = std::fs::metadata(&path).expect("test: db file").len();
    assert!(
        on_disk <= after.file_bytes + 65_536,
        "the file on disk is the page accounting: {on_disk} vs {}",
        after.file_bytes
    );
}

// what this catches: a legacy auto_vacuum=0 file that a pass cannot shrink and that nothing
// converts. A pass leaves it alone (it reports the freelist); `compact`, in a stop window, gives
// the pages back and makes it incremental, after which it is never rebuilt again.
#[tokio::test]
async fn compact_converts_a_legacy_file_once() {
    // Each step is bounded and named: on Windows CI (airc #1654) this test outlived its job
    // with nothing to say which await held it.
    async fn step<T>(name: &str, work: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(120), work)
            .await
            .unwrap_or_else(|_| panic!("test: `{name}` did not finish within 120 s"))
    }
    let dir = tempfile::tempdir().expect("test: tempdir");
    let path = dir.path().join("events.sqlite");
    // A store as every binary before #1653 created it: migrated with no vacuum mode set.
    let raw = step(
        "connect",
        sea_orm::Database::connect(crate::sqlite::sqlite_file_url(&path)),
    )
    .await
    .expect("test: connect");
    step(
        "legacy migrate",
        <crate::migration::Migrator as sea_orm_migration::MigratorTrait>::up(&raw, None),
    )
    .await
    .expect("test: legacy migrate");
    drop(raw);
    let store = step("store open", SqliteEventStore::open_path(&path))
        .await
        .expect("test: store");
    let sink = step("sink open", SqliteDurableSink::open_path(&path))
        .await
        .expect("test: sink");
    assert!(
        !step("mode", store.is_incremental())
            .await
            .expect("test: mode"),
        "legacy"
    );
    step(
        "heartbeats",
        heartbeats(&sink, RoomId::from_u128(0xfeed), 0..2_000),
    )
    .await;
    let drained = step("drain", store.drain()).await.expect("test: drain");
    assert!(drained.after.free_bytes > 0);
    assert_eq!(
        run::reclaim_for(drained.after, false),
        None,
        "a pass never rebuilds"
    );
    let compacted = step("compact", store.compact())
        .await
        .expect("test: compact");
    assert_eq!(
        compacted.free_bytes, 0,
        "the rebuild returned every free page"
    );
    assert!(
        step("mode after", store.is_incremental())
            .await
            .expect("test: mode"),
        "and made it incremental"
    );
    assert_eq!(
        step("compact again", store.compact())
            .await
            .expect("test: again"),
        step("footprint", store.footprint())
            .await
            .expect("test: footprint"),
        "an incremental file is left as it is"
    );
}
