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
        let signature = airc_diagnostics::timing::timed("transport.sign", || {
            self.keypair
                .sign_envelope(&frame.envelope, self.self_peer_id, self.key_id)
        })
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
