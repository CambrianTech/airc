//! Peer-to-peer task negotiation header conventions.
//!
//! Card 94b5f668. Joel directive: idle agents must ASK EACH OTHER
//! for work, not wait on human. The substrate doesn't ship a typed
//! "TaskRequest" event — that would mix consumer-domain (what "task"
//! MEANS to hermes vs continuum vs openclaw) with the substrate's
//! bus role. Instead: chat messages with these header constants
//! carry the negotiation. Consumers subscribe with a header filter
//! and decide based on THEIR intelligence what to offer / accept.
//!
//! ## Convention
//!
//! An idle agent publishes a chat message with
//! [`HEADER_AIRC_TASK_REQUEST`] set to a free-form criteria string
//! (consumer-defined: `"P0"`, `"hermes:planning"`, `"any"`,
//! `"continuum:tensor"`, whatever the asker can specify).
//!
//! Any subscriber in the room sees it, decides whether to respond.
//! A response is a chat message with [`HEADER_AIRC_TASK_OFFER`]
//! pointing at a card_id, body explaining the offer.
//!
//! The asker chooses which offer to accept by claiming the offered
//! card (existing `airc work claim` flow). No new typed events; the
//! substrate just routes the typed envelopes; intelligence is in
//! the consumer.
//!
//! ## Why headers, not a new event variant
//!
//! Same reason capability advertising stayed at the header level
//! (card 1e624cff): the substrate doesn't grow domain concepts.
//! Consumers (hermes, continuum, openclaw, future personas) each
//! interpret "task" differently. Headers are middleware; bus carries
//! the envelope; consumers act.

/// Set on a chat message to advertise "I am idle, looking for work."
/// Value is a consumer-defined criteria string. Subscribers filter
/// on this header to spot requests without parsing every chat msg
/// body.
///
/// Example:
///   `airc msg --header airc.task.request=P0 "@anyone got a P0 I should look at?"`
pub const HEADER_AIRC_TASK_REQUEST: &str = "airc.task.request";

/// Set on a chat message in response to a request. Value is the
/// card_id (UUID) being offered. Body explains the offer (priority,
/// title, why this is a fit, etc.). The requester claims the card
/// via the existing `airc work claim` flow if they accept.
///
/// Example:
///   `airc msg --header airc.task.offer=<card-uuid> "P0 abc123 — gh client tests, well-bounded"`
pub const HEADER_AIRC_TASK_OFFER: &str = "airc.task.offer";

/// Set on a chat message to acknowledge "I've claimed your offer."
/// Value is the card_id (UUID) the claimant took. Lets the offering
/// peer prune their offer queue + lets observers in the room see
/// the negotiation close. Optional; the substrate-side claim event
/// is authoritative, this is a courtesy ping.
pub const HEADER_AIRC_TASK_ACCEPTED: &str = "airc.task.accepted";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_constants_use_stable_dotted_namespace() {
        // Consumers filter on prefix; pin the namespace shape so a
        // typo can't quietly drift consumers that grep on
        // `airc.task.`.
        for h in [
            HEADER_AIRC_TASK_REQUEST,
            HEADER_AIRC_TASK_OFFER,
            HEADER_AIRC_TASK_ACCEPTED,
        ] {
            assert!(
                h.starts_with("airc.task."),
                "header must be namespaced: {h}"
            );
            assert!(!h.contains(' '), "headers must not contain whitespace: {h}");
        }
    }

    #[test]
    fn header_constants_are_distinct() {
        // Defensive: catching a copy-paste error where two constants
        // ended up with the same value (silently makes requests
        // indistinguishable from offers on the wire).
        assert_ne!(HEADER_AIRC_TASK_REQUEST, HEADER_AIRC_TASK_OFFER);
        assert_ne!(HEADER_AIRC_TASK_REQUEST, HEADER_AIRC_TASK_ACCEPTED);
        assert_ne!(HEADER_AIRC_TASK_OFFER, HEADER_AIRC_TASK_ACCEPTED);
    }
}
