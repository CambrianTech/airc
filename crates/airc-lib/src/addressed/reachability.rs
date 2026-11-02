//! Which recipients are backing off, and the session generation that fences a stale failure: one
//! lock, so checking and recording can never interleave with a session install (Codex on #1650).
//!
//! A recipient that formed no session when last asked backs off on the route's own bounded policy
//! ([`crate::route::Quarantine`], backoff-only: an unanswered handshake is uncertainty and is never
//! Dead). A probe takes the recipient's session generation when it starts; a session installed with
//! the recipient bumps it and clears the backoff; a failure is recorded only if the generation is
//! still the one its probe started under. So a probe that started before a session cannot back the
//! recipient off after it.

use std::collections::HashMap;

use airc_core::PeerId;

use crate::route::{DialGate, Quarantine};

/// A recipient's session generation, taken when a probe starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionGeneration(u64);

#[derive(Debug)]
pub(crate) struct Reachability {
    backoff: Quarantine<PeerId>,
    generations: HashMap<PeerId, u64>,
}

impl Default for Reachability {
    fn default() -> Self {
        Self {
            backoff: Quarantine::backoff_only(),
            generations: HashMap::new(),
        }
    }
}

impl Reachability {
    /// `Some(remaining_ms)` while sends to `peer` back off.
    pub(crate) fn backing_off(&mut self, peer: PeerId, now_ms: u64) -> Option<u64> {
        match self.backoff.gate(&peer, now_ms, 0, 0) {
            DialGate::Backoff { remaining_ms } => Some(remaining_ms),
            DialGate::Dial | DialGate::Dead { .. } => None,
        }
    }

    /// The generation a probe of `peer` starts under.
    pub(crate) fn generation(&self, peer: PeerId) -> SessionGeneration {
        SessionGeneration(self.generations.get(&peer).copied().unwrap_or_default())
        // unwrap_or_default: a peer never seen is at generation zero
    }

    /// A session with `peer` installed: it answered. A new generation, and no backoff.
    pub(crate) fn session_installed(&mut self, peer: PeerId) {
        *self.generations.entry(peer).or_default() += 1;
        self.backoff.record_success(&peer);
    }

    /// `peer` formed no session for a remote reason, in a probe that started at `started`. Recorded
    /// only if no session was installed since; returns whether it was.
    pub(crate) fn record_unanswered(
        &mut self,
        peer: PeerId,
        started: SessionGeneration,
        now_ms: u64,
    ) -> bool {
        if self.generation(peer) != started {
            return false;
        }
        self.backoff.record_failure(peer, now_ms, 0);
        true
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.backoff.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the check-then-insert race (Codex on #1650). A probe starts, a session with
    // the recipient installs, then the probe's failure arrives: it is fenced out by generation, in
    // this exact order, with no scheduler involved.
    #[test]
    fn a_failure_from_a_probe_that_started_before_a_session_is_fenced_out() {
        let mut memo = Reachability::default();
        let peer = PeerId::new();
        let started = memo.generation(peer);
        memo.session_installed(peer);
        assert!(
            !memo.record_unanswered(peer, started, 1_000),
            "stale: a session installed since"
        );
        assert_eq!(memo.backing_off(peer, 1_000), None);
        let started = memo.generation(peer);
        assert!(
            memo.record_unanswered(peer, started, 1_000),
            "current: recorded"
        );
        assert!(memo.backing_off(peer, 1_000).is_some());
    }

    // what this catches: backoff keyed by the wrong identity. Another peer's session (a host,
    // another recipient) neither clears nor fences this recipient.
    #[test]
    fn another_peers_session_does_not_touch_the_recipient() {
        let mut memo = Reachability::default();
        let (recipient, host) = (PeerId::new(), PeerId::new());
        let started = memo.generation(recipient);
        memo.session_installed(host);
        assert!(memo.record_unanswered(recipient, started, 1_000));
        memo.session_installed(host);
        assert!(memo.backing_off(recipient, 1_000).is_some());
        memo.session_installed(recipient);
        assert_eq!(memo.backing_off(recipient, 1_000), None);
    }
}
