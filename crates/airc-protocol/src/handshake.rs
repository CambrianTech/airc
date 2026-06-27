//! Stream-plane handshake — "sign the handle."
//!
//! An Ed25519-authenticated X25519 ECDH that establishes the shared secret
//! feeding [`StreamSession`](crate::session) (see `docs/stream-plane-crypto.md`).
//! Each peer signs its **ephemeral** X25519 public key with its long-term
//! Ed25519 identity, so the ephemeral — and therefore the whole session — is
//! bound to the pinned peer identity (`PeerKeyRegistry`). This is the ONE
//! asymmetric cost; every subsequent frame is symmetric AEAD (nanoseconds).
//!
//! Properties:
//! - **Mutual authentication** — both sides verify the other's Ed25519 signature
//!   over the ephemeral, against the enrolled identity key.
//! - **Forward secrecy** — keys derive from ephemeral X25519 secrets that never
//!   touch disk; compromising a long-term key doesn't decrypt past sessions.
//! - **Response binding** — the responder signs over the INITIATOR's ephemeral
//!   too, so a response can't be replayed into a different handshake.
//! - **Transcript binding** — the session keys are derived under a transcript
//!   hash of both ephemerals + both peer ids, so a key can't be lifted.
//!
//! Two messages: `HandshakeInit` (initiator → responder) and `HandshakeResp`
//! (responder → initiator). Both serialize for the wire; the transport carries
//! them, this module is pure crypto + no I/O.

use ed25519_dalek::Signature as Ed25519Sig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey};

use airc_core::PeerId;

use crate::keypair::PeerKeypair;
use crate::session::{SessionRole, StreamSession};
use crate::signature::PeerKeyRegistry;

const DOMAIN_INIT: &[u8] = b"airc-stream-handshake-v1-init";
const DOMAIN_RESP: &[u8] = b"airc-stream-handshake-v1-resp";
const DOMAIN_TRANSCRIPT: &[u8] = b"airc-stream-handshake-v1-transcript";

/// Why a handshake step rejected its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// The claimed peer has no enrolled Ed25519 key in the registry — we don't
    /// trust them, so we won't agree a session.
    UnknownPeer,
    /// The peer's Ed25519 signature over its ephemeral did not verify — a
    /// forged, tampered, or cross-session message.
    BadSignature,
    /// The X25519 ECDH produced a non-contributory (all-zero) shared secret —
    /// the peer sent a low-order/identity ephemeral. Defense-in-depth: the
    /// signature gate already blocks an external MITM, but reject it outright
    /// rather than key a session off a predictable secret.
    WeakEphemeral,
    /// Deriving the [`StreamSession`] keys failed (HKDF) — unreachable for the
    /// fixed key size, propagated rather than panicked (no-panic gate).
    KeyDerivation,
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandshakeError::UnknownPeer => write!(f, "handshake peer not enrolled in registry"),
            HandshakeError::BadSignature => write!(f, "handshake signature verification failed"),
            HandshakeError::WeakEphemeral => {
                write!(
                    f,
                    "handshake ECDH produced a non-contributory shared secret"
                )
            }
            HandshakeError::KeyDerivation => write!(f, "handshake session key derivation failed"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Initiator → responder. The signed ephemeral that opens the handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeInit {
    /// The initiator's pinned identity.
    pub peer_id: PeerId,
    /// Which of the initiator's enrolled keys signed (supports rotation — the
    /// registry is keyed by `(PeerId, key_id)`). The verifier looks up THIS key.
    pub key_id: u32,
    /// The initiator's ephemeral X25519 public key.
    pub eph_pub: [u8; 32],
    /// Ed25519 over `DOMAIN_INIT || key_id || eph_pub || peer_id` by the
    /// initiator's identity key.
    #[serde(with = "crate::signature::serde_bytes_64")]
    pub sig: [u8; 64],
}

/// Responder → initiator. The signed ephemeral that completes the handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResp {
    /// The responder's pinned identity.
    pub peer_id: PeerId,
    /// Which of the responder's enrolled keys signed (rotation; see
    /// [`HandshakeInit::key_id`]).
    pub key_id: u32,
    /// The responder's ephemeral X25519 public key.
    pub eph_pub: [u8; 32],
    /// Ed25519 over `DOMAIN_RESP || key_id || eph_pub || initiator_eph_pub ||
    /// peer_id` by the responder's identity key — binds this response to the
    /// initiator's ephemeral so it can't be replayed into another handshake.
    #[serde(with = "crate::signature::serde_bytes_64")]
    pub sig: [u8; 64],
}

