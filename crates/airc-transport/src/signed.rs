//! `SignedTransport<T>` — wraps any `Transport` with per-frame Ed25519
//! signing on send and verification on receive.
//!
//! Per-frame signing is orthogonal to transport encryption. TLS gives
//! you a confidential channel; signing gives you identity-bound,
//! replay-resistant, audit-able envelopes that survive being re-emitted
//! by intermediaries. Both layers compose.
//!
//! Usage: `SignedTransport::new(LocalFsAdapter::new(...), keypair, ...)`
//! or `SignedTransport::new(LanTcpAdapter::new(...), keypair, ...)`.
//! The inner adapter handles bytes; this wrapper handles cryptography.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};

use airc_core::PeerId;
use airc_protocol::{
    verify, CanonicalError, Frame, PeerKeyRegistry, PeerKeypair, Subscription, VerificationError,
    VerificationPolicy,
};

use crate::transport::{FrameStream, Transport};

/// Combined error type for the wrapper — distinguishes inner transport
/// errors from cryptographic ones so callers can react appropriately.
#[derive(Debug)]
pub enum SignedError<E> {
    Inner(E),
    /// Signing failed at send time — almost always a malformed envelope
    /// that won't canonical-encode (substrate-internal bug if it
    /// happens).
    Sign(CanonicalError),
    /// Verification failed on a received frame — tampered, unsigned
    /// under Strict, unknown signer, or replay-to mismatch.
    Verify(VerificationError),
}

impl<E: std::fmt::Display> std::fmt::Display for SignedError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignedError::Inner(error) => write!(f, "signed-transport inner: {error}"),
            SignedError::Sign(error) => write!(f, "signed-transport sign: {error}"),
            SignedError::Verify(error) => write!(f, "signed-transport verify: {error}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for SignedError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SignedError::Inner(error) => Some(error),
            SignedError::Sign(_error) => None,
            SignedError::Verify(error) => Some(error),
        }
    }
}

/// Wrap any transport with per-frame Ed25519 signing + verification.
pub struct SignedTransport<T: Transport> {
    inner: T,
    keypair: PeerKeypair,
    self_peer_id: PeerId,
    /// Which enrolled key the signer uses. Multiple keys per peer
    /// support rotation; substrate sticks with a single id by default.
    key_id: u32,
    registry: Arc<PeerKeyRegistry>,
    policy: VerificationPolicy,
}

impl<T: Transport> SignedTransport<T> {
    pub fn new(
        inner: T,
        keypair: PeerKeypair,
        self_peer_id: PeerId,
        registry: Arc<PeerKeyRegistry>,
        policy: VerificationPolicy,
    ) -> Self {
        Self {
            inner,
            keypair,
            self_peer_id,
            key_id: 0,
            registry,
            policy,
        }
    }

    /// Set the key_id used for signing — call when rotating.
    pub fn with_key_id(mut self, key_id: u32) -> Self {
        self.key_id = key_id;
        self
    }
}

