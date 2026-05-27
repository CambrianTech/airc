//! Acceptance test 2 — crash-safe seq (§3.8 generational order, §11.1).
//!
//! Publish N; simulate a restart (rebuild `SeqSource` with epoch+1, counter
//! seeded from the sink's durable max — which is < N because some weren't
//! flushed); publish more. Assert no `seq` is ever reissued and the total
//! order is monotonic: every post-restart event sorts strictly AFTER every
//! pre-restart event.

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use airc_bus::{RouterConfig, Seq};
use airc_core::RoomId;

use common::{durable, Owner};

#[tokio::test]
async fn restart_never_reissues_a_seq_and_order_is_monotonic() {
    let ch = RoomId::from_u128(0x5e9);

    // --- pre-crash daemon ---
    let epoch_store = airc_bus::InMemoryEpochStore::new();
    let sink = Arc::new(airc_bus::InMemoryDurableSink::new());

    let owner1 = Owner::with_parts(
        RouterConfig::default(),
        epoch_store.clone(),
        sink.clone(),
        0,
    );

    // Publish N=50; capture the seqs the live subscribers "observed."
    let mut pre_seqs = Vec::new();
    for i in 1..=50u128 {
        let seq = owner1
            .router
            .publish(durable(ch, i, &format!("pre{i}")))
            .await
            .unwrap();
        pre_seqs.push(seq);
    }
    assert_eq!(owner1.epoch_store.current(), 1, "pre-crash epoch is 1");

    // Only SOME are flushed before the crash — drain only briefly, then drop
    // the router (== crash). The sink's durable max is < 50.
    tokio::time::sleep(Duration::from_millis(15)).await;
    let durable_max = sink.max_counter();
    // (We don't strictly require it to be < 50, but the test is meaningful
    // when the counter seed is below the live high-water; assert the seeding
    // models "rebuild from ORM max" regardless.)

    // "Crash": drop owner1 entirely. Its in-memory counter is gone.
    drop(owner1);

    // --- restart: same persisted epoch store + same sink, seed counter from
    // the sink's durable max (the "rebuilt from ORM" counter, which rewinds).
    let owner2 = Owner::with_parts(
        RouterConfig::default(),
        epoch_store.clone(),
        sink.clone(),
        durable_max,
    );
    assert_eq!(
        owner2.epoch_store.current(),
        2,
        "restart bumped the persisted epoch to 2 (§3.8)"
    );

    let mut post_seqs = Vec::new();
    for i in 1..=50u128 {
        let seq = owner2
            .router
            .publish(durable(ch, 1000 + i, &format!("post{i}")))
            .await
            .unwrap();
        post_seqs.push(seq);
    }

    // (1) No seq is ever reissued across the whole lifetime.
    let mut all: Vec<Seq> = pre_seqs.iter().chain(post_seqs.iter()).copied().collect();
    let unique: HashSet<Seq> = all.iter().copied().collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "no (epoch, counter) pair is reissued across the restart"
    );

    // (2) Every post-restart event sorts strictly AFTER every pre-restart one.
    let max_pre = *pre_seqs.iter().max().unwrap();
    let min_post = *post_seqs.iter().min().unwrap();
    assert!(
        min_post > max_pre,
        "post-restart min {min_post} must exceed pre-restart max {max_pre} \
         (epoch dominates even though counter rewound: pre counters reached {}, \
          post counters restart at {durable_max})",
        max_pre.counter
    );

    // (3) The total order is globally monotonic when sorted — pre block then
    // post block, no interleave.
    all.sort();
    let split = all.iter().position(|s| s.epoch == 2).unwrap();
    assert!(
        all[..split].iter().all(|s| s.epoch == 1) && all[split..].iter().all(|s| s.epoch == 2),
        "sorted order is all epoch-1 then all epoch-2 — no interleave"
    );
}
