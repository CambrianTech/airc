//! Whether an event is addressed to this reader: the one predicate behind "an
//! authorized direct request is never filtered out or reduced to a count".
//!
//! An event is addressed to me when its typed target is my peer, or when its body
//! opens with `@names` and one of them is mine: my display name or agent name
//! (case-insensitive), or a shown-id prefix of my peer id. Today's requests are
//! mostly `@name` text broadcast to the room, so the typed target alone missed
//! them. Only LEADING names address; a name mid-sentence is a mention, not a
//! request. Receiver-side: every fact is local, no roster is read.

use airc_core::shown_id::Shown;
use airc_core::{MentionTarget, PeerId, TranscriptEvent};

use crate::{Airc, AircError};

/// Who "me" is for addressing.
#[derive(Clone, Debug)]
pub struct Addressee {
    peer: PeerId,
    names: Vec<String>,
}

impl Addressee {
    /// `names` are the names this identity answers to; blanks are dropped.
    pub fn new(peer: PeerId, names: impl IntoIterator<Item = String>) -> Self {
        let mut names: Vec<String> = names
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        names.dedup();
        Self { peer, names }
    }

    pub fn is_addressed(&self, event: &TranscriptEvent) -> bool {
        event.target == MentionTarget::Peer(self.peer)
            || event
                .body
                .as_ref()
                .and_then(|body| body.as_text())
                .is_some_and(|text| leading_mentions(text).any(|name| self.answers_to(name)))
    }

    fn answers_to(&self, mention: &str) -> bool {
        self.names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(mention))
            || Shown::<PeerId>::try_from(mention)
                .is_ok_and(|shown| shown.resolve([self.peer]).is_ok())
    }
}

/// The `@names` a body opens with, without the `@`: `"@Fable @Codex, review"`
/// yields `Fable`, `Codex`. Names are separated by whitespace or commas; the
/// first word that is not an `@name` ends the list. A name is alphanumerics,
/// `-` and `_`.
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

impl Airc {
    /// This identity as an addressee: its peer id, its agent name and the
    /// name on its local identity card.
    pub async fn addressee(&self) -> Result<Addressee, AircError> {
        let card = self
            .event_store()
            .load_local_identity_by_agent_name(self.agent_name())
            .await
            .map_err(AircError::from)?;
        let mut names = vec![self.agent_name().to_string()];
        names.extend(card.map(|stored| stored.identity.name));
        Ok(Addressee::new(self.peer_id(), names))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{Body, ClientId, EventId, Headers, RoomId, TranscriptKind};

    fn text(body: &str, target: MentionTarget) -> TranscriptEvent {
        TranscriptEvent {
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

    // what this catches: @name requests (target All) are addressed to every leading
    // name, case-insensitively, by name or shown peer id; a mid-sentence name is not.
    #[test]
    fn leading_names_address_and_mid_sentence_names_do_not() {
        let fable = Addressee::new(PeerId::from_u128(0x7711fe60 << 96), ["Fable".into()]);
        let codex = Addressee::new(PeerId::new(), ["Codex".into()]);
        let both = text("@Fable @Codex, review #1617", MentionTarget::All);
        assert!(fable.is_addressed(&both) && codex.is_addressed(&both));
        assert!(fable.is_addressed(&text("@fable: ping", MentionTarget::All)));
        assert!(fable.is_addressed(&text("@7711fe60 ping", MentionTarget::All)));
        assert!(!fable.is_addressed(&text("I asked Fable and @Fable later", MentionTarget::All)));
        assert!(!fable.is_addressed(&text("@Fabled ping", MentionTarget::All)));
        assert!(!fable.is_addressed(&text("@77 too short to be an id", MentionTarget::All)));
        let typed = text(
            "no names",
            MentionTarget::Peer(PeerId::from_u128(0x7711fe60 << 96)),
        );
        assert!(fable.is_addressed(&typed));
        let names: Vec<_> = leading_mentions("@M5,ack").collect();
        assert_eq!(names, ["M5"]);
        let names: Vec<_> = leading_mentions("@a @b: @c").collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(leading_mentions("@ stray").count(), 0);
    }
}
