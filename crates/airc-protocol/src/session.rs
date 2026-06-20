//! Stream-plane symmetric session — "sign the handle, not the frame."
//!
//! Per-frame Ed25519 (`signature.rs`) is correct for the low-rate CONTROL
//! plane but a throughput killer for high-rate STREAMS (WebRTC/UDP media) —
//! see `docs/stream-plane-crypto.md`. The fix is the standard DTLS-SRTP/QUIC/
//! Noise shape: authenticate the SESSION once (asymmetric), then protect each
//! frame with a SYMMETRIC AEAD keyed off that session — nanoseconds/frame
//! instead of ~113µs.
//!
//! This module is the symmetric half: given a shared secret (from the
//! Ed25519-authenticated X25519 handshake — a later slice), derive a
//! [`StreamSession`] and `seal`/`open` frames with ChaCha20-Poly1305.
//!
//! ## Correctness invariants (crypto is unforgiving)
//! - **Directional keys.** One shared secret → TWO keys via HKDF (i→r and
//!   r→i). Each direction encrypts under its own key, so the same counter
//!   space on both sides never reuses a (key, nonce) pair. You also cannot
//!   open a frame you sent (different key) — reflection is structurally out.
//! - **Monotonic per-direction counter → nonce.** The sender's counter is the
//!   AEAD nonce; it only ever increments. Counter exhaustion is a hard error
//!   (rekey), never wraps (wrap = nonce reuse = catastrophic).
//! - **Sliding replay window** on open: a counter must be in-window AND unseen,
//!   and the window only advances on an AUTHENTICATED frame, so a forged
//!   counter can neither replay nor poison the window.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// Which side of the handshake this peer is. Selects which derived key seals
/// vs opens, so the two peers agree on direction without extra negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    Initiator,
    Responder,
}

/// Errors from the symmetric session. Distinct variants so callers can tell a
/// replay (drop quietly) from an auth failure (a real tampered/forged frame)
/// from counter exhaustion (rekey).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// AEAD open failed — tampered ciphertext, wrong key, or wrong AAD.
    AeadFailed,
    /// The frame's counter was already seen, or is older than the replay
    /// window. Not necessarily an attack (UDP reorder past the window), but
    /// the frame must be dropped.
    Replay,
    /// The send counter is exhausted (2^64 frames). The session MUST be
    /// re-handshaked; sealing further would reuse a nonce.
    CounterExhausted,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::AeadFailed => write!(f, "stream-session AEAD open failed"),
            SessionError::Replay => write!(f, "stream-session frame replayed or too old"),
            SessionError::CounterExhausted => {
                write!(f, "stream-session send counter exhausted (rekey required)")
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// A sealed frame: the monotonic counter (the AEAD nonce, sent in the clear —
/// it's not secret, only unique) + the ciphertext (which includes the 16-byte
/// Poly1305 tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedFrame {
    pub counter: u64,
    pub ciphertext: Vec<u8>,
}

const WINDOW_BITS: u64 = 64;

/// Sliding-window anti-replay (RFC-6479 style, 64-wide). Tracks the highest
/// accepted counter and a bitmap of the 64 below it.
#[derive(Debug, Default, Clone, Copy)]
struct ReplayWindow {
    highest: u64,
    seen: u64,
    /// `true` once any frame has been accepted — distinguishes "counter 0 not
    /// yet seen" from the `highest == 0` default.
    armed: bool,
}

impl ReplayWindow {
    /// Would `ctr` be accepted right now (in window + unseen)? Pure.
    fn would_accept(&self, ctr: u64) -> bool {
        if !self.armed || ctr > self.highest {
            return true;
        }
        let diff = self.highest - ctr;
        if diff >= WINDOW_BITS {
            return false; // older than the window
        }
        self.seen & (1u64 << diff) == 0
    }

    /// Record `ctr` as accepted, advancing the window. Call ONLY after the
    /// frame authenticated.
    fn record(&mut self, ctr: u64) {
        if !self.armed {
            self.armed = true;
            self.highest = ctr;
            self.seen = 1;
            return;
        }
        if ctr > self.highest {
            let shift = ctr - self.highest;
            self.seen = if shift >= WINDOW_BITS {
                0
            } else {
                self.seen << shift
            };
            self.seen |= 1;
            self.highest = ctr;
        } else {
            let diff = self.highest - ctr;
            if diff < WINDOW_BITS {
                self.seen |= 1u64 << diff;
            }
        }
    }
}

