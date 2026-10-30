//! `SessionManager` — per-peer stream-session lifecycle (slice 3b).
//!
//! The stateful manager over the stateless crypto core ([`session`] +
//! [`handshake`]): it drives the Ed25519-authenticated X25519 handshake, holds a
//! live [`StreamSession`] per peer, and seals/opens frames against it. **No
//! I/O** — it produces and consumes handshake/data messages; the caller (a
//! `SessionTransport<T>`, the next slice) carries the bytes over the wire. That
//! keeps this a pure, exhaustively-testable state machine, separate from the
//! transport plumbing.
//!
//! Lifecycle (initiator A, responder B):
//! ```text
//! A.begin_handshake(B)  -> HandshakeInit  ──wire──▶  B.on_init(init) -> HandshakeResp
//! A.on_resp(resp)  ◀──wire──────────────────────────┘   (B session for A installed)
//! (A session for B installed)
//! A.seal_for(B, …) ──▶ B.open_from(A, …)   and vice-versa
//! ```
//!
//! [`session`]: crate::session
//! [`handshake`]: crate::handshake

use std::sync::Arc;

use dashmap::DashMap;

use airc_core::PeerId;

use crate::handshake::{self, HandshakeError, HandshakeInit, HandshakeResp, PendingHandshake};
use crate::keypair::PeerKeypair;
use crate::session::{SealedFrame, SessionError, StreamSession};
use crate::signature::PeerKeyRegistry;

/// Errors from the session manager — distinguishes the handshake phase, the
/// per-frame AEAD phase, and "no session / no pending handshake" state errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionManagerError {
    /// A handshake step failed (unknown peer, bad signature, weak ephemeral).
    Handshake(HandshakeError),
    /// A seal/open failed (AEAD, replay, counter exhausted).
    Session(SessionError),
    /// `seal_for`/`open_from` with no established session for that peer — the
    /// caller must complete a handshake first.
    NoSession,
    /// `on_resp` for a peer we never `begin_handshake`'d (or already completed).
    NoPendingHandshake,
    /// Both peers initiated at once and the other peer's id is higher, so ours
    /// is the handshake that completes: drop their init without answering. The
    /// higher-id peer yields on receiving ours, so exactly one session forms.
    Crossed,
    /// The init is not newer than the last one accepted from that peer: a
    /// recorded init replayed to reset a live session. Refused.
    StaleInit,
}

impl std::fmt::Display for SessionManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionManagerError::Handshake(e) => write!(f, "session-manager handshake: {e}"),
            SessionManagerError::Session(e) => write!(f, "session-manager session: {e}"),
            SessionManagerError::NoSession => write!(f, "session-manager: no session for peer"),
            SessionManagerError::NoPendingHandshake => {
                write!(f, "session-manager: no pending handshake for peer")
            }
            SessionManagerError::Crossed => {
                write!(f, "session-manager: crossed handshake; ours completes")
            }
            SessionManagerError::StaleInit => {
                write!(f, "session-manager: init not newer than the last accepted")
            }
        }
    }
}

impl std::error::Error for SessionManagerError {}

impl From<HandshakeError> for SessionManagerError {
    fn from(e: HandshakeError) -> Self {
        SessionManagerError::Handshake(e)
    }
}
impl From<SessionError> for SessionManagerError {
    fn from(e: SessionError) -> Self {
        SessionManagerError::Session(e)
    }
}

/// Owns this peer's identity + the live sessions/handshakes with every other
/// peer. Cheap to share (`Arc` the whole thing); the inner maps are sharded.
pub struct SessionManager {
    keypair: PeerKeypair,
    peer_id: PeerId,
    key_id: u32,
    registry: Arc<PeerKeyRegistry>,
    /// Established sessions, keyed by the OTHER peer's id.
    sessions: DashMap<PeerId, StreamSession>,
    /// In-flight initiations we started, awaiting the responder's reply, keyed
    /// by the peer we initiated TO.
    pending: DashMap<PeerId, PendingHandshake>,
    /// The `issued_at_ms` of the last init accepted from each peer. An init must
    /// be strictly newer, so a recorded one can't replace a live session.
    accepted_init_at: DashMap<PeerId, u64>,
    /// The last `issued_at_ms` this side signed, so two inits in the same
    /// millisecond still increase.
    last_issued_ms: std::sync::atomic::AtomicU64,
}