/// Initiator-side in-flight state: holds the ephemeral secret until the
/// responder's reply completes the exchange. Not `Clone` — the secret is
/// one-shot (consumed by the ECDH on `complete`).
pub struct PendingHandshake {
    secret: EphemeralSecret,
    eph_pub: [u8; 32],
    my_peer_id: PeerId,
}

fn uuid_bytes(peer: PeerId) -> [u8; 16] {
    *peer.as_uuid().as_bytes()
}

/// Bytes the initiator signs (and the responder verifies). Binds the advertised
/// `key_id` so it can't be tampered to point the verifier at a different key.
fn init_signed_bytes(eph_pub: &[u8; 32], peer: PeerId, key_id: u32) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DOMAIN_INIT.len() + 4 + 32 + 16);
    msg.extend_from_slice(DOMAIN_INIT);
    msg.extend_from_slice(&key_id.to_be_bytes());
    msg.extend_from_slice(eph_pub);
    msg.extend_from_slice(&uuid_bytes(peer));
    msg
}

/// Bytes the responder signs (and the initiator verifies). Includes the
/// initiator's ephemeral so the response is bound to THIS handshake, and the
/// responder's `key_id`.
fn resp_signed_bytes(
    resp_eph: &[u8; 32],
    init_eph: &[u8; 32],
    peer: PeerId,
    key_id: u32,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DOMAIN_RESP.len() + 4 + 32 + 32 + 16);
    msg.extend_from_slice(DOMAIN_RESP);
    msg.extend_from_slice(&key_id.to_be_bytes());
    msg.extend_from_slice(resp_eph);
    msg.extend_from_slice(init_eph);
    msg.extend_from_slice(&uuid_bytes(peer));
    msg
}

/// Transcript hash both peers derive identically — binds session keys to both
/// ephemerals + both identities.
fn transcript(init_eph: &[u8; 32], resp_eph: &[u8; 32], init: PeerId, resp: PeerId) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN_TRANSCRIPT);
    h.update(init_eph);
    h.update(resp_eph);
    h.update(uuid_bytes(init));
    h.update(uuid_bytes(resp));
    h.finalize().into()
}

fn verify_sig(
    registry: &PeerKeyRegistry,
    peer: PeerId,
    key_id: u32,
    message: &[u8],
    sig: &[u8; 64],
) -> Result<(), HandshakeError> {
    let key = registry
        .lookup(peer, key_id)
        .ok_or(HandshakeError::UnknownPeer)?;
    let signature = Ed25519Sig::from_bytes(sig);
    // verify_strict rejects cofactored/malleable signatures — the stricter
    // choice for a fresh security-plane module.
    key.verify_strict(message, &signature)
        .map_err(|_| HandshakeError::BadSignature)
}

/// Reject a non-contributory ECDH (all-zero shared secret from a low-order /
/// identity ephemeral) — defense-in-depth on the key-agreement boundary.
fn checked_shared(
    secret: EphemeralSecret,
    their_eph: &[u8; 32],
) -> Result<[u8; 32], HandshakeError> {
    let shared = secret.diffie_hellman(&PublicKey::from(*their_eph));
    if !shared.was_contributory() {
        return Err(HandshakeError::WeakEphemeral);
    }
    Ok(shared.to_bytes())
}

/// Start a handshake: generate an ephemeral, sign it, return the wire message +
/// the pending state to complete with the responder's reply.
pub fn initiate(
    keypair: &PeerKeypair,
    my_peer_id: PeerId,
    key_id: u32,
) -> (PendingHandshake, HandshakeInit) {
    let secret = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = PublicKey::from(&secret).to_bytes();
    let sig = keypair.sign_bytes(&init_signed_bytes(&eph_pub, my_peer_id, key_id));
    let init = HandshakeInit {
        peer_id: my_peer_id,
        key_id,
        eph_pub,
        sig,
    };
    (
        PendingHandshake {
            secret,
            eph_pub,
            my_peer_id,
        },
        init,
    )
}

