//! Card 34942ec1 Sub-C — `airc peer add --tier=…` + `airc peer
//! set-tier` integration tests.
//!
//! Written FIRST per Joel's TDD/VDD directive: these tests pin the
//! validation criteria (V1-V7 in the card) before the implementation
//! lands, so the surface is locked at the desired shape and the
//! implementation has to satisfy the validation we've already agreed
//! on.
//!
//! Failure mode the suite is shaped to catch:
//!   - Sub-A's default-Untrusted contract is preserved (V1 default arm)
//!   - --tier= takes the explicit override (V1 explicit arm)
//!   - set-tier on unknown peer surfaces a useful error (V3)
//!   - set-tier is idempotent (V6)
//!   - list --json surfaces tier so consumers can route on it (V4)
//!   - the security invariant from Sub-A's
//!     `replace_peer_trust_preserves_existing_tier` survives a
//!     set-tier path (V5)

use airc_core::process::background;
use std::path::Path;

mod common;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// Sub-C tests need a peer with a real-shape Ed25519 pubkey because
/// `airc peer add` crypto-verifies decompression. Mint by initialising
/// a throwaway scope, harvesting its peer_spec line, and discarding the
/// scope. Each call returns a distinct spec — keeps the tests honest
/// about working with real key material.
fn mint_peer_spec(seed: &str) -> String {
    let probe = common::daemon_tempdir();
    let probe_home = probe.path().join(seed);
    let output = background(airc_core())
        .arg("--home")
        .arg(&probe_home)
        .arg("init")
        .env("HOME", probe.path())
        .env("USERPROFILE", probe.path())
        .output()
        .expect("airc init must spawn for spec mint");
    assert!(
        output.status.success(),
        "init failed for mint_peer_spec({seed}): {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("init stdout utf-8");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("peer_spec:").map(str::trim))
        .expect("init prints peer_spec line")
        .to_string()
}

/// Extract the peer_id (UUID) prefix from a peer_spec string.
fn peer_id_of(spec: &str) -> &str {
    spec.split(':').next().expect("spec has uuid prefix")
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    // Card 303f2384: --no-lease-required gate needs HOME pointing at
    // a scope-owner. Sub-C's peer commands inherit the same harness.
    let machine_home = home.parent().unwrap_or(home);
    let output = background(airc_core())
        .current_dir(machine_home)
        .env("HOME", machine_home)
        .env("USERPROFILE", machine_home)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn run_expect_failure(home: &Path, args: &[&str]) -> (String, String) {
    let machine_home = home.parent().unwrap_or(home);
    let output = background(airc_core())
        .current_dir(machine_home)
        .env("HOME", machine_home)
        .env("USERPROFILE", machine_home)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        !output.status.success(),
        "airc-core {:?} unexpectedly succeeded: stdout={}",
        args,
        String::from_utf8_lossy(&output.stdout),
    );
    (
        String::from_utf8(output.stdout).expect("stdout utf-8"),
        String::from_utf8(output.stderr).expect("stderr utf-8"),
    )
}

// One isolated account owns this compatible CLI lifecycle. Fresh initialization,
// daemon restart and key-rotation behavior remain in their dedicated suites.
// What this catches: default/explicit enrollment, all tier variants, refusal,
// promotion, idempotence and JSON shape must agree through the public commands.
#[test]
fn peer_trust_lifecycle_preserves_cli_contracts() {
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);

    // V3: an unknown peer is refused before enrollment; do not implicitly add it.
    let ghost = uuid::Uuid::from_u128(0xc0c0_a0a0).to_string();
    let (_stdout, stderr) = run_expect_failure(&home, &["peer", "set-tier", &ghost, "own_machine"]);
    assert!(
        stderr.contains("not enrolled") || stderr.contains("not enroled"),
        "refusal must name the cause: {stderr}"
    );
    assert!(
        stderr.contains("peer add"),
        "refusal must point at the corrective command (peer add): {stderr}"
    );

    // V1 default: a fresh enrollment is Untrusted, and the failed command above
    // has not created a ghost row.
    let default_spec = mint_peer_spec("default");
    run_ok(&home, &["peer", "add", &default_spec]);
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let peers = parsed.as_array().expect("list returns an array");
    assert_eq!(peers.len(), 1, "default enrollment is the only peer");
    assert_eq!(peers[0]["tier"].as_str(), Some("untrusted"));

    // V1 explicit + V4: both explicit Friend and default Untrusted have the
    // consumer JSON shape, with their exact tiers preserved.
    let friend_spec = mint_peer_spec("friend");
    run_ok(&home, &["peer", "add", &friend_spec, "--tier", "friend"]);
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let peers = parsed.as_array().expect("array");
    assert_eq!(peers.len(), 2);
    for peer in peers {
        assert!(peer["peer_id"].is_string(), "missing peer_id: {peer}");
        assert!(peer["pubkey_b64"].is_string(), "missing pubkey_b64: {peer}");
        assert!(peer["tier"].is_string(), "missing tier: {peer}");
        let id = peer["peer_id"].as_str().unwrap();
        let expected = if id == peer_id_of(&friend_spec) {
            "friend"
        } else {
            assert_eq!(id, peer_id_of(&default_spec));
            "untrusted"
        };
        assert_eq!(peer["tier"].as_str(), Some(expected), "peer: {peer}");
    }

    // V2: promotion reports both states and persists to that peer's trust row.
    let promoted = run_ok(
        &home,
        &["peer", "set-tier", peer_id_of(&default_spec), "friend"],
    );
    assert!(
        promoted.contains("untrusted") && promoted.contains("friend"),
        "set-tier must report the old and new tier: {promoted}"
    );
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let promoted_peer = parsed
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["peer_id"].as_str() == Some(peer_id_of(&default_spec)))
        .expect("promoted peer remains enrolled");
    assert_eq!(promoted_peer["tier"].as_str(), Some("friend"));

    // V6: setting the explicit Friend peer to its current tier is an honest no-op.
    let same = run_ok(
        &home,
        &["peer", "set-tier", peer_id_of(&friend_spec), "friend"],
    );
    assert!(
        same.contains("no change") || same.contains("already") || same.contains("idempotent"),
        "idempotent path should report no change: {same}"
    );

    // V1 all variants: Friend was explicitly enrolled above. Each remaining
    // variant gets a distinct real key and a fresh enrollment through --tier.
    for tier in ["own_machine", "own_account", "untrusted"] {
        let spec = mint_peer_spec(tier);
        run_ok(&home, &["peer", "add", &spec, "--tier", tier]);
    }
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let peers = parsed.as_array().expect("array");
    assert_eq!(
        peers.len(),
        5,
        "all five distinct enrollments remain present"
    );
    let observed: std::collections::HashSet<&str> = peers
        .iter()
        .map(|peer| peer["tier"].as_str().expect("tier is a string"))
        .collect();
    let expected: std::collections::HashSet<&str> =
        ["own_machine", "own_account", "friend", "untrusted"]
            .into_iter()
            .collect();
    assert_eq!(observed, expected, "every declared tier must round-trip");
}

// ====================================================================
// V5 — set-tier interacts cleanly with the rotation invariant
// ====================================================================
//
// Sub-A pinned `replace_peer_trust_preserves_existing_tier`. Sub-C
// adds a path that explicitly sets the tier; the invariant must
// still hold across the new path — i.e. if I `set-tier friend`
// then a rotation lands, the tier stays Friend, not the rotate
// path's "preserve whatever was there" which could have raced.
//
// This is a substrate-layer invariant that the CLI can't directly
// exercise without a real rotation, but Sub-B's
// `replace_peer_trust_preserves_existing_tier` already pins it. We
// note the cross-reference here so the CLI test file documents the
// invariant Sub-C relies on, even though the assertion lives in
// the airc-store crate.
