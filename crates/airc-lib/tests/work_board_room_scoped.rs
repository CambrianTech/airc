//! Continuum #345: a work board must be readable for a NAMED room, and the
//! answer must be that room's cards — never the current room's.
//!
//! `airc work board` had no room parameter at all: it read whatever room the
//! scope's default pointer happened to reference. That is not a missing
//! convenience, it is a correctness hazard, because the failure produces a
//! *plausible* board rather than an error — a real board, fully populated, for
//! the wrong room. The operator who filed this card was fooled by it within the
//! hour, and nearly reported a card released while another peer still held it.
//!
//! What this catches: a regression where the scoped read falls back to
//! `current_room()` (the exact bug), or where the resolver auto-joins /
//! silently substitutes a room instead of refusing. The two rooms below each
//! hold a card the other does not, so a fallback cannot pass by coincidence.

mod common;

use airc_lib::{Airc, CreateWorkCard, Priority, RepoId};
use common::Machine;

async fn create_card(airc: &Airc, title: &str) {
    airc.create_work_card(CreateWorkCard {
        repo: RepoId::new("test-org/test-repo").unwrap(),
        title: title.to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        reviews: None,
    })
    .await
    .expect("create work card");
}

fn titles(board: &airc_lib::WorkBoardProjection) -> Vec<String> {
    let mut titles: Vec<String> = board
        .snapshot()
        .cards
        .into_iter()
        .map(|card| card.title)
        .collect();
    titles.sort();
    titles
}

// Regression: a reviewer standing elsewhere must publish the sibling and merge
// into the parent's room, without redirecting their next unscoped command.
#[tokio::test]
async fn review_and_merge_keep_the_selected_room_without_changing_default() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;
    let target = alice.join("target-room").await.unwrap();
    let parent = alice
        .create_work_card(CreateWorkCard::new(
            RepoId::new("test-org/test-repo").unwrap(),
            "parent",
            Priority::P1,
        ))
        .await
        .unwrap();
    let home = alice.join("home-room").await.unwrap();
    let review = alice
        .create_work_card_in(
            &target,
            CreateWorkCard::new(
                RepoId::new("test-org/test-repo").unwrap(),
                "review",
                Priority::P1,
            )
            .reviewing(parent),
        )
        .await
        .unwrap();
    let request = airc_lib::MarkPullRequestMerged {
        card_id: parent,
        pull_request: airc_work::PullRequestRef {
            repo: RepoId::new("test-org/test-repo").unwrap(),
            number: 1,
            head: airc_work::BranchName::new("feature").unwrap(),
            base: airc_work::BranchName::new("canary").unwrap(),
        },
        merged_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    assert!(alice
        .mark_pull_request_merged(request.clone())
        .await
        .is_err());
    alice
        .mark_pull_request_merged_in(&target, request)
        .await
        .unwrap();
    let board = alice.work_board_in(&target).await.unwrap();
    assert_eq!(board.card(review).unwrap().reviews, Some(parent));
    assert_eq!(
        board.card(parent).unwrap().state,
        airc_work::CardState::Merged
    );
    assert!(alice
        .work_board_in(&home)
        .await
        .unwrap()
        .snapshot()
        .cards
        .is_empty());
    assert_eq!(alice.current_room().await.unwrap().channel, home.channel);
}