/// Responder side: verify the initiator's signed ephemeral, generate our own,
/// derive the session (as [`SessionRole::Responder`]), and return our signed
/// reply for the initiator to complete with.
pub fn respond(
    keypair: &PeerKeypair,
    my_peer_id: PeerId,
    key_id: u32,
    init: &HandshakeInit,
    registry: &PeerKeyRegistry,
) -> Result<(StreamSession, HandshakeResp), HandshakeError> {
    verify_sig(
        registry,
        init.peer_id,
        init.key_id,
        &init_signed_bytes(&init.eph_pub, init.peer_id, init.key_id),
        &init.sig,
    )?;

    let secret = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = PublicKey::from(&secret).to_bytes();
    let shared = checked_shared(secret, &init.eph_pub)?;
    let transcript = transcript(&init.eph_pub, &eph_pub, init.peer_id, my_peer_id);
    let session = StreamSession::derive(&shared, &transcript, SessionRole::Responder)
        .map_err(|_| HandshakeError::KeyDerivation)?;

    let sig = keypair.sign_bytes(&resp_signed_bytes(
        &eph_pub,
        &init.eph_pub,
        my_peer_id,
        key_id,
    ));
    let resp = HandshakeResp {
        peer_id: my_peer_id,
        key_id,
        eph_pub,
        sig,
    };
    Ok((session, resp))
}

impl PendingHandshake {
    /// Complete the handshake with the responder's reply: verify it (bound to
    /// our ephemeral), run the ECDH, and derive the session (as
    /// [`SessionRole::Initiator`]). Consumes `self` — the ephemeral secret is
    /// one-shot.
    pub fn complete(
        self,
        resp: &HandshakeResp,
        registry: &PeerKeyRegistry,
    ) -> Result<StreamSession, HandshakeError> {
        verify_sig(
            registry,
            resp.peer_id,
            resp.key_id,
            &resp_signed_bytes(&resp.eph_pub, &self.eph_pub, resp.peer_id, resp.key_id),
            &resp.sig,
        )?;

        let shared = checked_shared(self.secret, &resp.eph_pub)?;
        let transcript = transcript(&self.eph_pub, &resp.eph_pub, self.my_peer_id, resp.peer_id);
        StreamSession::derive(&shared, &transcript, SessionRole::Initiator)
            .map_err(|_| HandshakeError::KeyDerivation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Peer {
        id: PeerId,
        keypair: PeerKeypair,
    }

    fn peer() -> Peer {
        Peer {
            id: PeerId::new(),
            keypair: PeerKeypair::generate(),
        }
    }

    /// A registry that trusts both peers' identity keys.
    fn registry(peers: &[&Peer]) -> PeerKeyRegistry {
        let r = PeerKeyRegistry::new();
        for p in peers {
            r.enrol(p.id, 0, p.keypair.public_bytes()).unwrap();
        }
        r
    }

    // what this catches: the full happy path — initiate → respond → complete
    // yields two sessions that interoperate BOTH directions. This is the whole
    // point: authenticated ECDH → a working symmetric session.
    #[test]
    fn full_handshake_establishes_interoperable_sessions() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&a, &b]);

        let (pending, init) = initiate(&a.keypair, a.id, 0);
        let (mut b_sess, resp) = respond(&b.keypair, b.id, 0, &init, &reg).unwrap();
        let mut a_sess = pending.complete(&resp, &reg).unwrap();

