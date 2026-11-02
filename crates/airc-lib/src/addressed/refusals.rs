//! Refused addressed admissions, counted and said in powers of two (card 92cd55d2).
//!
//! The IntelMac, 2026-11-02: a host whose recipient had no live owner refused every frame for it
//! and said so at ERROR once per frame, about 240 lines a minute, until its log reached 740 MB.
//! A refusal is now counted by kind and said at its 1st, 2nd, 4th, 8th... occurrence with the
//! count and the latest frame's claims: the same evidence, at a log-scale of lines, and only when
//! refusals happen (nothing runs between them). The key is our own refusal kind, never a claimed
//! sender or target: those are unverified claims, and keyed by them the counts would grow without
//! bound for anyone who sends.

use std::collections::HashMap;

use super::AddressedError;

/// The kind of a refusal: a finite key.
fn kind_of(error: &AddressedError) -> &'static str {
    match error {
        AddressedError::Transport(_) => "transport",
        AddressedError::Handshake(_) => "handshake",
        AddressedError::Codec(_) => "codec",
        AddressedError::DeadlineExceeded => "deadline",
    }
}

#[derive(Debug, Default)]
pub(crate) struct Refusals {
    by_kind: HashMap<&'static str, u64>,
}

/// One refusal that is said: its kind, and how many of that kind there have been.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Said {
    pub(crate) kind: &'static str,
    pub(crate) count: u64,
}

impl Refusals {
    /// Count one refusal; `Some` when this one is said (its kind's count is a power of two).
    pub(crate) fn count(&mut self, error: &AddressedError) -> Option<Said> {
        let kind = kind_of(error);
        let count = self.by_kind.entry(kind).or_default();
        *count += 1;
        count.is_power_of_two().then_some(Said {
            kind,
            count: *count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a refusal flood said once per frame (the 740 MB log), or counted under
    // keys a sender chooses. A thousand refusals of one kind are said ten times, each with its
    // running count; another kind keeps its own count.
    #[test]
    fn a_flood_of_one_kind_is_said_at_powers_of_two() {
        let mut refusals = Refusals::default();
        let no_receiver =
            AddressedError::Transport("addressed admission refused: Queue(NoReceiver)".into());
        let said: Vec<u64> = (0..1000)
            .filter_map(|_| refusals.count(&no_receiver))
            .map(|s| s.count)
            .collect();
        assert_eq!(said, vec![1, 2, 4, 8, 16, 32, 64, 128, 256, 512]);
        assert_eq!(
            refusals.count(&AddressedError::DeadlineExceeded),
            Some(Said {
                kind: "deadline",
                count: 1
            })
        );
    }
}