#[tokio::test]
async fn a_board_read_for_a_named_room_returns_that_rooms_cards_not_the_current_rooms() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;

    // Two rooms, one distinct card each. Cards land in whatever room is
    // current at creation time, so create-then-switch.
    let other = alice
        .join("other-room")
        .await
        .expect("alice joins other-room");
    create_card(&alice, "OTHER-ROOM CARD").await;
    let home = alice
        .join("home-room")
        .await
        .expect("alice joins home-room");
    create_card(&alice, "HOME-ROOM CARD").await;

    // Precondition: the default read answers for home-room. If this ever
    // fails the rest of the test proves nothing, so assert it explicitly.
    assert_eq!(
        titles(&alice.work_board().await.expect("default board")),
        vec!["HOME-ROOM CARD".to_string()],
        "test precondition: the unscoped board reads the current (home) room"
    );

    // By name.
    let resolved = alice
        .room_by_name_or_channel(&other.name, "read the work board of")
        .await
        .expect("resolve other-room by name");
    assert_eq!(resolved.channel, other.channel);
    assert_eq!(
        titles(&alice.work_board_in(&resolved).await.expect("scoped board")),
        vec!["OTHER-ROOM CARD".to_string()],
        "BUG: a board read scoped to other-room returned the CURRENT room's cards \
         — the silent-default failure #345 exists to make impossible"
    );

    // By channel id — an agent reading a receipt or a board row holds the
    // uuid and nothing else, and must not have to invent an id→name lookup.
    let by_id = alice
        .room_by_name_or_channel(&other.channel.to_string(), "read the work board of")
        .await
        .expect("resolve other-room by channel id");
    assert_eq!(by_id.channel, other.channel, "id resolution finds the room");

    // Reading another room's board must not move the pointer — same
    // invariant `publish_to_room_does_not_mutate_default` pins for sends.
    let current = alice.current_room().await.expect("read current room after");
    assert_eq!(
        current.channel, home.channel,
        "BUG: a scoped board read mutated the default-room pointer"
    );
}

#[tokio::test]
async fn resolving_an_unsubscribed_room_refuses_loudly_instead_of_falling_back() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;
    alice
        .join("home-room")
        .await
        .expect("alice joins home-room");

    // What this catches: a "helpful" fallback to the current room. A board for
    // a room you are not in must be an error the caller can see, because the
    // alternative — a valid-looking board for a different room — is exactly the
    // answer that fooled a careful reader.
    let error = alice
        .room_by_name_or_channel("a-room-alice-never-joined", "read the work board of")
        .await
        .expect_err("unsubscribed room must refuse, never fall back");
    let rendered = error.to_string();
    assert!(
        rendered.contains("a-room-alice-never-joined"),
        "refusal must name the room the caller asked for, got: {rendered}"
    );
    assert!(
        rendered.contains("join"),
        "refusal must name the remedy, got: {rendered}"
    );
}

#[tokio::test]
async fn a_state_change_for_a_named_room_lands_on_that_rooms_card() {
    // What this catches: the WRITE half of the #345 class. Reads grew
    // `work_board_in` while every mutate stayed pinned to `current_room()`,
    // so a multi-room supervisor (continuum's bench-grade sweeper, first live
    // tick 2026-08-15) had 21 auto-closes refused in one pass — every target
    // card lived in a room its author wasn't standing in. The guard must keep
    // its strength (card must be ON the named room's board) while the room
    // becomes a parameter instead of ambient state.
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;

    let other = alice
        .join("other-room")
        .await
        .expect("alice joins other-room");
    create_card(&alice, "OTHER-ROOM CARD").await;
    let other_card = alice
        .work_board()
        .await
        .expect("other board")
        .snapshot()
        .cards[0]
        .card_id;

    let home = alice
        .join("home-room")
        .await
        .expect("alice joins home-room");
    create_card(&alice, "HOME-ROOM CARD").await;

    // Current-room mutate of an out-of-room card still refuses — the guard
    // keeps its strength.
    alice
        .change_work_card_state(airc_lib::ChangeWorkCardState {
            card_id: other_card,
            state: airc_lib::CardState::Closed,
        })
        .await
        .expect_err("mutating a card outside the current room must refuse");

    // Room-scoped mutate closes the card WHERE IT LIVES, without moving the
    // current-room pointer.
    alice
        .change_work_card_state_in(
            &other,
            airc_lib::ChangeWorkCardState {
                card_id: other_card,
                state: airc_lib::CardState::Closed,
            },
        )
        .await
        .expect("room-scoped state change");
    let closed = alice
        .work_board_in(&other)
        .await
        .expect("re-read other board")
        .snapshot()
        .cards
        .into_iter()
        .find(|c| c.card_id == other_card)
        .expect("card still on the board");
    assert_eq!(
        closed.state,
        airc_lib::CardState::Closed,
        "BUG: the room-scoped state change did not land on the named room's card"
    );

    // And the guard is room-parameterized, not gone: a card NOT in the named
    // room refuses with the same error class.
    let home_card = alice
        .work_board()
        .await
        .expect("home board")
        .snapshot()
        .cards[0]
        .card_id;
    alice
        .change_work_card_state_in(
            &other,
            airc_lib::ChangeWorkCardState {
                card_id: home_card,
                state: airc_lib::CardState::Closed,
            },
        )
        .await
        .expect_err("a card not on the NAMED room's board must refuse");
    let current = alice.current_room().await.expect("current room");
    assert_eq!(
        current.channel, home.channel,
        "BUG: a room-scoped mutate moved the default-room pointer"
    );
}

