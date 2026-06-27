//! Consumer-facing identity-assertion round-trip (the airc analogue of
//! a WebAuthn assertion): a participant signs a domain-separated
//! assertion via the airc-lib SDK, and a relying party verifies it from
//! nothing but the peer-spec pubkey. This is the credential primitive
//! Continuum / jtag / browser / server clients build session tokens and
//! Forge-alloy contract-step signatures on top of — later backed by a
//! Secure-Enclave signer for hardware attestation.

use airc_lib::{Airc, PeerSpec};
use tempfile::TempDir;

#[tokio::test]
async fn sdk_signs_and_verifies_a_domain_separated_assertion() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    // The challenge is whatever the relying party binds: a server nonce,
    // a session descriptor, or a Forge-alloy Merkle context/contract
    // root + nonce. airc neither parses nor cares — it signs the bytes.
    let challenge = b"forge.alloy.contract-root::nonce-7";
    let assertion = airc.sign_assertion("continuum.session", challenge);

    // A relying party holding only the peer-spec pubkey can verify —
    // no shared secret, proof-of-possession like a WebAuthn assertion.
    let spec: PeerSpec = airc.peer_spec().parse().unwrap();
    assert!(assertion.verify_with_pubkey(&spec.pubkey).is_ok());
    assert_eq!(assertion.peer_id, airc.peer_id());
    assert_eq!(assertion.context, "continuum.session");

    // Replay/tamper fails closed: the same signature must not verify
    // against a different challenge (different chain position / nonce).
    let mut replayed = assertion.clone();
    replayed.challenge = b"forge.alloy.contract-root::nonce-8".to_vec();
    assert!(replayed.verify_with_pubkey(&spec.pubkey).is_err());

    // Domain binding fails closed: a different RP/"type" context must
    // not verify either (WebAuthn clientDataJSON.type binding).
    let mut wrong_ctx = assertion.clone();
    wrong_ctx.context = "some.other.rp".to_string();
    assert!(wrong_ctx.verify_with_pubkey(&spec.pubkey).is_err());
}