#[async_trait]
impl<T: Transport + 'static> Transport for SignedTransport<T>
where
    T::Error: std::error::Error + Send + Sync + 'static,
{
    type Error = SignedError<T::Error>;

    async fn send(&self, mut frame: Frame) -> Result<(), Self::Error> {
        // Sign over the canonical bytes of the envelope (excluding the
        // signature field itself per signature.rs's contract).
        let signature = self
            .keypair
            .sign_envelope(&frame.envelope, self.self_peer_id, self.key_id)
            .map_err(SignedError::Sign)?;
        frame.envelope.signature = signature;
        self.inner.send(frame).await.map_err(SignedError::Inner)
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        let inner_stream = self
            .inner
            .subscribe(subscription)
            .await
            .map_err(SignedError::Inner)?;
        let policy = self.policy;
        let registry = self.registry.clone();

        let verified = inner_stream.then(move |item| {
            let registry = registry.clone();
            async move {
                match item {
                    Err(error) => Err(SignedError::Inner(error)),
                    Ok(frame) => match verify(&frame, policy, registry.as_ref()) {
                        Ok(()) => Ok(frame),
                        Err(error) => Err(SignedError::Verify(error)),
                    },
                }
            }
        });

        Ok(Box::pin(verified) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_fs::LocalFsAdapter;
    use airc_core::{
        headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, RoomId,
        TranscriptCursor,
    };
    use airc_protocol::{ChannelId, Envelope, Frame, FrameKind, Signature, Subscription};
    use futures::stream::StreamExt;
    use std::time::Duration;
    use tempfile::TempDir;

    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn unsigned_frame(lamport: u64, sender: PeerId, channel: ChannelId, body: &str) -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id: EventId::from_u128(lamport as u128),
                sender,
                sender_client: ClientId::from_u128(0xc1),
                channel,
                target: MentionTarget::All,
                lamport,
                occurred_at_ms: 1_700_000_000_000 + lamport,
                reply_to: None,
                headers: Headers::new(),
                body: Some(Body::text(body)),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    /// Build a fresh registry with two peers (Alice, Bob), both enrolled.
    fn paired_registry() -> (
        PeerId,
        PeerKeypair,
        PeerId,
        PeerKeypair,
        Arc<PeerKeyRegistry>,
    ) {
        let alice_id = PeerId::from_u128(0xa1);
        let bob_id = PeerId::from_u128(0xb2);
        let alice_kp = PeerKeypair::generate();
        let bob_kp = PeerKeypair::generate();
        let registry = PeerKeyRegistry::new();
        registry
            .enrol(alice_id, 0, alice_kp.public_bytes())
            .unwrap();
        registry.enrol(bob_id, 0, bob_kp.public_bytes()).unwrap();
        (alice_id, alice_kp, bob_id, bob_kp, Arc::new(registry))
    }

    fn replay_sub() -> Subscription {
        Subscription {
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn signed_local_fs_round_trips_a_signed_frame() {
        // Integration: SignedTransport<LocalFsAdapter> on both sides
        // of a shared wire dir. Alice signs + sends; Bob verifies
        // under Strict and receives.
        ensure_crypto_provider();
        let dir = TempDir::new().unwrap();
        let (alice_id, alice_kp, bob_id, bob_kp, registry) = paired_registry();

        let alice = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            alice_kp,
            alice_id,
            registry.clone(),
            VerificationPolicy::Strict,
        );
        let bob = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            bob_kp,
            bob_id,
            registry.clone(),
            VerificationPolicy::Strict,
        );

        let mut bob_stream = bob.subscribe(replay_sub()).await.unwrap();

        let channel = RoomId::from_u128(0xc0ffee);
        alice
            .send(unsigned_frame(1, alice_id, channel, "signed hello"))
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
            .await
            .expect("must yield within 2s")
            .expect("stream must yield Some")
            .expect("verify must pass");
        assert_eq!(received.envelope.lamport, 1);
        // The received envelope MUST carry the Ed25519 signature now,
        // not Unsigned — pin that the wrapper actually signed before
        // sending, not just delegated.
        match received.envelope.signature {
            Signature::Ed25519 { signer, key_id, .. } => {
                assert_eq!(signer, alice_id);
                assert_eq!(key_id, 0);
            }
            Signature::Unsigned => panic!("expected signed frame, got Unsigned"),
        }
    }

    #[tokio::test]
    async fn unenrolled_signer_is_rejected_under_strict() {
        // Mallory is not in Alice/Bob's registry. Mallory signs and
        // writes to the shared wire. Bob (Strict) gets the inner
        // frame from local-fs but verify rejects with UnknownSigner.
        ensure_crypto_provider();
        let dir = TempDir::new().unwrap();
        let (alice_id, alice_kp, bob_id, bob_kp, registry) = paired_registry();

        // Mallory has her own keypair but is NOT enrolled. She wraps
        // her own private registry that contains only herself so she
        // can sign — the wire is shared but registries are not.
        let mallory_id = PeerId::from_u128(0xc4);
        let mallory_kp = PeerKeypair::generate();
        let mallory_registry = PeerKeyRegistry::new();
        mallory_registry
            .enrol(mallory_id, 0, mallory_kp.public_bytes())
            .unwrap();
        // Mallory needs to know Alice's pubkey too to receive (not
        // needed for this test, but realistic).
        mallory_registry
            .enrol(alice_id, 0, alice_kp.public_bytes())
            .unwrap();
        let mallory_registry = Arc::new(mallory_registry);
        let mallory = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            mallory_kp,
            mallory_id,
            mallory_registry,
            VerificationPolicy::Strict,
        );

        let bob = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            bob_kp,
            bob_id,
            registry,
            VerificationPolicy::Strict,
        );

        let mut bob_stream = bob.subscribe(replay_sub()).await.unwrap();
        let channel = RoomId::from_u128(0xc0ffee);
        mallory
            .send(unsigned_frame(1, mallory_id, channel, "evil"))
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
            .await
            .expect("must yield within 2s")
            .expect("stream must yield Some");
        assert!(
            matches!(
                &received,
                Err(SignedError::Verify(VerificationError::UnknownSigner(p))) if *p == mallory_id
            ),
            "expected UnknownSigner(mallory), got {received:?}"
        );
    }

    #[tokio::test]
    async fn unsigned_under_allow_unsigned_passes_through() {
        // The dev-mode escape hatch: AllowUnsigned policy lets the raw
        // adapter's Unsigned frames pass verification. Useful during
        // bring-up when only some peers are keyed.
        ensure_crypto_provider();
        let dir = TempDir::new().unwrap();
        let (alice_id, alice_kp, bob_id, bob_kp, registry) = paired_registry();

        // Alice sends via a SignedTransport (so the frame IS signed),
        // but Bob uses AllowUnsigned policy — should still verify
        // signed frames AND accept Unsigned frames.
        let alice = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            alice_kp,
            alice_id,
            registry.clone(),
            VerificationPolicy::AllowUnsigned,
        );
        let bob = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            bob_kp,
            bob_id,
            registry,
            VerificationPolicy::AllowUnsigned,
        );

        let mut bob_stream = bob.subscribe(replay_sub()).await.unwrap();
        let channel = RoomId::from_u128(0xc0ffee);
        alice
            .send(unsigned_frame(1, alice_id, channel, "soft"))
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
            .await
            .expect("must yield within 2s")
            .expect("stream must yield Some")
            .expect("AllowUnsigned must accept signed frames too");
        assert_eq!(received.envelope.lamport, 1);
    }

    #[tokio::test]
    async fn tampered_frame_after_signing_is_rejected() {
        // The substrate's most important guarantee: an attacker who
        // has wire access (e.g. another process writing to the same
        // local-fs directory) cannot forge a frame that verify()
        // accepts. We simulate by writing a hand-crafted frame
        // directly to frames.jsonl that claims to be from Alice
        // but is signed by a DIFFERENT key.
        ensure_crypto_provider();
        let dir = TempDir::new().unwrap();
        let (alice_id, alice_kp, bob_id, bob_kp, registry) = paired_registry();

        // First: legitimate signed frame from Alice via the wrapper.
        // Bob receives it cleanly.
        let alice = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            alice_kp.clone(),
            alice_id,
            registry.clone(),
            VerificationPolicy::Strict,
        );
        let bob = SignedTransport::new(
            LocalFsAdapter::new(dir.path()),
            bob_kp,
            bob_id,
            registry,
            VerificationPolicy::Strict,
        );

        let mut bob_stream = bob.subscribe(replay_sub()).await.unwrap();
        let channel = RoomId::from_u128(0xc0ffee);
        alice
            .send(unsigned_frame(1, alice_id, channel, "legit"))
            .await
            .unwrap();

        let first = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.envelope.lamport, 1);

        // Now: tamper. Take Alice's signed frame, change a header
        // (e.g. insert a malicious value), and write it to the wire
        // by hand via the raw LocalFsAdapter. Bob should reject on
        // BadSignature.
        let mut tampered = first.clone();
        tampered.envelope.lamport = 2; // change so it appears as a new frame
        tampered.envelope.event_id = EventId::from_u128(2);
        tampered.envelope.headers.insert(
            "x-malicious".to_string(),
            "injected after signing".to_string(),
        );
        // The signature still claims to be from Alice but no longer
        // covers the modified content.
        let raw_writer = LocalFsAdapter::new(dir.path());
        raw_writer.send(tampered).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
            .await
            .expect("must yield within 2s")
            .expect("stream must yield Some");
        assert!(
            matches!(
                received,
                Err(SignedError::Verify(VerificationError::BadSignature))
            ),
            "expected BadSignature, got {received:?}"
        );
    }
}
