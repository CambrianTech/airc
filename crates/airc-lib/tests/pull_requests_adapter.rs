//! Integration: PR observation publishes typed work events.
//!
//! Drives [`Airc::observe_pull_requests`] against the in-memory stub
//! source and asserts the resulting events land on the substrate
//! transcript stream. Proves the adapter contract is wired without
//! taking a dependency on `gh` or network I/O.

mod common;

use airc_core::TranscriptKind;
use airc_lib::ObservePullRequests;
use airc_work::{
    BranchName, InMemoryPullRequestSource, PrCheckState, PrMergeState, PrReviewState,
    PullRequestObserver, PullRequestRef, PullRequestSnapshot, RepoId, RepoPullRequestSnapshot,
};
use common::Machine;
use std::collections::HashMap;

fn test_repo() -> RepoId {
    RepoId::new("test-org/test-repo").unwrap()
}

fn snapshot_with_one_pr(check: PrCheckState, merge: PrMergeState) -> RepoPullRequestSnapshot {
    let pr = PullRequestSnapshot {
        pull_request: PullRequestRef {
            repo: test_repo(),
            number: 1,
            head: BranchName::new("test-head").unwrap(),
            base: BranchName::new("test-base").unwrap(),
        },
        check_state: check,
        merge_state: merge,
        reviews: HashMap::new(),
    };
    RepoPullRequestSnapshot {
        repo: test_repo(),
        pulls: [(1, pr)].into_iter().collect(),
    }
}

#[tokio::test]
async fn observe_publishes_check_and_merge_on_cold_start() {
    let machine = Machine::boot().await;
    let airc = machine.solo("pr-adapter-test").await;

    let source = InMemoryPullRequestSource::new().with_snapshot(snapshot_with_one_pr(
        PrCheckState::Running,
        PrMergeState::Open,
    ));
    let observer = PullRequestObserver::new(source);

    let observed = airc
        .observe_pull_requests(
            &observer,
            ObservePullRequests {
                repo: test_repo(),
                previous: None,
            },
        )
        .await
        .expect("observe");

    assert_eq!(
        observed.emitted_event_ids.len(),
        2,
        "cold start with one PR emits a check event + a merge event"
    );

    let page = airc.page_recent(64).await.expect("page");
    let check_count = page
        .iter()
        .filter(|e| {
            airc_work::transcript_is_work_event(e)
                && matches!(e.kind, TranscriptKind::Message | TranscriptKind::System)
        })
        .count();
    // Sanity: at least the two we just emitted are visible on the
    // transcript. The exact kind tagging is the substrate codec's
    // call; what matters is that two work events surfaced.
    assert!(
        check_count >= 2,
        "expected at least 2 work events on the transcript, found {check_count}"
    );
}

#[tokio::test]
async fn observe_emits_nothing_when_snapshot_unchanged() {
    let machine = Machine::boot().await;
    let airc = machine.solo("pr-adapter-idempotent").await;

    let snapshot = snapshot_with_one_pr(PrCheckState::Passed, PrMergeState::Ready);
    let source = InMemoryPullRequestSource::new().with_snapshot(snapshot.clone());
    let observer = PullRequestObserver::new(source);

    // First observation: cold start, emits 2 events (check + merge).
    let first = airc
        .observe_pull_requests(
            &observer,
            ObservePullRequests {
                repo: test_repo(),
                previous: None,
            },
        )
        .await
        .expect("first observe");
    assert_eq!(first.emitted_event_ids.len(), 2);

    // Second observation with the previous snapshot fed back: same
    // state → no events.
    let second = airc
        .observe_pull_requests(
            &observer,
            ObservePullRequests {
                repo: test_repo(),
                previous: Some(first.snapshot.clone()),
            },
        )
        .await
        .expect("second observe");
    assert_eq!(
        second.emitted_event_ids.len(),
        0,
        "unchanged snapshot must not emit events"
    );
}

#[tokio::test]
async fn observe_emits_one_event_per_review_state_change() {
    let machine = Machine::boot().await;
    let airc = machine.solo("pr-adapter-review").await;

    let reviewer = airc_core::PeerId::new();

    // Initial snapshot: no reviews.
    let initial = snapshot_with_one_pr(PrCheckState::Running, PrMergeState::Open);
    let mut source = InMemoryPullRequestSource::new().with_snapshot(initial.clone());
    let observer = PullRequestObserver::new(source.clone());

    let first = airc
        .observe_pull_requests(
            &observer,
            ObservePullRequests {
                repo: test_repo(),
                previous: None,
            },
        )
        .await
        .expect("first observe");
    assert_eq!(first.emitted_event_ids.len(), 2); // check + merge

    // Update the source with the reviewer's approval.
    let mut approved = initial.clone();
    approved
        .pulls
        .get_mut(&1)
        .unwrap()
        .reviews
        .insert(reviewer, PrReviewState::Approved);
    source.set(approved);
    let observer = PullRequestObserver::new(source);

    let second = airc
        .observe_pull_requests(
            &observer,
            ObservePullRequests {
                repo: test_repo(),
                previous: Some(first.snapshot),
            },
        )
        .await
        .expect("second observe");
    assert_eq!(
        second.emitted_event_ids.len(),
        1,
        "only the review event should emit; check + merge are unchanged"
    );
}
