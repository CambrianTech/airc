//! Build rustls `ServerConfig` and `ClientConfig` wired to the
//! substrate's peer-pinning verifiers.
//!
//! Both configs are TLS 1.3 only, Ed25519 only, mutual-auth. The
//! standard CA path is replaced by `PinnedServerVerifier` /
//! `PinnedClientVerifier` from this module — there is no fall-back to
//! standard PKI, and the configs refuse to be built without a registry
//! reference.

use std::sync::Arc;

use rustls::{ClientConfig, ServerConfig};

use airc_core::PeerId;
use airc_protocol::{PeerKeyRegistry, PeerKeypair};

use crate::lan_tcp::cert::{generate_self_signed_cert, CertGenError};
use crate::lan_tcp::verifier::{PinnedClientVerifier, PinnedServerVerifier};

/// What can go wrong configuring the TLS layer.
#[derive(Debug)]
pub enum TlsConfigError {
    /// Cert generation from the peer keypair failed.
    CertGen(CertGenError),

    /// rustls rejected the config — usually a misconfigured cipher
    /// suite, key/cert mismatch, or missing crypto provider.
    Rustls(rustls::Error),
}

impl std::fmt::Display for TlsConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TlsConfigError::CertGen(error) => write!(f, "TLS cert generation: {error}"),
            TlsConfigError::Rustls(error) => write!(f, "rustls config: {error}"),
        }
    }
}

impl std::error::Error for TlsConfigError {}

impl From<CertGenError> for TlsConfigError {
    fn from(error: CertGenError) -> Self {
        TlsConfigError::CertGen(error)
    }
}

impl From<rustls::Error> for TlsConfigError {
    fn from(error: rustls::Error) -> Self {
        TlsConfigError::Rustls(error)
    }
}

/// Build a TLS `ServerConfig` that presents the peer's self-signed
/// Ed25519-bound cert and verifies incoming client certs against the
/// registry via `PinnedClientVerifier`.
///
/// `peer_id` is THIS peer's id (used as the cert subject CN).
/// `keypair` is THIS peer's signing key (used to sign the cert).
/// `registry` is the trust anchor (must contain the peers expected to
/// connect; an empty registry means the server refuses every client).
pub fn build_server_config(
    peer_id: PeerId,
    keypair: &PeerKeypair,
    registry: Arc<PeerKeyRegistry>,
) -> Result<Arc<ServerConfig>, TlsConfigError> {
    let (cert, key) = generate_self_signed_cert(keypair, peer_id)?;

    let verifier = Arc::new(PinnedClientVerifier::new(registry));

    let config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert], key)?;

    Ok(Arc::new(config))
}

/// Build a TLS `ClientConfig` that presents the peer's cert and pins
/// the server side to `expected_peer`'s enrolled Ed25519 pubkey.
///
/// Construct a fresh `ClientConfig` per `connect(...)` call when the
/// expected peer differs — the verifier is baked in at config build
/// time, not selectable per-connection.
pub fn build_client_config(
    self_peer_id: PeerId,
    keypair: &PeerKeypair,
    expected_peer: PeerId,
    registry: Arc<PeerKeyRegistry>,
) -> Result<Arc<ClientConfig>, TlsConfigError> {
    let (cert, key) = generate_self_signed_cert(keypair, self_peer_id)?;

    let verifier = Arc::new(PinnedServerVerifier::new(expected_peer, registry));

    let config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert], key)?;

    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_crypto_provider() {
        // rustls 0.23 requires a CryptoProvider to be installed before
        // building configs. We install ring once per test process;
        // ignore the duplicate-installation error since tests can run
        // in any order.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn server_config_builds_for_enrolled_peer() {
        ensure_crypto_provider();
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();

        let registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();
        let registry = Arc::new(registry);

        let config = build_server_config(peer, &keypair, registry);
        assert!(config.is_ok(), "expected Ok, got {config:?}");
    }

    #[test]
    fn client_config_builds_for_expected_peer() {
        ensure_crypto_provider();
        let self_peer = PeerId::from_u128(0xa1);
        let other_peer = PeerId::from_u128(0xb2);
        let self_kp = PeerKeypair::generate();
        let other_kp = PeerKeypair::generate();

        let registry = PeerKeyRegistry::new();
        registry
            .enrol(self_peer, 0, self_kp.public_bytes())
            .unwrap();
        registry
            .enrol(other_peer, 0, other_kp.public_bytes())
            .unwrap();
        let registry = Arc::new(registry);

        let config = build_client_config(self_peer, &self_kp, other_peer, registry);
        assert!(config.is_ok(), "expected Ok, got {config:?}");
    }
}
