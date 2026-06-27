//! `Airc::room_doctrine` is latest-write-wins, not first-match.
//!
//! what this catches: the read used to `return Ok(Some(card))` on the
//! FIRST `DoctrinePublished` event in `page_recent`. `page_recent` is
//! not guaranteed newest-first (the channel_purpose LWW test proves
//! it isn't), so after a doctrine is REPUBLISHED, first-match could
//! surface the STALE prior doctrine — and continuum's RoomDoctrineSource
//! would inject yesterday's rules into a persona's prompt. This pins
//! that the projection returns the doctrine with the highest
//! `published_at_ms`.

use airc_lib::Airc;
use tempfile::TempDir;

#[tokio::test]
async fn room_doctrine_returns_latest_not_first_match() {
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join(".airc")).await.expect("open");
    airc.join("general").await.expect("join general");

    // Unset → None.
    assert!(
        airc.room_doctrine().await.expect("read unset").is_none(),
        "a room with no published doctrine reads back None",
    );

    // Publish v1, then v2 (a later republish — the operator updated the
    // rules). LWW must return v2.
    airc.publish_room_doctrine(
        "# rules v1\nbe terse".to_string(),
        "v1aaaaaaaaaa".to_string(),
    )
    .await
    .expect("publish v1");
    airc.publish_room_doctrine(
        "# rules v2\nbe terse AND cite sources".to_string(),
        "v2bbbbbbbbbb".to_string(),
    )
    .await
    .expect("publish v2");

    let current = airc
        .room_doctrine()
        .await
        .expect("read doctrine")
        .expect("a doctrine is published");
    assert_eq!(
        current.version, "v2bbbbbbbbbb",
        "room_doctrine must return the LATEST republished doctrine (LWW), \
         not the first match in page_recent: got {current:?}",
    );
    assert!(
        current.body.contains("cite sources"),
        "the latest body must be returned, not the stale v1: {current:?}",
    );
}
