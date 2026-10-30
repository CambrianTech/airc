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
    pub const ALL: [Self; 26] = [
        Self::Correspondence(CorrespondenceKind::Message),
        Self::Correspondence(CorrespondenceKind::Announcement),
        Self::Activity(ActivityKind::Intent),
        Self::Activity(ActivityKind::WorkReceipt),
        Self::Activity(ActivityKind::WorkState),
        Self::Activity(ActivityKind::Presence),
        Self::Activity(ActivityKind::WorkPresence),
        Self::Activity(ActivityKind::Coordination),
        Self::Control(ControlKind::Command),
        Self::Control(ControlKind::CommandResult),
        Self::Control(ControlKind::Session),
        Self::Control(ControlKind::Receipt),
        Self::Control(ControlKind::Addressed),
        Self::Control(ControlKind::Signal),
        Self::Control(ControlKind::Stream),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::PeerArrived)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::PeerDeparted)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::WireEstablished)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::WireLost)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::RoomJoined)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::RoomParted)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::SubscriptionAdvanced)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::IdentityPublished)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::DoctrinePublished)),
        Self::Control(ControlKind::Lifecycle(LifecycleKind::WallPostPublished)),
        Self::Control(ControlKind::Lifecycle(
            LifecycleKind::ChannelPurposePublished,
        )),
    ];

    /// Canonical transcript category; admission independently refuses addressed traffic.
    pub const fn transcript_kind(self) -> crate::TranscriptKind {
        use crate::TranscriptKind;
        match self {
            Self::Correspondence(_) => TranscriptKind::Message,
            Self::Activity(_) => TranscriptKind::System,
            Self::Control(ControlKind::Command) => TranscriptKind::Command,
            Self::Control(ControlKind::CommandResult) => TranscriptKind::CommandResult,
            Self::Control(ControlKind::Lifecycle(kind)) => kind.transcript_kind(),
            Self::Control(_) => TranscriptKind::SessionControl,
        }
    }

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
    WorkPresence,
    Coordination,
    Presence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ControlKind {
    Command,
    CommandResult,
    Session,
    Receipt,
    Addressed,
    Signal,
    Stream,
    Lifecycle(LifecycleKind),
}

/// Substrate lifecycle semantics, carried in authenticated metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleKind {
    PeerArrived,
    PeerDeparted,
    WireEstablished,
    WireLost,
    RoomJoined,
    RoomParted,
    SubscriptionAdvanced,
    IdentityPublished,
    DoctrinePublished,
    WallPostPublished,
    ChannelPurposePublished,
}

impl LifecycleKind {
    pub const ALL: [Self; 11] = [
        Self::PeerArrived,
        Self::PeerDeparted,
        Self::WireEstablished,
        Self::WireLost,
        Self::RoomJoined,
        Self::RoomParted,
        Self::SubscriptionAdvanced,
        Self::IdentityPublished,
        Self::DoctrinePublished,
        Self::WallPostPublished,
        Self::ChannelPurposePublished,
    ];
    pub const fn transcript_kind(self) -> crate::TranscriptKind {
        match self {
            Self::PeerArrived => crate::TranscriptKind::PeerArrived,
            Self::PeerDeparted => crate::TranscriptKind::PeerDeparted,
            Self::WireEstablished => crate::TranscriptKind::WireEstablished,
            Self::WireLost => crate::TranscriptKind::WireLost,
            Self::RoomJoined => crate::TranscriptKind::RoomJoined,
            Self::RoomParted => crate::TranscriptKind::RoomParted,
            Self::SubscriptionAdvanced => crate::TranscriptKind::SubscriptionAdvanced,
            Self::IdentityPublished => crate::TranscriptKind::IdentityPublished,
            Self::DoctrinePublished => crate::TranscriptKind::DoctrinePublished,
            Self::WallPostPublished => crate::TranscriptKind::WallPostPublished,
            Self::ChannelPurposePublished => crate::TranscriptKind::ChannelPurposePublished,
        }
    }
}

/// A subscription can request a whole family or one exact semantic variant.
/// This predicate cannot inspect payloads, names, or display punctuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "select", content = "class", rename_all = "snake_case")]
pub enum EventSelection {
    Correspondence,
    Activity,
    Control,
    Exact(EventClass),
    All,
    /// Match any declared selection; an empty union admits nothing.
    AnyOf(Vec<EventSelection>),
}

impl EventSelection {
    pub const fn family(&self) -> Option<EventFamily> {
        match self {
            Self::Correspondence => Some(EventFamily::Correspondence),
            Self::Activity => Some(EventFamily::Activity),
            Self::Control => Some(EventFamily::Control),
            Self::Exact(class) => Some(class.family()),
            Self::All | Self::AnyOf(_) => None,
        }
    }
    pub fn matches(&self, class: EventClass) -> bool {
        match self {
            Self::Correspondence => matches!(class, EventClass::Correspondence(_)),
            Self::Activity => matches!(class, EventClass::Activity(_)),
            Self::Control => matches!(class, EventClass::Control(_)),
            Self::Exact(expected) => class == *expected,
            Self::All => true,
            Self::AnyOf(selections) => selections.iter().any(|selection| selection.matches(class)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_metadata_roundtrips_without_losing_subtype() {
        for lifecycle in LifecycleKind::ALL {
            let class = EventClass::Control(ControlKind::Lifecycle(lifecycle));
            assert_eq!(class.transcript_kind(), lifecycle.transcript_kind());
            let encoded = serde_json::to_string(&class).expect("class");
            assert_eq!(
                serde_json::from_str::<EventClass>(&encoded).expect("class decode"),
                class
            );
        }
        assert!(serde_json::from_str::<EventClass>(
            r#"{"family":"control","kind":{"kind":"lifecycle"}}"#
        )
        .is_err());
    }

    #[test]
    fn control_transcript_projection_is_shared_across_transports() {
        for control in [
            ControlKind::Session,
            ControlKind::Receipt,
            ControlKind::Signal,
            ControlKind::Stream,
        ] {
            assert_eq!(
                EventClass::Control(control).transcript_kind(),
                crate::TranscriptKind::SessionControl
            );
        }
        assert_eq!(
            EventClass::Control(ControlKind::Command).transcript_kind(),
            crate::TranscriptKind::Command
        );
    }

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
    fn unions_preserve_exact_selections_without_admitting_other_activity() {
        let selection = EventSelection::AnyOf(vec![
            EventSelection::Correspondence,
            EventSelection::Exact(EventClass::Activity(ActivityKind::WorkState)),
        ]);
        assert!(selection.matches(EventClass::Correspondence(CorrespondenceKind::Message)));
        assert!(selection.matches(EventClass::Activity(ActivityKind::WorkState)));
        assert!(!selection.matches(EventClass::Activity(ActivityKind::Presence)));
        assert!(!selection.matches(EventClass::Activity(ActivityKind::WorkReceipt)));
        assert!(!EventSelection::AnyOf(Vec::new())
            .matches(EventClass::Correspondence(CorrespondenceKind::Message)));
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
