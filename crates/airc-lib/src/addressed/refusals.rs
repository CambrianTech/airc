//! Refused addressed admissions, counted by cause and said in powers of two (card 92cd55d2).
//!
//! The IntelMac, 2026-11-02: a host whose recipient had no live owner refused every frame for it
//! and said so at ERROR once per frame, about 240 lines a minute, until its log reached 740 MB.
//! A refusal is now counted by its typed cause and said at its 1st, 2nd, 4th, 8th... occurrence
//! with the count and the latest frame's claims: the same evidence, at a log-scale of lines. The
//! key is our own typed cause, never a claimed sender or target (those are unverified claims, and
//! keyed by them the counts would grow without bound for anyone who sends), and never the error's
//! text: a signature refusal after a NoReceiver flood is its own cause and is said at once (Fable
//! on #1651). A delivery that succeeds ends the episode: every count starts again, so the first
//! refusal of the next outage is said.

use std::collections::HashMap;

use super::AddressedError;

/// Why the host refused to admit an addressed frame: the daemon's admission error carried across the
/// sink seam as a finite type, without its detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdmissionRefusal {
    /// The daemon that owns admission has closed.
    OwnerClosed,
    WrongKindOrChannel,
    WrongTarget,
    SenderMustSign,
    Trust,
    Signature,
    Queue(QueueRefusal),
    Forward(ForwardRefusal),
    Expiry,
}

/// Why the recipient's queue refused a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueRefusal {
    ReceiverLimit,
    ReceiverBusy,
    NoReceiver,
    Backpressure,
    Closed,
    StatePoisoned,
    DeadlineElapsed,
}

/// Why forwarding a frame to the recipient's host failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForwardRefusal {
    NotInstalled,
    AlreadyInstalled,
    UnknownRecipient,
    UnknownHost,
    LocalOwnerAbsent,
    InvalidEndpoints,
    Trust,
    Transport,
    Admission(QueueRefusal),
}

/// What a refusal is counted under: a finite key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RefusalKind {
    Transport,
    Handshake,
    Codec,
    Deadline,
    Admission(AdmissionRefusal),
}

impl RefusalKind {
    fn of(error: &AddressedError) -> Self {
        match error {
            AddressedError::Transport(_) => Self::Transport,
            AddressedError::Handshake(_) => Self::Handshake,
            AddressedError::Codec(_) => Self::Codec,
            AddressedError::DeadlineExceeded => Self::Deadline,
            AddressedError::Admission { cause, .. } => Self::Admission(*cause),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Refusals {
    by_kind: HashMap<RefusalKind, u64>,
}

/// One refusal that is said: its kind, and how many of that kind there have been this episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Said {
    pub(crate) kind: RefusalKind,
    pub(crate) count: u64,
}

impl Refusals {
    /// Count one refusal; `Some` when this one is said (its kind's count is a power of two).
    pub(crate) fn count(&mut self, error: &AddressedError) -> Option<Said> {
        let kind = RefusalKind::of(error);
        let count = self.by_kind.entry(kind).or_default();
        *count += 1;
        count.is_power_of_two().then_some(Said {
            kind,
            count: *count,
        })
    }

    /// A delivery succeeded: the episode ends, and every cause's next refusal is said again.
    pub(crate) fn delivered(&mut self) {
        self.by_kind.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(cause: AdmissionRefusal) -> AddressedError {
        AddressedError::Admission {
            cause,
            detail: format!("addressed admission refused: {cause:?}"),
        }
    }

    // what this catches: a refusal flood said once per frame (the 740 MB log), counted under keys a
    // sender chooses, or under one key for every cause (Fable on #1651: a signature refusal after a
    // NoReceiver flood was counted as "transport" #301 and never said). A thousand refusals of one
    // cause are said ten times; a different cause is said at once; after a delivery succeeds, the
    // same cause is said again from 1.
    #[test]
    fn each_cause_is_said_at_powers_of_two_and_again_after_a_success() {
        let mut refusals = Refusals::default();
        let no_receiver = refused(AdmissionRefusal::Queue(QueueRefusal::NoReceiver));
        let said: Vec<u64> = (0..1000)
            .filter_map(|_| refusals.count(&no_receiver))
            .map(|s| s.count)
            .collect();
        assert_eq!(said, vec![1, 2, 4, 8, 16, 32, 64, 128, 256, 512]);
        assert_eq!(
            refusals.count(&refused(AdmissionRefusal::Signature)),
            Some(Said {
                kind: RefusalKind::Admission(AdmissionRefusal::Signature),
                count: 1
            }),
            "a security-relevant cause after a flood is said at once"
        );
        assert_eq!(
            refusals
                .count(&AddressedError::DeadlineExceeded)
                .map(|s| s.count),
            Some(1)
        );
        refusals.delivered();
        assert_eq!(
            refusals.count(&no_receiver).map(|s| s.count),
            Some(1),
            "a new outage is said from its first refusal"
        );
    }
}