impl SessionManager {
    pub fn new(keypair: PeerKeypair, peer_id: PeerId, registry: Arc<PeerKeyRegistry>) -> Self {
        Self {
            keypair,
            peer_id,
            key_id: 0,
            registry,
            sessions: DashMap::new(),
            pending: DashMap::new(),
            accepted_init_at: DashMap::new(),
            last_issued_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Use a non-default signing key_id (key rotation).
    pub fn with_key_id(mut self, key_id: u32) -> Self {
        self.key_id = key_id;
        self
    }

    /// Is there a live session with `peer`?
    pub fn has_session(&self, peer: PeerId) -> bool {
        self.sessions.contains_key(&peer)
    }

    /// Is a handshake we started to `peer` still awaiting its response?
    pub fn has_pending(&self, peer: PeerId) -> bool {
        self.pending.contains_key(&peer)
    }

    /// Abandon our unanswered handshake to `peer` (its init or response was
    /// lost), so the next send starts a fresh one.
    pub fn cancel_pending(&self, peer: PeerId) {
        self.pending.remove(&peer);
    }

    /// Begin a handshake to `peer`: returns the `HandshakeInit` to send and
    /// records the pending state to finish with [`on_resp`](Self::on_resp).
    /// Re-initiating replaces any prior pending handshake to that peer. `now_ms`
    /// is this side's wall clock; the signed issue time is strictly increasing
    /// even when two inits share a millisecond.
    pub fn begin_handshake(&self, peer: PeerId, now_ms: u64) -> HandshakeInit {
        use std::sync::atomic::Ordering;
        let mut last = self.last_issued_ms.load(Ordering::Acquire);
        let issued_at_ms = loop {
            let next = now_ms.max(last.saturating_add(1));
            match self.last_issued_ms.compare_exchange_weak(
                last,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break next,
                Err(current) => last = current,
            }
        };
        let (pending, init) =
            handshake::initiate(&self.keypair, self.peer_id, self.key_id, peer, issued_at_ms);
        self.pending.insert(peer, pending);
        init
    }

    /// Handle an inbound `HandshakeInit`: verify it, install our session (as
    /// responder), and return the `HandshakeResp` to send back.
    ///
    /// A crossed handshake (we initiated to them too) completes only the lower
    /// id's init. An init not newer than the last accepted from that peer is
    /// refused. A newer one replaces a live session: that is how a restarted
    /// peer re-keys.
    pub fn on_init(&self, init: &HandshakeInit) -> Result<HandshakeResp, SessionManagerError> {
        if self.pending.contains_key(&init.peer_id)
            && self.peer_id.as_uuid() < init.peer_id.as_uuid()
        {
            return Err(SessionManagerError::Crossed);
        }
        if let Some(last) = self.accepted_init_at.get(&init.peer_id) {
            if init.issued_at_ms <= *last {
                return Err(SessionManagerError::StaleInit);
            }
        }
        let (session, resp) = handshake::respond(
            &self.keypair,
            self.peer_id,
            self.key_id,
            init,
            &self.registry,
        )?;
        self.pending.remove(&init.peer_id);
        self.accepted_init_at
            .insert(init.peer_id, init.issued_at_ms);
        self.sessions.insert(init.peer_id, session);
        Ok(resp)
    }

    /// Handle the responder's `HandshakeResp`: complete our pending handshake to
    /// that peer and install the session (as initiator).
    pub fn on_resp(&self, resp: &HandshakeResp) -> Result<(), SessionManagerError> {
        let (_peer, pending) = self
            .pending
            .remove(&resp.peer_id)
            .ok_or(SessionManagerError::NoPendingHandshake)?;
        let session = pending.complete(resp, &self.registry)?;
        self.sessions.insert(resp.peer_id, session);
        Ok(())
    }

    /// Seal a frame for `peer` (requires an established session). `aad` is
    /// authenticated-not-encrypted — pass the routing headers a relay reads.
    pub fn seal_for(
        &self,
        peer: PeerId,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<SealedFrame, SessionManagerError> {
        let mut session = self
            .sessions
            .get_mut(&peer)
            .ok_or(SessionManagerError::NoSession)?;
        Ok(session.seal(aad, plaintext)?)
    }

    /// Open a sealed frame from `peer` (requires an established session).
    pub fn open_from(
        &self,
        peer: PeerId,
        aad: &[u8],
        frame: &SealedFrame,
    ) -> Result<Vec<u8>, SessionManagerError> {
        let mut session = self
            .sessions
            .get_mut(&peer)
            .ok_or(SessionManagerError::NoSession)?;
        Ok(session.open(frame, aad)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr(reg: &Arc<PeerKeyRegistry>) -> (SessionManager, PeerId) {
        let keypair = PeerKeypair::generate();
        let id = PeerId::new();
        reg.enrol(id, 0, keypair.public_bytes()).unwrap();
        (SessionManager::new(keypair, id, Arc::clone(reg)), id)
    }

    // what this catches: the full lifecycle end-to-end through the manager —
    // begin_handshake → on_init → on_resp installs sessions on both sides, then
    // seal_for/open_from interoperate BOTH directions. This is slice 3b's core:
    // the stateful session lifecycle over the crypto primitives.
    #[test]
    fn handshake_then_seal_open_both_ways() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (a, a_id) = mgr(&reg);
        let (b, b_id) = mgr(&reg);

        assert!(!a.has_session(b_id));
        let init = a.begin_handshake(b_id, 1_000);
        let resp = b.on_init(&init).unwrap();
        a.on_resp(&resp).unwrap();
        assert!(a.has_session(b_id) && b.has_session(a_id));

        let f = a.seal_for(b_id, b"hdr", b"a->b").unwrap();
        assert_eq!(b.open_from(a_id, b"hdr", &f).unwrap(), b"a->b");
        let g = b.seal_for(a_id, b"hdr", b"b->a").unwrap();
        assert_eq!(a.open_from(b_id, b"hdr", &g).unwrap(), b"b->a");
    }

    // what this catches: sealing/opening before a handshake fails with NoSession
    // (not a panic, not plaintext) — the caller must establish a session first.
    #[test]
    fn no_session_before_handshake() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (a, _a_id) = mgr(&reg);
        let (_b, b_id) = mgr(&reg);
        assert_eq!(
            a.seal_for(b_id, b"", b"x").err(),
            Some(SessionManagerError::NoSession)
        );
        assert_eq!(
            a.open_from(
                b_id,
                b"",
                &SealedFrame {
                    counter: 0,
                    ciphertext: vec![0; 32]
                }
            )
            .err(),
            Some(SessionManagerError::NoSession)
        );
    }

    // what this catches: on_resp without a matching begin_handshake is rejected
    // (NoPendingHandshake) — a stray/replayed response can't install a session.
    #[test]
    fn resp_without_pending_rejected() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (a, _a_id) = mgr(&reg);
        let (b, _b_id) = mgr(&reg);
        let (c, _c_id) = mgr(&reg);
        // b responds to a handshake a started with c (not b's business to a).
        let init_for_c = a.begin_handshake(_c_id, 1_000);
        let resp = c.on_init(&init_for_c).unwrap();
        // a never began a handshake with b, so b's view is irrelevant; feed the
        // resp to a peer that has no pending for resp.peer_id (c) — use b.
        assert_eq!(
            b.on_resp(&resp).err(),
            Some(SessionManagerError::NoPendingHandshake)
        );
    }

    // what this catches: an unenrolled initiator is refused at on_init (the
    // handshake's trust gate flows through the manager).
    #[test]
    fn unenrolled_initiator_rejected() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (_a, _a_id) = mgr(&reg);
        let (b, _b_id) = mgr(&reg);
        // A stranger NOT in the registry initiates.
        let stranger_kp = PeerKeypair::generate();
        let stranger_id = PeerId::new();
        let stranger = SessionManager::new(stranger_kp, stranger_id, Arc::clone(&reg));
        let init = stranger.begin_handshake(_b_id, 1_000);
        assert_eq!(
            b.on_init(&init).err(),
            Some(SessionManagerError::Handshake(HandshakeError::UnknownPeer))
        );
    }

    // what this catches: both peers initiate at once (the normal case when two
    // nodes send each other a command in the same instant). Without a tie-break
    // each side installs a session from a different handshake and every frame
    // fails AEAD. The lower id's handshake completes; the higher id yields.
    #[test]
    fn crossed_handshakes_form_exactly_one_session() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (x, x_id) = mgr(&reg);
        let (y, y_id) = mgr(&reg);
        let ((lo, lo_id), (hi, hi_id)) = if x_id.as_uuid() < y_id.as_uuid() {
            ((x, x_id), (y, y_id))
        } else {
            ((y, y_id), (x, x_id))
        };
        let lo_init = lo.begin_handshake(hi_id, 1_000);
        let hi_init = hi.begin_handshake(lo_id, 1_000);

        assert_eq!(
            lo.on_init(&hi_init).err(),
            Some(SessionManagerError::Crossed)
        );
        let resp = hi.on_init(&lo_init).unwrap();
        lo.on_resp(&resp).unwrap();

        let f = lo.seal_for(hi_id, b"", b"lo->hi").unwrap();
        assert_eq!(hi.open_from(lo_id, b"", &f).unwrap(), b"lo->hi");
        let g = hi.seal_for(lo_id, b"", b"hi->lo").unwrap();
        assert_eq!(lo.open_from(hi_id, b"", &g).unwrap(), b"hi->lo");
    }

