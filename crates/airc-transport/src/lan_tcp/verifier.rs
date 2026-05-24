//! TLS verifiers that pin to enrolled Ed25519 pubkeys.
//!
//! rustls's default chain-of-trust validation (CA → intermediate →
//! leaf) doesn't fit airc's model: there is no CA, no PKI hierarchy,
//! no chain. The substrate's `PeerKeyRegistry` IS the trust anchor.
//! We replace rustls's standard verifiers with these two:
//!
//! - **`PinnedServerVerifier`** — client-side. The caller knows which
//!   peer they're dialing (passed at construction). The verifier
//!   accepts the server cert iff its subject pubkey matches one of
//!   the expected peer's enrolled keys. Wrong peer / wrong key →
//!   reject. Cert isn't Ed25519 → reject. Pubkey not enrolled →
//!   reject. There is no path that accepts an unknown server.
//!
//! - **`PinnedClientVerifier`** — server-side. The server can't know
//!   in advance which peer is connecting, so the verifier accepts any
//!   client whose cert pubkey is enrolled in the registry for ANY
//!   peer. Caller looks up the peer post-handshake via
//!   `PeerKeyRegistry::find_peer` for connection binding. Cert not in
//!   registry → reject.
//!
//! Both verifiers reject:
//!   - Malformed cert DER
//!   - Non-Ed25519 subject pubkey (wrong algorithm OID)
//!   - Pubkey length ≠ 32 bytes (malformed Ed25519 SPKI)
//!
//! There is no "fall back to standard PKI" path. There is no
//! "allow self-signed" toggle. The only acceptance criterion is
//! "this pubkey is in the registry."

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error as RustlsError, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

use airc_core::PeerId;
use airc_protocol::PeerKeyRegistry;

use crate::lan_tcp::cert::extract_ed25519_pubkey;

/// Schemes we tell rustls we can verify for. Just Ed25519 — substrate
/// identity is exclusively Ed25519, no fall-back ciphers.
fn supported_schemes() -> Vec<SignatureScheme> {
    vec![SignatureScheme::ED25519]
}

/// Client-side: the caller dialed a specific `expected_peer` and the
/// server's cert MUST present one of that peer's enrolled keys.
#[derive(Debug)]
pub struct PinnedServerVerifier {
    expected_peer: PeerId,
    registry: Arc<PeerKeyRegistry>,
}