        let f = a_sess.seal(b"hdr", b"hello b").unwrap();
        assert_eq!(b_sess.open(&f, b"hdr").unwrap(), b"hello b");
        let g = b_sess.seal(b"hdr2", b"hello a").unwrap();
        assert_eq!(a_sess.open(&g, b"hdr2").unwrap(), b"hello a");
    }

    // what this catches: an initiator the responder doesn't trust (not enrolled)
    // is refused — no session with an unknown peer.
    #[test]
    fn unknown_initiator_rejected() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&b]); // a NOT enrolled
        let (_pending, init) = initiate(&a.keypair, a.id, 0);
        assert_eq!(
            respond(&b.keypair, b.id, 0, &init, &reg).err(),
            Some(HandshakeError::UnknownPeer)
        );
    }

    // what this catches: a tampered initiator signature (or swapped ephemeral
    // the attacker can't re-sign) is rejected — MITM on the init is out.
    #[test]
    fn tampered_init_rejected() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&a, &b]);
        let (_pending, mut init) = initiate(&a.keypair, a.id, 0);
        init.eph_pub[0] ^= 0x01; // swap the ephemeral; sig no longer matches
        assert_eq!(
            respond(&b.keypair, b.id, 0, &init, &reg).err(),
            Some(HandshakeError::BadSignature)
        );
    }

    // what this catches: a response not bound to OUR initiator ephemeral is
    // rejected on complete — a response from a different handshake can't be
    // replayed in (the responder signs over the initiator's ephemeral).
    #[test]
    fn response_not_bound_to_our_handshake_rejected() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&a, &b]);

        // Our handshake.
        let (pending, _init) = initiate(&a.keypair, a.id, 0);
        // A DIFFERENT initiator ephemeral the responder replied to.
        let (_other_pending, other_init) = initiate(&a.keypair, a.id, 0);
        let (_b_sess, resp_for_other) = respond(&b.keypair, b.id, 0, &other_init, &reg).unwrap();

        // Completing OUR pending with a response bound to the other handshake
        // must fail (sig is over the other initiator ephemeral, not ours).
        assert_eq!(
            pending.complete(&resp_for_other, &reg).err(),
            Some(HandshakeError::BadSignature)
        );
    }

    // what this catches: forward secrecy in practice — two independent
    // handshakes (fresh ephemerals) produce non-interoperable sessions, so a
    // frame from one can't be opened by the other.
    #[test]
    fn distinct_handshakes_produce_distinct_sessions() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&a, &b]);

        let (p1, i1) = initiate(&a.keypair, a.id, 0);
        let (mut b1, r1) = respond(&b.keypair, b.id, 0, &i1, &reg).unwrap();
        let _a1 = p1.complete(&r1, &reg).unwrap();

        let (p2, i2) = initiate(&a.keypair, a.id, 0);
        let (_b2, r2) = respond(&b.keypair, b.id, 0, &i2, &reg).unwrap();
        let mut a2 = p2.complete(&r2, &reg).unwrap();

        // a2 sealed under handshake 2; b1 is handshake 1 — must NOT open.
        let f = a2.seal(b"", b"secret").unwrap();
        assert!(b1.open(&f, b"").is_err());
    }

    // what this catches: the contributory-ECDH guard (finding 1). A malicious
    // but ENROLLED initiator sends a correctly-signed low-order (all-zero)
    // ephemeral; the signature passes, so only the was_contributory() check
    // stops it keying a session off a predictable shared secret.
    #[test]
    fn non_contributory_ephemeral_rejected() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&a, &b]);
        let eph_pub = [0u8; 32]; // low-order / identity point
        let sig = a.keypair.sign_bytes(&init_signed_bytes(&eph_pub, a.id, 0));
        let init = HandshakeInit {
            peer_id: a.id,
            key_id: 0,
            eph_pub,
            sig,
        };
        assert_eq!(
            respond(&b.keypair, b.id, 0, &init, &reg).err(),
            Some(HandshakeError::WeakEphemeral)
        );
    }

    // what this catches: rotation correctness (finding 2). Peers enrolled under
    // a non-zero key_id handshake successfully when they advertise that key_id —
    // the verifier looks up the advertised key, not a hardcoded 0.
    #[test]
    fn rotated_key_id_handshake_succeeds() {
        let (a, b) = (peer(), peer());
        let reg = PeerKeyRegistry::new();
        reg.enrol(a.id, 1, a.keypair.public_bytes()).unwrap();
        reg.enrol(b.id, 1, b.keypair.public_bytes()).unwrap();

        let (pending, init) = initiate(&a.keypair, a.id, 1);
        let (mut b_sess, resp) = respond(&b.keypair, b.id, 1, &init, &reg).unwrap();
        let mut a_sess = pending.complete(&resp, &reg).unwrap();
        let f = a_sess.seal(b"", b"rotated").unwrap();
        assert_eq!(b_sess.open(&f, b"").unwrap(), b"rotated");
    }

    // what this catches: advertising a key_id the registry doesn't hold for that
    // peer fails CLOSED (UnknownPeer) — never falls back to a different key.
    #[test]
    fn unknown_key_id_rejected() {
        let (a, b) = (peer(), peer());
        let reg = registry(&[&a, &b]); // enrolled under key_id 0 only
        let (_pending, init) = initiate(&a.keypair, a.id, 1); // advertises key_id 1
        assert_eq!(
            respond(&b.keypair, b.id, 0, &init, &reg).err(),
            Some(HandshakeError::UnknownPeer)
        );
    }
}