    // what this catches: a recorded init replayed later must not replace the
    // live session (that would cut the link: the replayer has no ephemeral
    // secret, so the real peer's frames stop opening).
    #[test]
    fn replayed_init_does_not_replace_a_live_session() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (a, a_id) = mgr(&reg);
        let (b, b_id) = mgr(&reg);
        let init = a.begin_handshake(b_id, 1_000);
        let resp = b.on_init(&init).unwrap();
        a.on_resp(&resp).unwrap();

        assert_eq!(b.on_init(&init).err(), Some(SessionManagerError::StaleInit));
        let f = a.seal_for(b_id, b"", b"still live").unwrap();
        assert_eq!(b.open_from(a_id, b"", &f).unwrap(), b"still live");
    }

    // what this catches: a peer that restarts (new manager, sessions lost) can
    // re-key with a newer init, and two inits signed in the same millisecond
    // still increase, so the second is not refused as stale.
    #[test]
    fn a_restarted_peer_rekeys_with_a_newer_init() {
        let reg = Arc::new(PeerKeyRegistry::new());
        let keypair = PeerKeypair::generate();
        let keypair_bytes = keypair.secret_bytes();
        let a_id = PeerId::new();
        reg.enrol(a_id, 0, keypair.public_bytes()).unwrap();
        let a = SessionManager::new(keypair, a_id, Arc::clone(&reg));
        let (b, b_id) = mgr(&reg);

        let first = a.begin_handshake(b_id, 1_000);
        let second = a.begin_handshake(b_id, 1_000);
        assert!(second.issued_at_ms > first.issued_at_ms);
        b.on_init(&first).unwrap();

        let restarted = SessionManager::new(
            PeerKeypair::from_secret_bytes(&keypair_bytes),
            a_id,
            Arc::clone(&reg),
        );
        let init = restarted.begin_handshake(b_id, 2_000);
        let resp = b.on_init(&init).unwrap();
        restarted.on_resp(&resp).unwrap();
        let f = restarted.seal_for(b_id, b"", b"after restart").unwrap();
        assert_eq!(b.open_from(a_id, b"", &f).unwrap(), b"after restart");
    }
}
