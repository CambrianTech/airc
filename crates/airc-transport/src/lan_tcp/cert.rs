//! Cert generation + pubkey extraction for TLS peer-pinning.
//!
//! - `generate_self_signed_cert(keypair, peer_id)` — turns the
//!   substrate's `PeerKeypair` (Ed25519) into a self-signed X.509 cert
//!   suitable for rustls. The cert's subject CN carries the peer's
//!   UUID string. TLS proves possession of the key; the verifier checks
//!   that exact UUID/key pair against enrollment before binding a session.
//!
//! - `extract_ed25519_pubkey(cert_der)` — reverse direction, used by
//!   the pinning verifiers: take a received cert and return its
//!   Ed25519 subject pubkey for registry lookup.

use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use x509_parser::prelude::*;

use airc_core::PeerId;
use airc_protocol::PeerKeypair;

/// What can go wrong generating a self-signed cert from a `PeerKeypair`.
#[derive(Debug)]
pub enum CertGenError {
    /// ed25519-dalek couldn't serialize its key as PKCS#8 — should
    /// only fail for buggy key material; substrate-generated keys
    /// always serialize cleanly.
    Pkcs8(ed25519_dalek::pkcs8::Error),

    /// rcgen rejected the PKCS#8 input or failed to produce a cert.
    Rcgen(String),
}

impl std::fmt::Display for CertGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CertGenError::Pkcs8(error) => write!(f, "PKCS#8 serialize: {error}"),
            CertGenError::Rcgen(message) => write!(f, "rcgen: {message}"),
        }
    }
}

impl std::error::Error for CertGenError {}

/// What can go wrong extracting an Ed25519 pubkey from a received cert.
#[derive(Debug, PartialEq, Eq)]
pub enum CertParseError {
    /// The bytes didn't decode as a valid X.509 cert.
    InvalidCert(String),

    /// The cert's SubjectPublicKeyInfo algorithm isn't Ed25519
    /// (OID 1.3.101.112). Peer is using an unsupported key type;
    /// substrate refuses the handshake.
    WrongAlgorithm { oid: String },

    /// The subject pubkey field had the wrong length (expected 32
    /// bytes for Ed25519). Indicates a malformed cert.
    WrongPubkeyLength { got: usize },
}

impl std::fmt::Display for CertParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CertParseError::InvalidCert(message) => write!(f, "invalid X.509: {message}"),
            CertParseError::WrongAlgorithm { oid } => {
                write!(
                    f,
                    "cert subject pubkey is OID {oid}, expected Ed25519 (1.3.101.112)"
                )
            }
            CertParseError::WrongPubkeyLength { got } => {
                write!(f, "cert subject pubkey is {got} bytes, expected 32")
            }
        }
    }
}

impl std::error::Error for CertParseError {}

/// Generate a self-signed cert binding `peer_id` to the keypair's
/// Ed25519 pubkey. Returns the cert + private key in the formats
/// rustls expects (`CertificateDer` + `PrivateKeyDer`).
pub fn generate_self_signed_cert(
    keypair: &PeerKeypair,
    peer_id: PeerId,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), CertGenError> {
    // ed25519-dalek SigningKey → PKCS#8 DER → rcgen KeyPair. Same
    // bytes through three representations; the substrate identity
    // and TLS identity are guaranteed identical by construction.
    let signing = SigningKey::from_bytes(&keypair.secret_bytes());
    let pkcs8 = signing.to_pkcs8_der().map_err(CertGenError::Pkcs8)?;
    let pkcs8_bytes = pkcs8.as_bytes().to_vec();

    let rcgen_key = KeyPair::try_from(pkcs8_bytes.as_slice())
        .map_err(|error| CertGenError::Rcgen(error.to_string()))?;

    // Subject Common Name = durable peer UUID, authenticated by TLS CertificateVerify.
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, peer_id.to_string());

    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|error| CertGenError::Rcgen(error.to_string()))?;
    params.distinguished_name = dn;

    let cert = params
        .self_signed(&rcgen_key)
        .map_err(|error| CertGenError::Rcgen(error.to_string()))?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(pkcs8_bytes));

    Ok((cert_der, key_der))
}

