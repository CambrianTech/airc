//! Signed event semantics, independent of payload and delivery policy.
//!
//! Producers choose a variant. Routers receive only this value when deciding
//! which subscription handles an event; neither text nor a JSON body is input.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "family", content = "kind", rename_all = "snake_case")]
pub enum EventClass {
    Correspondence(CorrespondenceKind),
    Activity(ActivityKind),
    Control(ControlKind),
}

/// Stable persistence discriminator; routing uses the enum, not its encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i32)]
pub enum EventFamily {
    Correspondence = 1,
    Activity = 2,
    Control = 3,
}

impl EventClass {
    pub const fn family(self) -> EventFamily {
        match self {
            Self::Correspondence(_) => EventFamily::Correspondence,
            Self::Activity(_) => EventFamily::Activity,
            Self::Control(_) => EventFamily::Control,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrespondenceKind {
    Message,
    Announcement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Intent,
    WorkReceipt,
    WorkState,
    Presence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Command,
    CommandResult,
    Session,
    Receipt,
    Addressed,
    Signal,
    Stream,
    Lifecycle,
}

/// A subscription can request a whole family or one exact semantic variant.
/// This predicate cannot inspect payloads, names, or display punctuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "select", content = "class", rename_all = "snake_case")]
pub enum EventSelection {
    Correspondence,
    Activity,
    Control,
    Exact(EventClass),
    All,
}

impl EventSelection {
    pub const fn family(self) -> Option<EventFamily> {
        match self {
            Self::Correspondence => Some(EventFamily::Correspondence),
            Self::Activity => Some(EventFamily::Activity),
            Self::Control => Some(EventFamily::Control),
            Self::Exact(class) => Some(class.family()),
            Self::All => None,
        }
    }
    pub fn matches(self, class: EventClass) -> bool {
        match self {
            Self::Correspondence => matches!(class, EventClass::Correspondence(_)),
            Self::Activity => matches!(class, EventClass::Activity(_)),
            Self::Control => matches!(class, EventClass::Control(_)),
            Self::Exact(expected) => class == expected,
            Self::All => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_accepts_only_the_declared_family_or_exact_variant() {
        let correspondence = EventClass::Correspondence(CorrespondenceKind::Message);
        let receipt = EventClass::Activity(ActivityKind::WorkReceipt);
        let intent = EventClass::Activity(ActivityKind::Intent);
        assert!(EventSelection::Correspondence.matches(correspondence));
        assert!(!EventSelection::Correspondence.matches(receipt));
        assert!(EventSelection::Activity.matches(receipt));
        assert!(!EventSelection::Exact(receipt).matches(intent));
    }

    #[test]
    fn malformed_or_unknown_classification_is_refused() {
        for input in [
            "null",
            "{}",
            r#"{"family":"activity","kind":"message"}"#,
            r#"{"family":"unknown","kind":"work_receipt"}"#,
        ] {
            assert!(
                serde_json::from_str::<EventClass>(input).is_err(),
                "{input}"
            );
        }
    }
}