/// A live symmetric session over one stream. Cheap per-frame: `seal`/`open`
/// are one ChaCha20-Poly1305 op each (no asymmetric crypto).
pub struct StreamSession {
    seal_key: ChaCha20Poly1305,
    open_key: ChaCha20Poly1305,
    seal_ctr: u64,
    replay: ReplayWindow,
}

impl StreamSession {
    /// Derive a session from the handshake's shared secret. `transcript` binds
    /// the keys to the specific handshake (the ephemeral pubkeys + peer ids) so
    /// a key can't be lifted to a different session. Both peers pass the SAME
    /// `shared`/`transcript`; opposite [`SessionRole`]s pick mirrored keys.
    pub fn derive(shared: &[u8; 32], transcript: &[u8], role: SessionRole) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(transcript), shared);
        let key_i2r = expand_key(&hk, b"airc stream v1 i2r");
        let key_r2i = expand_key(&hk, b"airc stream v1 r2i");
        let (seal_bytes, open_bytes) = match role {
            SessionRole::Initiator => (key_i2r, key_r2i),
            SessionRole::Responder => (key_r2i, key_i2r),
        };
        StreamSession {
            seal_key: ChaCha20Poly1305::new(Key::from_slice(&seal_bytes)),
            open_key: ChaCha20Poly1305::new(Key::from_slice(&open_bytes)),
            seal_ctr: 0,
            replay: ReplayWindow::default(),
        }
    }

    /// Seal `plaintext` (authenticating `aad` too) into the next frame. The
    /// counter increments; `aad` is authenticated but not encrypted — pass the
    /// routing headers a relay must read in the clear.
    pub fn seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Result<SealedFrame, SessionError> {
        let counter = self.seal_ctr;
        // Reserve the next counter up front; on exhaustion refuse rather than
        // wrap (a wrap would reuse a nonce under the same key).
        self.seal_ctr = counter
            .checked_add(1)
            .ok_or(SessionError::CounterExhausted)?;
        let nonce = nonce_from_counter(counter);
        let ciphertext = self
            .seal_key
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| SessionError::AeadFailed)?;
        Ok(SealedFrame {
            counter,
            ciphertext,
        })
    }

    /// Open a sealed frame: replay-check (in-window + unseen), then AEAD-verify,
    /// then mark seen. Returns the plaintext, or an error the caller drops on.
    pub fn open(&mut self, frame: &SealedFrame, aad: &[u8]) -> Result<Vec<u8>, SessionError> {
        // Cheap pre-check before spending a decrypt; the window is only
        // ADVANCED below, after authentication, so a forged counter can't
        // poison it.
        if !self.replay.would_accept(frame.counter) {
            return Err(SessionError::Replay);
        }
        let nonce = nonce_from_counter(frame.counter);
        let plaintext = self
            .open_key
            .decrypt(
                &nonce,
                Payload {
                    msg: &frame.ciphertext,
                    aad,
                },
            )
            .map_err(|_| SessionError::AeadFailed)?;
        self.replay.record(frame.counter);
        Ok(plaintext)
    }
}

fn expand_key(hk: &Hkdf<Sha256>, info: &[u8]) -> [u8; 32] {
    let mut okm = [0u8; 32];
    // expand only fails for absurd output lengths (>255*32); 32 bytes is safe.
    hk.expand(info, &mut okm)
        .expect("HKDF expand of 32 bytes is infallible");
    okm
}

