//! The facts that say who an event is addressed to, owned once below every
//! store: the typed target peer, and whether the body opens with `@names`.
//!
//! The exact "addressed to me" decision needs the reader's names and lives in
//! the SDK (`airc_lib::Addressee`). The stores only need a candidate test they
//! can answer from indexed columns, so a scan over a range returns the events
//! that could be addressed to a peer however much ambient traffic the range
//! holds. [`Addressing`] is that test's input, computed when an event is
//! stored; [`AddresseeFilter`] is the question a scan asks of it.

use serde::{Deserialize, Serialize};

use crate::{Body, MentionTarget, PeerId, TranscriptEvent};

/// The `@names` a body opens with, without the `@`: `"@Fable @Codex, review"`
/// yields `Fable`, `Codex`. Names are separated by whitespace or commas; the
/// first word that is not an `@name` ends the list. A name is alphanumerics,
/// `-` and `_`. Only LEADING names address; a name mid-sentence is a mention,
/// not a request.
pub fn leading_mentions(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let tail = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        let after = tail.strip_prefix('@')?;
        let end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(after.len());
        if end == 0 {
            return None;
        }
        let (name, remainder) = after.split_at(end);
        // A name followed by anything but a separator closes the list after it.
        rest = if remainder.starts_with(|c: char| c.is_whitespace() || c == ',') {
            remainder
        } else {
            ""
        };
        Some(name)
    })
}

/// Who one stored event could be addressed to, as the stores index it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Addressing {
    /// The peer a typed target names, if it names one.
    pub peer: Option<PeerId>,
    /// The body's text opens with at least one `@name` ([`leading_mentions`]).
    pub leading_mention: bool,
}

impl Addressing {
    /// From a typed target peer and an optional body.
    pub fn of(peer: Option<PeerId>, body: Option<&Body>) -> Self {
        Self {
            peer,
            leading_mention: body
                .and_then(Body::as_text)
                .is_some_and(|text| leading_mentions(text).next().is_some()),
        }
    }

    /// From a transcript target and body (a stored `events` row's parts).
    pub fn of_target(target: &MentionTarget, body: Option<&Body>) -> Self {
        let peer = match target {
            MentionTarget::Peer(peer) => Some(*peer),
            MentionTarget::All | MentionTarget::Room(_) => None,
        };
        Self::of(peer, body)
    }

    pub fn of_event(event: &TranscriptEvent) -> Self {
        Self::of_target(&event.target, event.body.as_ref())
    }

    /// Whether `filter` keeps this event as a candidate.
    pub fn admitted_by(&self, filter: &AddresseeFilter) -> bool {
        self.peer == Some(filter.peer) || self.leading_mention
    }
}

/// A scan's addressee pre-filter: events whose typed target is `peer`, or
/// whose body opens with an `@name`. A superset of what is addressed to
/// `peer` by any of its names; the reader applies its exact predicate after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddresseeFilter {
    pub peer: PeerId,
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the candidate test must keep every event the exact
    // predicate could keep (typed target, or a leading @name), and drop a body
    // whose @ is mid-sentence or not followed by a name.
    #[test]
    fn candidates_are_typed_targets_and_leading_names() {
        let me = PeerId::new();
        let filter = AddresseeFilter { peer: me };
        let text = |s: &str| Addressing::of(None, Some(&Body::text(s)));
        assert!(Addressing::of(Some(me), None).admitted_by(&filter));
        assert!(!Addressing::of(Some(PeerId::new()), None).admitted_by(&filter));
        assert!(text(" ,@Fable ping").admitted_by(&filter));
        assert!(!text("ask @Fable").admitted_by(&filter));
        assert!(!text("@ stray").admitted_by(&filter));
        let names: Vec<_> = leading_mentions("@a @b: @c").collect();
        assert_eq!(names, ["a", "b"]);
        let names: Vec<_> = leading_mentions("@M5,ack").collect();
        assert_eq!(names, ["M5"]);
    }
}