// What this catches (continuum 2026-09-21, Kimi's lapsed lease): a claim's
// heartbeat and release were pinned to `current_room()`, so a citizen renewing
// a card she holds in a bench room while standing in the project room was
// refused "not in current room" every minute until the lease lapsed and the card
// was re-granted to another peer with her patch finished. The room-scoped
// heartbeat lands the renewal on the board the card lives on and moves the
// expiry; the current-room wrapper still refuses (the guard keeps its strength);
// the room-scoped release clears the claim there; the current-room pointer
// never moves.
#[tokio::test]
async fn a_claim_is_renewed_and_released_where_the_card_lives_not_where_she_stands() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;

    let bench = alice
        .join("bench-room")
        .await
        .expect("alice joins bench-room");
    create_card(&alice, "BENCH CARD").await;
    let card_id = alice
        .work_board()
        .await
        .expect("bench board")
        .snapshot()
        .cards[0]
        .card_id;
    let claim_id = alice
        .claim_work_card(airc_lib::ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .expect("claim in the bench room");
    let claimed_expiry = alice
        .work_board_in(&bench)
        .await
        .expect("bench board")
        .snapshot()
        .cards
        .into_iter()
        .find(|c| c.card_id == card_id)
        .expect("the claimed card")
        .claim_expires_at_ms
        .expect("a live lease");

    // She stands in the project room now.
    let project = alice
        .join("project-room")
        .await
        .expect("alice joins project-room");

    // The current-room heartbeat refuses — the exact refusal Kimi got every minute.
    alice
        .heartbeat_work_claim(airc_lib::HeartbeatWorkClaim {
            card_id,
            claim_id,
            ttl_ms: 600_000,
        })
        .await
        .expect_err("a heartbeat against the current room must refuse for a card elsewhere");

    // The room-scoped heartbeat lands where the card lives and moves the expiry.
    alice
        .heartbeat_work_claim_in(
            &bench,
            airc_lib::HeartbeatWorkClaim {
                card_id,
                claim_id,
                ttl_ms: 600_000,
            },
        )
        .await
        .expect("room-scoped heartbeat");
    let renewed = alice
        .work_board_in(&bench)
        .await
        .expect("bench board after heartbeat")
        .snapshot()
        .cards
        .into_iter()
        .find(|c| c.card_id == card_id)
        .expect("the card is still hers");
    assert_eq!(
        renewed.owner,
        Some(alice.peer_id()),
        "the lease is still hers"
    );
    assert!(
        renewed.claim_expires_at_ms.expect("a live lease") > claimed_expiry,
        "BUG: the room-scoped heartbeat did not extend the lease on the named room's board"
    );

    // The guard is room-parameterized, not gone: the wrong room still refuses.
    alice
        .heartbeat_work_claim_in(
            &project,
            airc_lib::HeartbeatWorkClaim {
                card_id,
                claim_id,
                ttl_ms: 600_000,
            },
        )
        .await
        .expect_err("a card not on the NAMED room's board must refuse");

    // The room-scoped release clears the claim where the card lives.
    alice
        .release_work_claim_in(
            &bench,
            airc_lib::ReleaseWorkClaim {
                card_id,
                claim_id,
                reason: Some("done".into()),
            },
        )
        .await
        .expect("room-scoped release");
    let released = alice
        .work_board_in(&bench)
        .await
        .expect("bench board after release")
        .snapshot()
        .cards
        .into_iter()
        .find(|c| c.card_id == card_id)
        .expect("the card stays on the board");
    assert_eq!(
        released.claim_id, None,
        "BUG: the room-scoped release did not clear the claim"
    );

    let current = alice.current_room().await.expect("current room");
    assert_eq!(
        current.channel, project.channel,
        "BUG: a room-scoped lifecycle call moved the default-room pointer"
    );
}
