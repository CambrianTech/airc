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

use std::path::Path;
use std::process::Command;

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
    let output = Command::new(airc_core())
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
    let output = Command::new(airc_core())
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
    let output = Command::new(airc_core())
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

// ====================================================================
// V1 — `airc peer add` defaults to Untrusted; `--tier=…` overrides
// ====================================================================

#[test]
fn peer_add_without_tier_flag_defaults_to_untrusted() {
    // Card 34942ec1 Sub-A contract: a fresh peer has tier=Untrusted
    // until something explicitly promotes it. Sub-C must not change
    // that default — existing scripts/sessions stay correct.
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);
    let spec = mint_peer_spec("seed-1");

    run_ok(&home, &["peer", "add", &spec]);
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let peers = parsed.as_array().expect("list returns an array");
    assert_eq!(peers.len(), 1);
    assert_eq!(
        peers[0]["tier"].as_str(),
        Some("untrusted"),
        "Sub-A default contract: no --tier flag means Untrusted"
    );
}

#[test]
fn peer_add_with_tier_flag_persists_explicit_tier() {
    // V1 explicit arm: --tier=friend lands the row at Friend, not
    // the default. This is what Joel + Toby will use to pin trust
    // on each other's machine accounts.
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);
    let spec = mint_peer_spec("seed-2");

    run_ok(&home, &["peer", "add", &spec, "--tier", "friend"]);
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    assert_eq!(
        parsed[0]["tier"].as_str(),
        Some("friend"),
        "--tier flag overrides the default"
    );
}

#[test]
fn peer_add_accepts_every_trust_tier_variant() {
    // ALL_VARIANTS round-trip discipline — every tier the substrate
    // declares must be reachable from the CLI. Forgetting to wire
    // a new variant into the clap value_enum would silently fall
    // through to "default-Untrusted" without this test.
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);

    let tiers = [
        ("own_machine", 0xa1),
        ("own_account", 0xa2),
        ("friend", 0xa3),
        ("untrusted", 0xa4),
    ];
    for (tier_str, peer_seed) in tiers {
        let spec = mint_peer_spec(&format!("tier-{peer_seed:x}"));
        run_ok(&home, &["peer", "add", &spec, "--tier", tier_str]);
    }
    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let observed: std::collections::HashSet<String> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["tier"].as_str().unwrap().to_string())
        .collect();
    let expected: std::collections::HashSet<String> =
        tiers.iter().map(|(t, _)| t.to_string()).collect();
    assert_eq!(observed, expected, "every declared tier must round-trip");
}

// ====================================================================
// V2 — `airc peer set-tier` updates an existing peer
// ====================================================================

#[test]
fn peer_set_tier_updates_existing_peer() {
    // V2 happy path. Sub-B shipped the store-side
    // set_peer_trust_tier; Sub-C is the CLI surface that consumers
    // (Joel manually pinning Friend on Toby) reach for.
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);
    let spec = mint_peer_spec("seed-3");
    let peer_id = peer_id_of(&spec).to_string();

    // Start at default Untrusted, then promote.
    run_ok(&home, &["peer", "add", &spec]);
    let promoted = run_ok(&home, &["peer", "set-tier", &peer_id, "friend"]);
    assert!(
        promoted.contains("untrusted") && promoted.contains("friend"),
        "set-tier output should name both the old and new tier so \
         the operator can audit the change: {promoted}"
    );

    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    assert_eq!(
        parsed[0]["tier"].as_str(),
        Some("friend"),
        "set-tier must persist to the trust store"
    );
}

// ====================================================================
// V3 — `set-tier` refuses unknown peer
// ====================================================================

#[test]
fn peer_set_tier_refuses_unknown_peer() {
    // V3: no implicit add. The substrate-side store layer returns
    // Ok(None) when the peer is missing; the CLI must surface this
    // as a non-zero exit with an explanatory error — silently doing
    // nothing would be misleading ("did it work? did it not?").
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);

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
}

// ====================================================================
// V6 — `set-tier` is idempotent
// ====================================================================

#[test]
fn peer_set_tier_is_idempotent_when_already_at_target() {
    // V6: setting to the current tier returns Ok with a no-op
    // marker. Sub-B's set_peer_trust_tier already shipped this at
    // the store layer; Sub-C must not crash or claim a change
    // happened when nothing did.
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);
    let spec = mint_peer_spec("seed-4");
    let peer_id = peer_id_of(&spec).to_string();

    run_ok(&home, &["peer", "add", &spec, "--tier", "friend"]);
    let same = run_ok(&home, &["peer", "set-tier", &peer_id, "friend"]);
    assert!(
        same.contains("no change") || same.contains("already") || same.contains("idempotent"),
        "idempotent path should be honest about doing nothing: {same}"
    );
}

// ====================================================================
// V4 — `peer list --json` exposes the tier field
// ====================================================================

#[test]
fn peer_list_json_exposes_tier_field() {
    // V4: consumers (bridge daemon, continuum router) read the tier
    // off this output to build their routing tables. Without
    // --json, the substrate forces them to scrape human-format
    // text — guaranteed to break. JSON shape is the contract.
    let ws = common::daemon_tempdir();
    let home = ws.path().join("agent");
    run_ok(&home, &["init"]);
    let spec_friend = mint_peer_spec("seed-5");
    let spec_untrusted = mint_peer_spec("seed-6");
    run_ok(&home, &["peer", "add", &spec_friend, "--tier", "friend"]);
    run_ok(&home, &["peer", "add", &spec_untrusted]);

    let list = run_ok(&home, &["peer", "list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&list).expect("peer list --json parses");
    let peers = parsed.as_array().expect("array");
    assert_eq!(peers.len(), 2);
    for peer in peers {
        assert!(
            peer["peer_id"].is_string(),
            "peer entry must carry peer_id: {peer}"
        );
        assert!(
            peer["pubkey_b64"].is_string(),
            "peer entry must carry pubkey_b64: {peer}"
        );
        assert!(
            peer["tier"].is_string(),
            "peer entry must carry tier: {peer}"
        );
        let tier = peer["tier"].as_str().unwrap();
        assert!(
            matches!(tier, "own_machine" | "own_account" | "friend" | "untrusted"),
            "tier must be one of the four declared variants: {tier}"
        );
    }
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
