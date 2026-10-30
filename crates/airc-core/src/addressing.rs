//! Addressing is authenticated envelope metadata. Payload text is never a route.
use crate::{MentionTarget, PeerId, TranscriptEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Addressing {
    pub peer: Option<PeerId>,
}
impl Addressing {
    pub fn of(peer: Option<PeerId>) -> Self {
        Self { peer }
    }
    pub fn of_target(target: &MentionTarget) -> Self {
        Self::of(match target {
            MentionTarget::Peer(peer) => Some(*peer),
            MentionTarget::All | MentionTarget::Room(_) => None,
        })
    }
    pub fn of_event(event: &TranscriptEvent) -> Self {
        Self::of_target(&event.target)
    }
    pub fn admitted_by(&self, filter: &AddresseeFilter) -> bool {
        self.peer == Some(filter.peer)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddresseeFilter {
    pub peer: PeerId,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_typed_target_can_address_a_peer() {
        let peer = PeerId::new();
        let filter = AddresseeFilter { peer };
        assert!(Addressing::of_target(&MentionTarget::Peer(peer)).admitted_by(&filter));
        assert!(!Addressing::of_target(&MentionTarget::All).admitted_by(&filter));
        assert!(!Addressing::of_target(&MentionTarget::Peer(PeerId::new())).admitted_by(&filter));
    }
}
