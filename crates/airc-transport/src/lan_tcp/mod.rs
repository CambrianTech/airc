//! Secure LAN transport — TLS-wrapped TCP with peer-pinned Ed25519
//! identities.
//!
//! AI peers on the same LAN talk via TLS 1.2/1.3 over plain TCP. There
//! is **no plaintext path**: the only way to use this transport is with
//! mutual TLS where both peers present self-signed X.509 certs bound
//! to the substrate's Ed25519 identity keys. Receivers pin by Ed25519
//! pubkey via the substrate's `PeerKeyRegistry` — no CA, no PKI bridge,
//! no chain-of-trust. The cert IS the identity assertion; the registry
//! IS the trust anchor.
//!
//! Failure modes are fail-closed:
//!   - Receiving a cert whose Ed25519 pubkey isn't enrolled → reject.
//!   - Receiving a cert from a peer different from the expected one
//!     (client side, where the caller knows who they're dialing) →
//!     reject.
//!   - Cert decode failure / wrong SPKI algorithm → reject.
//!
//! Module layout:
//!   - `cert` — bidirectional cert ↔ Ed25519 pubkey bridge
//!   - `verifier` — rustls verifiers pinned to enrolled keys
//!   - `tls_config` — composes cert + verifier into rustls configs
//!   - `adapter` — `LanTcpAdapter` (the `Transport` impl)

pub mod adapter;
pub mod cert;
pub mod tls_config;
pub mod verifier;

pub use adapter::{LanTcpAdapter, LanTcpError};
pub use cert::{extract_ed25519_pubkey, generate_self_signed_cert, CertGenError, CertParseError};
pub use tls_config::{build_client_config, build_server_config, TlsConfigError};
pub use verifier::{PinnedClientVerifier, PinnedServerVerifier};
