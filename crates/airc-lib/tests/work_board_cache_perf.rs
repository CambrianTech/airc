//! Perf harness for the persistent work-board projection cache
//! (card 1291173d). `#[ignore]` — run on demand:
//!
//! ```sh
//! cargo test -p airc-lib --release --test work_board_cache_perf -- --ignored --nocapture
//! ```
//!
//! Seeds a synthetic 5k-work-event room through the real in-process
//! daemon (isolated temp homes — never the operator's `~/.airc`),
//! then times `work_board_complete`:
//!
//! - **cold** (no snapshot) — the from-scratch full replay every call
//!   paid before this card;
//! - **warm** (snapshot at tip) — the steady-state board/merger tick;
//! - **incremental** (snapshot + a handful of new events) — the
//!   typical agent loop between two reads.

mod common;

use std::time::Instant;

use airc_lib::{Airc, CardState, ChangeWorkCardState, CreateWorkCard, Priority, RepoId};
use common::Machine;

const SEED_CARDS: usize = 2_500; // 2 events per card ⇒ 5k work events
const CACHE_DIR: &str = "work-board-cache";

async fn board(airc: &Airc) -> airc_lib::WorkBoardProjection {
    airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await
        .expect("work board")
}

#[tokio::test]
#[ignore = "perf harness — run with --ignored --nocapture"]
async fn measure_board_projection_cold_vs_cached() {
    let machine = Machine::boot().await;
    let airc = machine.solo("board-cache-perf").await;
    let repo = RepoId::new("test-org/test-repo").expect("repo id");

    let seed_start = Instant::now();
    for n in 0..SEED_CARDS {
        let card_id = airc
            .create_work_card(CreateWorkCard {
                repo: repo.clone(),
                title: format!("synthetic card {n}"),
                body: None,
                priority: Priority::P2,
                lane_id: None,
                reviews: None,
            })
            .await
            .expect("seed card");
        airc.change_work_card_state(ChangeWorkCardState {
            card_id,
            state: CardState::Closed,
        })
        .await
        .expect("close card");
    }
    eprintln!(
        "seeded {} work events in {:.1?}",
        SEED_CARDS * 2,
        seed_start.elapsed()
    );

    // Cold: no snapshot ⇒ the full from-scratch replay (the only path
    // that existed before card 1291173d).
    let cache_dir = airc.home().join(CACHE_DIR);
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cold_start = Instant::now();
    let cold = board(&airc).await;
    let cold_elapsed = cold_start.elapsed();
    assert_eq!(cold.snapshot().cards.len(), SEED_CARDS);
    eprintln!("cold full replay:        {cold_elapsed:.1?}");

    // Warm: snapshot at tip ⇒ one empty resume + one tip probe.
    let warm_start = Instant::now();
    let warm = board(&airc).await;
    let warm_elapsed = warm_start.elapsed();
    assert_eq!(warm.snapshot().cards.len(), SEED_CARDS);
    eprintln!("warm cached (at tip):    {warm_elapsed:.1?}");

    // Incremental: a few new events since the snapshot.
    airc.create_work_card(CreateWorkCard {
        repo: repo.clone(),
        title: "fresh card".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        reviews: None,
    })
    .await
    .expect("fresh card");
    let inc_start = Instant::now();
    let incremental = board(&airc).await;
    let inc_elapsed = inc_start.elapsed();
    assert_eq!(incremental.snapshot().cards.len(), SEED_CARDS + 1);
    eprintln!("incremental (+2 events): {inc_elapsed:.1?}");
}
