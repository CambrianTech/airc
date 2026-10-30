//! Direct addressing is exclusively the signed typed target.
use crate::{Airc, AircError};
use airc_core::{AddresseeFilter, MentionTarget, PeerId, TranscriptEvent};

#[derive(Clone, Debug)]
pub struct Addressee {
    peer: PeerId,
}
impl Addressee {
    pub fn new(peer: PeerId) -> Self {
        Self { peer }
    }
    pub fn filter(&self) -> AddresseeFilter {
        AddresseeFilter { peer: self.peer }
    }
    pub fn is_addressed(&self, event: &TranscriptEvent) -> bool {
        event.target == MentionTarget::Peer(self.peer)
    }
}
impl Airc {
    pub async fn addressee(&self) -> Result<Addressee, AircError> {
        Ok(Addressee::new(self.peer_id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{Body, ClientId, EventId, Headers, RoomId, TranscriptKind};

    fn text(body: &str, target: MentionTarget) -> TranscriptEvent {
        TranscriptEvent {
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn only_typed_target_addresses_a_reader() {
        let peer = PeerId::new();
        let reader = Addressee::new(peer);
        for body in ["@Fable @Codex, review", "@7711fe60 ping", "ordinary text"] {
            assert!(!reader.is_addressed(&text(body, MentionTarget::All)));
            assert!(reader.is_addressed(&text(body, MentionTarget::Peer(peer))));
            assert!(!reader.is_addressed(&text(body, MentionTarget::Peer(PeerId::new()))));
        }
    }
}