impl PinnedServerVerifier {
    pub fn new(expected_peer: PeerId, registry: Arc<PeerKeyRegistry>) -> Self {
        Self {
            expected_peer,
            registry,
        }
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let pubkey = extract_ed25519_pubkey(end_entity)
            .map_err(|error| RustlsError::General(error.to_string()))?;

        // Resolve the cert's pubkey to a (peer, key_id) entry. We
        // accept only if the resolved peer matches the expected one.
        match self.registry.find_peer(&pubkey) {
            Some((peer, _key_id)) if peer == self.expected_peer => {
                Ok(ServerCertVerified::assertion())
            }
            Some((peer, _)) => Err(RustlsError::General(format!(
                "server cert is for peer {peer}, expected {expected}",
                expected = self.expected_peer
            ))),
            None => Err(RustlsError::General(format!(
                "server cert pubkey is not enrolled (expected peer {})",
                self.expected_peer
            ))),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        // Substrate is Ed25519-only; TLS 1.2 paths are not used. Reject
        // explicitly so a downgrade attack surfaces as a hard error.
        Err(RustlsError::General(
            "TLS 1.2 not supported by airc substrate (Ed25519-only, TLS 1.3 path)".into(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_ed25519(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_schemes()
    }
}

/// Server-side: accept any cert whose pubkey is enrolled in the
/// registry. The caller resolves the peer post-handshake.
#[derive(Debug)]
pub struct PinnedClientVerifier {
    registry: Arc<PeerKeyRegistry>,
    /// Held as an owned empty slice so `root_hint_subjects` can
    /// return a borrow per the rustls 0.23 trait signature.
    empty_hints: Vec<DistinguishedName>,
}

impl PinnedClientVerifier {
    pub fn new(registry: Arc<PeerKeyRegistry>) -> Self {
        Self {
            registry,
            empty_hints: Vec::new(),
        }
    }
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        // No CA-issued hints; clients use self-signed substrate certs.
        // Returning empty tells rustls "I'm not advertising any CA
        // subject names" which is the correct posture for self-signed
        // peer-pinned auth.
        &self.empty_hints
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        let pubkey = extract_ed25519_pubkey(end_entity)
            .map_err(|error| RustlsError::General(error.to_string()))?;

        // Any enrolled peer is acceptable; the adapter will read the
        // cert pubkey post-handshake and bind the connection to the
        // resolved PeerId. Pubkey not in registry → reject.
        if self.registry.find_peer(&pubkey).is_some() {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(RustlsError::General(
                "client cert pubkey is not enrolled in the peer key registry".to_string(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Err(RustlsError::General(
            "TLS 1.2 not supported by airc substrate (Ed25519-only, TLS 1.3 path)".into(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_ed25519(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_schemes()
    }
}

/// Shared Ed25519 TLS 1.3 signature verification. Both verifiers
/// delegate here so the crypto check stays in one place.
fn verify_tls13_ed25519(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, RustlsError> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(RustlsError::General(format!(
            "TLS handshake offered scheme {scheme:?}, expected Ed25519 only",
            scheme = dss.scheme,
        )));
    }

    let pubkey_bytes =
        extract_ed25519_pubkey(cert).map_err(|error| RustlsError::General(error.to_string()))?;

    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&pubkey_bytes)
        .map_err(|error| RustlsError::General(format!("Ed25519 pubkey decode: {error}")))?;

    if dss.signature().len() != 64 {
        return Err(RustlsError::General(format!(
            "TLS 1.3 Ed25519 signature is {} bytes, expected 64",
            dss.signature().len()
        )));
    }
    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(dss.signature());
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);

    use ed25519_dalek::Verifier;
    verifying_key
        .verify(message, &signature)
        .map(|_| HandshakeSignatureValid::assertion())
        .map_err(|_| RustlsError::General("TLS 1.3 Ed25519 signature did not verify".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_protocol::PeerKeypair;

    use crate::lan_tcp::cert::generate_self_signed_cert;

    fn make_registry_with(peer: PeerId, keypair: &PeerKeypair) -> Arc<PeerKeyRegistry> {
        let registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();
        Arc::new(registry)
    }

    fn unix_now() -> UnixTime {
        // The verifiers ignore the time argument (we don't enforce
        // notBefore/notAfter — pinning IS the check). Pass a stable
        // value so tests stay deterministic.
        UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000))
    }

    #[test]
    fn server_verifier_accepts_expected_peer_cert() {
        // The base secure case: dialing peer A, A's cert presents
        // their enrolled key → verify_server_cert returns Ok.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let (cert, _) = generate_self_signed_cert(&keypair, peer).unwrap();
        let registry = make_registry_with(peer, &keypair);

        let verifier = PinnedServerVerifier::new(peer, registry);
        let server_name = ServerName::try_from("localhost").unwrap();
        let result = verifier.verify_server_cert(&cert, &[], &server_name, &[], unix_now());
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[test]
    fn server_verifier_rejects_wrong_peer_cert() {
        // We dialed peer A, but the cert is from peer B (B also has
        // an enrolled key in the registry, just not the one we want).
        // Must reject — pinning isn't "any enrolled peer," it's "the
        // expected one."
        let peer_a = PeerId::from_u128(0xa1);
        let peer_b = PeerId::from_u128(0xb2);
        let kp_a = PeerKeypair::generate();
        let kp_b = PeerKeypair::generate();
        let (cert_b, _) = generate_self_signed_cert(&kp_b, peer_b).unwrap();

        let registry = PeerKeyRegistry::new();
        registry.enrol(peer_a, 0, kp_a.public_bytes()).unwrap();
        registry.enrol(peer_b, 0, kp_b.public_bytes()).unwrap();
        let registry = Arc::new(registry);

        let verifier = PinnedServerVerifier::new(peer_a, registry);
        let server_name = ServerName::try_from("localhost").unwrap();
        let result = verifier.verify_server_cert(&cert_b, &[], &server_name, &[], unix_now());
        assert!(result.is_err());
        // Diagnostic message must call out the mismatch so debug
        // output is useful when this fires in production.
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("expected") || msg.contains("for peer"),
            "error message must mention expected vs actual peer: {msg}"
        );
    }

    #[test]
    fn server_verifier_rejects_unenrolled_cert() {
        // Cert is well-formed but its pubkey was never enrolled.
        // Fail-closed: no acceptance path for unknown identities.
        let peer = PeerId::from_u128(0xa1);
        let known_keypair = PeerKeypair::generate();
        let stranger_keypair = PeerKeypair::generate();
        let (stranger_cert, _) = generate_self_signed_cert(&stranger_keypair, peer).unwrap();

        let registry = make_registry_with(peer, &known_keypair);
        let verifier = PinnedServerVerifier::new(peer, registry);
        let server_name = ServerName::try_from("localhost").unwrap();
        let result =
            verifier.verify_server_cert(&stranger_cert, &[], &server_name, &[], unix_now());
        assert!(result.is_err());
    }

    #[test]
    fn server_verifier_rejects_malformed_cert() {
        // Garbage bytes → typed reject, not a panic.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let registry = make_registry_with(peer, &keypair);
        let verifier = PinnedServerVerifier::new(peer, registry);
        let server_name = ServerName::try_from("localhost").unwrap();
        let garbage = CertificateDer::from(vec![0u8; 16]);
        let result = verifier.verify_server_cert(&garbage, &[], &server_name, &[], unix_now());
        assert!(result.is_err());
    }

    #[test]
    fn client_verifier_accepts_any_enrolled_peer() {
        // Server accepts a client cert from peer A AND from peer B,
        // as long as both are enrolled. Server doesn't pin to one
        // specific peer; it pins to "any enrolled identity."
        let peer_a = PeerId::from_u128(0xa1);
        let peer_b = PeerId::from_u128(0xb2);
        let kp_a = PeerKeypair::generate();
        let kp_b = PeerKeypair::generate();
        let (cert_a, _) = generate_self_signed_cert(&kp_a, peer_a).unwrap();
        let (cert_b, _) = generate_self_signed_cert(&kp_b, peer_b).unwrap();

        let registry = PeerKeyRegistry::new();
        registry.enrol(peer_a, 0, kp_a.public_bytes()).unwrap();
        registry.enrol(peer_b, 0, kp_b.public_bytes()).unwrap();
        let registry = Arc::new(registry);

        let verifier = PinnedClientVerifier::new(registry);
        assert!(verifier
            .verify_client_cert(&cert_a, &[], unix_now())
            .is_ok());
        assert!(verifier
            .verify_client_cert(&cert_b, &[], unix_now())
            .is_ok());
    }

    #[test]
    fn client_verifier_rejects_unenrolled_peer() {
        // The same shape as the server-side unenrolled case. No
        // acceptance path for unknown clients.
        let known_peer = PeerId::from_u128(0xa1);
        let known_kp = PeerKeypair::generate();
        let stranger_kp = PeerKeypair::generate();
        let (stranger_cert, _) =
            generate_self_signed_cert(&stranger_kp, PeerId::from_u128(0xdeadbeef)).unwrap();

        let registry = make_registry_with(known_peer, &known_kp);
        let verifier = PinnedClientVerifier::new(registry);
        let result = verifier.verify_client_cert(&stranger_cert, &[], unix_now());
        assert!(result.is_err());
    }

    #[test]
    fn verifiers_only_accept_ed25519_scheme() {
        // Substrate is Ed25519-only. If rustls offers another scheme
        // (RSA, ECDSA), we tell it we can't verify — forces the
        // handshake to choose Ed25519 or fail. This is the substrate
        // posture: no algorithm-downgrade surface.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let registry = make_registry_with(peer, &keypair);

        let server_verifier = PinnedServerVerifier::new(peer, registry.clone());
        assert_eq!(
            server_verifier.supported_verify_schemes(),
            vec![SignatureScheme::ED25519]
        );

        let client_verifier = PinnedClientVerifier::new(registry);
        assert_eq!(
            client_verifier.supported_verify_schemes(),
            vec![SignatureScheme::ED25519]
        );
    }

    // NOTE: a direct unit test of `verify_tls12_signature` is
    // omitted because rustls 0.23 makes `DigitallySignedStruct::new`
    // private. The TLS 1.2 reject path is an unconditional `Err(...)`
    // — readable from source — and will be exercised end-to-end by
    // the integration tests once `LanTcpAdapter` lands in PR-3c
    // (real handshake with TLS 1.3 only, TLS 1.2 paths refused).
    #[test]
    fn supported_schemes_helper_returns_only_ed25519() {
        // The helper is what `supported_verify_schemes()` returns;
        // pinning it directly catches a regression where someone
        // adds a fallback scheme to the list.
        assert_eq!(supported_schemes(), vec![SignatureScheme::ED25519]);
    }
}
