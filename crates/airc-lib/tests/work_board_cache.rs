//! Integration: persistent work-board projection cache (card 1291173d).
//!
//! `Airc::work_board_complete` used to replay the room's full
//! transcript from event zero on every call; it now snapshots the
//! projection keyed by the last-applied transcript cursor and resumes
//! incrementally. These tests run the REAL daemon path (in-process
//! owner-core, real Unix socket, isolated temp homes — never the
//! operator's `~/.airc`) and pin:
//!
//! - the incremental resume produces the same board as a from-scratch
//!   rebuild (delete the snapshot, rebuild, compare);
//! - the snapshot is actually served (a doctored snapshot at the true
//!   tip cursor is visible in the result — this is the mutation pin:
//!   breaking the increment path and always rebuilding fails it);
//! - a snapshot whose cursor the log no longer agrees with is
//!   discarded and rebuilt from scratch, never served stale;
//! - a corrupt snapshot file is discarded and rebuilt.

mod common;

use airc_lib::{Airc, CreateWorkCard, Priority, RepoId};
use common::Machine;

const CACHE_DIR: &str = "work-board-cache";

async fn create_card(airc: &Airc, title: &str) -> airc_lib::WorkCardId {
    airc.create_work_card(CreateWorkCard {
        repo: RepoId::new("test-org/test-repo").unwrap(),
        title: title.to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        reviews: None,
    })
    .await
    .expect("create work card")
}

/// The single snapshot file this scope's board reads persisted.
fn cache_file(airc: &Airc) -> std::path::PathBuf {
    let dir = airc.home().join(CACHE_DIR);
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("cache dir exists after a board read")
        .map(|entry| entry.expect("cache dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one room snapshot in {}",
        dir.display()
    );
    entries.pop().expect("one entry")
}

fn card_titles(board: &airc_lib::WorkBoardProjection) -> Vec<String> {
    let mut titles: Vec<String> = board
        .snapshot()
        .cards
        .into_iter()
        .map(|card| card.title)
        .collect();
    titles.sort();
    titles
}

#[tokio::test]
async fn incremental_resume_matches_from_scratch_rebuild() {
    let machine = Machine::boot().await;
    let airc = machine.solo("board-cache-equivalence").await;

    create_card(&airc, "first card").await;
    let cold = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("cold board");
    assert_eq!(card_titles(&cold), vec!["first card".to_string()]);
    let path = cache_file(&airc);
    assert!(path.exists(), "first board read persists a snapshot");

    // New events after the snapshot: the next read resumes from the
    // cached cursor and folds them in.
    create_card(&airc, "second card").await;
    let resumed = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("resumed board");
    assert_eq!(
        card_titles(&resumed),
        vec!["first card".to_string(), "second card".to_string()]
    );

    // Ground truth: delete the snapshot and replay from event zero.
    // The incremental fold must be indistinguishable from it.
    std::fs::remove_file(&path).expect("drop snapshot");
    let rebuilt = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("rebuilt board");
    assert_eq!(
        resumed.snapshot().cards,
        rebuilt.snapshot().cards,
        "incremental resume diverged from full replay"
    );
}

#[tokio::test]
async fn snapshot_at_tip_is_served_without_replay() {
    let machine = Machine::boot().await;
    let airc = machine.solo("board-cache-served").await;

    create_card(&airc, "true title").await;
    airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("board read to persist snapshot");

    // Doctor the snapshot's projection while leaving its cursor at the
    // true room tip. A served snapshot shows the doctored title; a
    // code path that quietly rebuilt from the log instead would show
    // the true title and this pin would fail — that is the point: it
    // proves the increment path is live.
    let path = cache_file(&airc);
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read snapshot")).expect("decode");
    let cards = cache["projection"]["cards"]
        .as_object_mut()
        .expect("projection has cards");
    assert_eq!(cards.len(), 1);
    for card in cards.values_mut() {
        card["title"] = serde_json::Value::String("doctored title".to_string());
    }
    std::fs::write(&path, serde_json::to_vec(&cache).expect("encode")).expect("write snapshot");

    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("board");
    assert_eq!(
        card_titles(&board),
        vec!["doctored title".to_string()],
        "snapshot at the tip cursor must be served, not rebuilt"
    );
}

#[tokio::test]
async fn snapshot_with_rewound_log_is_rebuilt_never_served_stale() {
    let machine = Machine::boot().await;
    let airc = machine.solo("board-cache-stale").await;

    create_card(&airc, "true title").await;
    airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("board read to persist snapshot");

    // Doctor the snapshot AND push its cursor past the log tip — the
    // shape a wiped/rewound store leaves behind. The read must detect
    // that the log no longer agrees with the cursor and rebuild; the
    // doctored state must never surface.
    let path = cache_file(&airc);
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read snapshot")).expect("decode");
    for card in cache["projection"]["cards"]
        .as_object_mut()
        .expect("projection has cards")
        .values_mut()
    {
        card["title"] = serde_json::Value::String("stale title".to_string());
    }
    cache["cursor"]["lamport"] = serde_json::Value::from(u64::MAX);
    std::fs::write(&path, serde_json::to_vec(&cache).expect("encode")).expect("write snapshot");

    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("board");
    assert_eq!(
        card_titles(&board),
        vec!["true title".to_string()],
        "a snapshot the log disagrees with must be rebuilt from scratch"
    );

    // And the recovery rewrites a healthy snapshot at the real tip.
    let healed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read snapshot")).expect("decode");
    assert_ne!(
        healed["cursor"]["lamport"],
        serde_json::Value::from(u64::MAX)
    );
}

#[tokio::test]
async fn corrupt_snapshot_is_discarded_and_rebuilt() {
    let machine = Machine::boot().await;
    let airc = machine.solo("board-cache-corrupt").await;

    create_card(&airc, "survives corruption").await;
    airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("board read to persist snapshot");

    let path = cache_file(&airc);
    std::fs::write(&path, b"{ definitely not a snapshot").expect("corrupt snapshot");

    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("board");
    assert_eq!(card_titles(&board), vec!["survives corruption".to_string()]);
}
