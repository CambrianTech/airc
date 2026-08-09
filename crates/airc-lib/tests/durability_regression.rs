//! DURABILITY: what a room was told, a room must still know after a restart.
//!
//! This file exists because of a measurement, not a theory. On the M5,
//! 2026-08-04, the live scope store held **504,013 events** and not one of
//! them was a message:
//!
//! ```text
//! subscription_advanced   503,656   (99.93%)
//! identity_published          232
//! room_joined                 117
//! system                        7
//! doctrine_published            1
//! ```
//!
//! A peer whose messages had been arriving all evening had exactly ONE row in
//! the entire store, six weeks old. Traffic was being delivered live and never
//! written down. Every "did she actually say that?" question in this project
//! ends in guesswork for that reason, and persona wake-hydration, repetition
//! detection, and recall each carry their own scaffolding to work around a room
//! with no history.
//!
//! ## Why the existing tests did not catch it
//!
//! They test the wrong axis. Delivery has tests and delivery works. Durability
//! is a property ACROSS TIME — write, drop everything, come back, still there —
//! and nothing asserted it. That is the shape of every defect found that night:
//! green components, untested behavior over time.
//!
//! Keep this file about that one property. It is deliberately hermetic (a
//! tempdir scope, the in-process path, no daemon, no network) so a failure here
//! means the substrate forgot something, never that a socket was busy.

use airc_lib::Airc;
use tempfile::TempDir;

/// what this catches: the 503,656-cursor-rows-and-zero-messages store. A room
/// is told something; the process that heard it goes away; a new handle opens
/// the same scope. If the message is gone, the room has no memory, and every
/// consumer that reads history — persona wake, recall, the transcript UI — is
/// reading a void it cannot distinguish from silence.
///
/// The reopen is the entire point. Asserting right after the write only proves
/// the in-memory path, which is exactly the assumption that let this ship.
#[tokio::test]
async fn a_message_survives_the_process_that_heard_it() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().join(".airc");

    let said = {
        let airc = Airc::open(&home).await.expect("open");
        airc.join("general").await.expect("join general");
        airc.say("the room must remember this line")
            .await
            .expect("say");
        // Prove the live path first, so a failure below is unambiguously about
        // DURABILITY and not about the message never existing at all.
        let live = airc.page_recent(32).await.expect("page live");
        assert!(
            live.iter().any(|event| event
                .body
                .as_ref()
                .and_then(airc_core::Body::as_text)
                .is_some_and(|text| text.contains("the room must remember this line"))),
            "the message was not even visible to the handle that wrote it — \
             this is a delivery failure, not a durability one"
        );
        "the room must remember this line"
    };

    // Everything from the first session is dropped here: handle, store, caches.
    // A fresh handle on the same scope is the honest stand-in for a restart.
    let reopened = Airc::open(&home).await.expect("reopen same scope");
    let history = reopened.page_recent(32).await.expect("page after reopen");

    assert!(
        history.iter().any(|event| event
            .body
            .as_ref()
            .and_then(airc_core::Body::as_text)
            .is_some_and(|text| text.contains(said))),
        "the room FORGOT what it was told: {} event(s) survived the reopen and \
         none is the message. Live delivery worked and the write did not persist \
         — the exact shape measured on the M5 (503,656 cursor rows, zero \
         messages). Everything that reads room history is reading a void.",
        history.len()
    );
}