/// 96-bit nonce from a 64-bit counter: 4 zero bytes ++ big-endian counter.
/// Unique per direction because the counter is monotonic and each direction
/// uses a distinct key.
fn nonce_from_counter(counter: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&counter.to_be_bytes());
    *Nonce::from_slice(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (StreamSession, StreamSession) {
        let shared = [7u8; 32];
        let transcript = b"test-handshake-transcript";
        (
            StreamSession::derive(&shared, transcript, SessionRole::Initiator),
            StreamSession::derive(&shared, transcript, SessionRole::Responder),
        )
    }

    // what this catches: the happy path BOTH ways — initiator→responder and
    // responder→initiator each round-trip under their directional keys.
    #[test]
    fn round_trips_both_directions() {
        let (mut i, mut r) = pair();
        let f = i.seal(b"aad", b"hello from initiator").unwrap();
        assert_eq!(r.open(&f, b"aad").unwrap(), b"hello from initiator");
        let g = r.seal(b"aad2", b"hello back").unwrap();
        assert_eq!(i.open(&g, b"aad2").unwrap(), b"hello back");
    }

    // what this catches: a flipped ciphertext byte fails the Poly1305 tag —
    // tamper is rejected, not silently accepted.
    #[test]
    fn tamper_fails() {
        let (mut i, mut r) = pair();
        let mut f = i.seal(b"", b"payload").unwrap();
        f.ciphertext[0] ^= 0x01;
        assert_eq!(r.open(&f, b""), Err(SessionError::AeadFailed));
    }

    // what this catches: AAD is authenticated — opening with different AAD than
    // was sealed fails (the routing headers can't be swapped under the frame).
    #[test]
    fn wrong_aad_fails() {
        let (mut i, mut r) = pair();
        let f = i.seal(b"headers-A", b"payload").unwrap();
        assert_eq!(r.open(&f, b"headers-B"), Err(SessionError::AeadFailed));
    }

    // what this catches: directional-key isolation — you CANNOT open a frame
    // you sealed (initiator's seal key ≠ initiator's open key). Prevents
    // reflection + cross-direction nonce reuse.
    #[test]
    fn cannot_open_own_sent_frame() {
        let (mut i, _r) = pair();
        let f = i.seal(b"", b"mine").unwrap();
        assert_eq!(i.open(&f, b""), Err(SessionError::AeadFailed));
    }

    // what this catches: replay — the same authenticated counter cannot be
    // opened twice.
    #[test]
    fn replay_rejected() {
        let (mut i, mut r) = pair();
        let f = i.seal(b"", b"once").unwrap();
        assert_eq!(r.open(&f, b"").unwrap(), b"once");
        assert_eq!(r.open(&f, b""), Err(SessionError::Replay));
    }

    // what this catches: in-window REORDER is fine (UDP delivers out of order)
    // but each counter still opens at most once.
    #[test]
    fn reorder_within_window_ok() {
        let (mut i, mut r) = pair();
        let frames: Vec<_> = (0..5).map(|n| i.seal(b"", &[n]).unwrap()).collect();
        // open out of order: 4, 2, 0, 3, 1
        for idx in [4usize, 2, 0, 3, 1] {
            assert_eq!(r.open(&frames[idx], b"").unwrap(), vec![idx as u8]);
        }
        // and a re-open of any is a replay
        assert_eq!(r.open(&frames[2], b""), Err(SessionError::Replay));
    }

    // what this catches: a frame older than the 64-wide window is rejected even
    // if never seen (can't keep state forever; the window is the contract).
    #[test]
    fn too_old_is_rejected() {
        let (mut i, mut r) = pair();
        let old = i.seal(b"", b"old").unwrap(); // counter 0
                                                // advance the window well past 64
        for _ in 0..70 {
            let f = i.seal(b"", b"x").unwrap();
            r.open(&f, b"").unwrap();
        }
        assert_eq!(r.open(&old, b""), Err(SessionError::Replay));
    }

    // what this catches: the seal counter increments per frame (nonce
    // uniqueness depends on it).
    #[test]
    fn counter_is_monotonic() {
        let (mut i, _r) = pair();
        assert_eq!(i.seal(b"", b"a").unwrap().counter, 0);
        assert_eq!(i.seal(b"", b"b").unwrap().counter, 1);
        assert_eq!(i.seal(b"", b"c").unwrap().counter, 2);
    }

    // what this catches: a different transcript yields different keys — a key
    // can't be lifted from one handshake to another.
    #[test]
    fn transcript_binds_keys() {
        let shared = [9u8; 32];
        let mut a = StreamSession::derive(&shared, b"transcript-1", SessionRole::Initiator);
        let mut b = StreamSession::derive(&shared, b"transcript-2", SessionRole::Responder);
        let f = a.seal(b"", b"payload").unwrap();
        assert_eq!(b.open(&f, b""), Err(SessionError::AeadFailed));
    }
}