/// Extract the Ed25519 subject pubkey from a received cert.
///
/// Used by both `PinnedServerVerifier` (server side, on receiving a
/// client cert) and `PinnedClientVerifier` (client side, on receiving
/// a server cert). Returns the 32-byte pubkey ready to pass to
/// `PeerKeyRegistry::find_peer` or `::lookup`.
pub fn extract_ed25519_pubkey(cert_der: &CertificateDer<'_>) -> Result<[u8; 32], CertParseError> {
    let (_, parsed) = X509Certificate::from_der(cert_der.as_ref())
        .map_err(|error| CertParseError::InvalidCert(error.to_string()))?;

    let spki = &parsed.subject_pki;
    // Ed25519 OID per RFC 8410.
    let oid = spki.algorithm.algorithm.to_id_string();
    if oid != "1.3.101.112" {
        return Err(CertParseError::WrongAlgorithm { oid });
    }

    let pubkey_bits = &spki.subject_public_key.data;
    if pubkey_bits.len() != 32 {
        return Err(CertParseError::WrongPubkeyLength {
            got: pubkey_bits.len(),
        });
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(pubkey_bits);
    Ok(out)
}

/// The peer claimed by this certificate. TLS CertificateVerify proves possession
/// of its key; callers must additionally check this exact ID/key pair is enrolled.
/// Never reverse-map a key: historical duplicate enrollments are not identity authority.
pub fn extract_peer_id(cert_der: &CertificateDer<'_>) -> Result<PeerId, CertParseError> {
    let (_, parsed) = X509Certificate::from_der(cert_der.as_ref())
        .map_err(|error| CertParseError::InvalidCert(error.to_string()))?;
    let mut names = parsed.subject().iter_common_name();
    let name = names
        .next()
        .ok_or_else(|| CertParseError::InvalidCert("missing peer UUID CN".into()))?;
    if names.next().is_some() {
        return Err(CertParseError::InvalidCert("multiple peer UUID CNs".into()));
    }
    let name = name
        .as_str()
        .map_err(|error| CertParseError::InvalidCert(error.to_string()))?;
    name.parse::<uuid::Uuid>()
        .map(PeerId::from_uuid)
        .map_err(|error| CertParseError::InvalidCert(format!("invalid peer UUID CN: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_protocol::PeerKeypair;

    #[test]
    fn multiple_identity_names_are_rejected_even_when_equal() {
        let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name = DistinguishedName::new();
        let peer = PeerId::from_u128(0xa1).to_string();
        params
            .distinguished_name
            .push(DnType::CommonName, peer.clone());
        params
            .distinguished_name
            .push(DnType::CustomDnType(vec![2, 5, 4, 3]), peer);
        let cert = params.self_signed(&key).unwrap();
        assert!(
            matches!(extract_peer_id(cert.der()), Err(CertParseError::InvalidCert(reason)) if reason == "multiple peer UUID CNs")
        );
    }
    #[test]
    fn generated_cert_carries_the_keypair_pubkey() {
        // The core invariant: the substrate's Ed25519 identity and the
        // TLS cert's subject pubkey are the SAME bytes. If they
        // diverge, peer-pinning becomes meaningless (you'd pin to one
        // key and verify TLS with another).
        let keypair = PeerKeypair::generate();
        let peer_id = PeerId::from_u128(0xa1);
        let (cert_der, _key_der) = generate_self_signed_cert(&keypair, peer_id).unwrap();
        let extracted = extract_ed25519_pubkey(&cert_der).unwrap();
        assert_eq!(extracted, keypair.public_bytes());
        assert_eq!(extract_peer_id(&cert_der).unwrap(), peer_id);
    }

    #[test]
    fn two_keypairs_produce_different_pubkeys_in_certs() {
        // Sanity: each peer's cert pins its own key, not some global
        // constant. Catches a hypothetical regression where the
        // generator hardcoded a test pubkey.
        let kp_a = PeerKeypair::generate();
        let kp_b = PeerKeypair::generate();
        let peer = PeerId::from_u128(0xa1);
        let (cert_a, _) = generate_self_signed_cert(&kp_a, peer).unwrap();
        let (cert_b, _) = generate_self_signed_cert(&kp_b, peer).unwrap();
        assert_ne!(
            extract_ed25519_pubkey(&cert_a).unwrap(),
            extract_ed25519_pubkey(&cert_b).unwrap()
        );
    }

    #[test]
    fn malformed_cert_bytes_return_invalid_cert_error() {
        // Garbage in → typed error out. The TLS verifier surfaces this
        // as a handshake rejection rather than panicking.
        let garbage = CertificateDer::from(vec![0u8; 16]);
        let result = extract_ed25519_pubkey(&garbage);
        assert!(matches!(result, Err(CertParseError::InvalidCert(_))));
        assert!(extract_peer_id(&garbage).is_err());
    }

    #[test]
    fn absent_or_malformed_identity_is_not_inferred_from_the_key() {
        for name in [None, Some("Fable"), Some("")] {
            let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
            let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
            params.distinguished_name = DistinguishedName::new();
            if let Some(name) = name {
                params.distinguished_name.push(DnType::CommonName, name);
            }
            let cert = params.self_signed(&key).unwrap();
            assert!(extract_peer_id(cert.der()).is_err());
        }
    }

    #[test]
    fn cert_round_trips_through_der_bytes() {
        // The cert produced for rustls must serialize cleanly and the
        // extracted pubkey survives. If rcgen ever changes its
        // encoding, this catches it.
        let keypair = PeerKeypair::generate();
        let peer_id = PeerId::from_u128(0xc0ffee);
        let (cert_der, _) = generate_self_signed_cert(&keypair, peer_id).unwrap();
        // Round trip via a Vec to simulate cert leaving + returning.
        let bytes: Vec<u8> = cert_der.as_ref().to_vec();
        let recoded = CertificateDer::from(bytes);
        assert_eq!(
            extract_ed25519_pubkey(&recoded).unwrap(),
            keypair.public_bytes()
        );
    }
}
