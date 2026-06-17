//! `Airc::publish_channel_purpose` / `Airc::channel_purpose` — the
//! typed room-nature the persona-groundedness consumer (continuum's
//! `RoomPurposeSource`) reads to drive a DETERMINISTIC participation
//! gate (M5 groundedness, 2026-06-16).
//!
//! Contract proven here:
//!   - an unset room reads back `None` (honest "no typed purpose");
//!   - a published typed kind round-trips back through the substrate
//!     verbatim (the consumer matches on the same enum the publisher
//!     sent — no prose, no inference);
//!   - latest-write-wins: republishing a different kind supersedes the
//!     prior one (the projection returns the newest).
//!
//! Purpose is published via `emit_lifecycle` (a local-store append, same
//! path as `publish_room_doctrine` + identity cards), so a single
//! in-process `Airc` proves the publish→projection round-trip
//! synchronously — no daemon route needed (and a daemon-backed
//! `page_recent` wouldn't see a local-only lifecycle append anyway).

use airc_lib::{Airc, ChannelPurpose};
use tempfile::TempDir;

#[tokio::test]
async fn channel_purpose_publishes_reads_back_and_is_lww() {
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join(".airc")).await.expect("open");
    airc.join("general").await.expect("join general");

    // Unset → None (no typed purpose published in the window).
    assert_eq!(
        airc.channel_purpose().await.expect("read unset"),
        None,
        "a room with no published purpose reads back None",
    );

    // Publish a typed kind; the projection returns it verbatim.
    airc.publish_channel_purpose(ChannelPurpose::Coordination)
        .await
        .expect("publish coordination");
    assert_eq!(
        airc.channel_purpose().await.expect("read coordination"),
        Some(ChannelPurpose::Coordination),
        "the typed purpose reads back verbatim — same enum the publisher sent",
    );

    // Latest-write-wins: a newer publish of a different kind supersedes.
    airc.publish_channel_purpose(ChannelPurpose::Game)
        .await
        .expect("republish game");
    assert_eq!(
        airc.channel_purpose().await.expect("read game"),
        Some(ChannelPurpose::Game),
        "LWW: the newest published purpose wins over the prior one",
    );

    // The `Other` escape hatch round-trips its consumer-defined string.
    airc.publish_channel_purpose(ChannelPurpose::Other("broadcast".to_string()))
        .await
        .expect("publish other");
    assert_eq!(
        airc.channel_purpose().await.expect("read other"),
        Some(ChannelPurpose::Other("broadcast".to_string())),
        "Other carries its consumer-defined kind through the substrate",
    );
}
