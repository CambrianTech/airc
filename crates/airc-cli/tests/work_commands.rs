//! End-to-end coverage for `airc-core work ...`.
//!
//! These tests execute the binary as a consumer would. The command
//! path must stay a thin wrapper over `airc-lib`, but the proof needs
//! to be from the CLI surface because that is what agents will use
//! while the daemon-attached API is still landing.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn work_create_claim_release_projects_on_board() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);

    let create = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "wire work commands through airc-lib",
            "--priority",
            "p1",
            "--body",
            "cli proof against the rust substrate",
        ],
    );
    let card_id = extract_field(&create, "card_id:").expect("create prints card_id");

    let board = run_ok(&home, &["work", "board"]);
    assert!(board.contains(card_id));
    assert!(board.contains("CambrianTech/airc"));
    assert!(board.contains("wire work commands through airc-lib"));
    assert!(board.contains("P1"));
    assert!(board.contains("Open"));

    let claim = run_ok(
        &home,
        &[
            "work",
            "claim",
            card_id,
            "--ttl-ms",
            "60000",
            "--no-lease-required",
        ],
    );
    let claim_id = extract_field(&claim, "claim_id:").expect("claim prints claim_id");

    let claimed_board = run_ok(&home, &["work", "board"]);
    assert!(claimed_board.contains(card_id));
    assert!(claimed_board.contains(claim_id));
    assert!(claimed_board.contains("Claimed"));

    let heartbeat = run_ok(
        &home,
        &["work", "heartbeat", card_id, claim_id, "--ttl-ms", "60000"],
    );
    assert!(heartbeat.contains("claim_heartbeat"));

    run_ok(
        &home,
        &[
            "work",
            "release",
            card_id,
            claim_id,
            "--reason",
            "merged into rust-rewrite",
        ],
    );

    let released_board = run_ok(&home, &["work", "board"]);
    assert!(released_board.contains(card_id));
    assert!(released_board.contains("Open"));
    assert!(released_board.contains("claim=-"));
}

#[test]
fn work_seed_is_idempotent_for_manager_candidates() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let first = run_ok(
        &home,
        &[
            "work",
            "seed",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "manager generated card",
            "--priority",
            "p1",
            "--body",
            "evidence from roadmap",
            "--evidence-key",
            "roadmap:manager-generated-card",
        ],
    );
    assert!(first.contains("outcome=created"), "{first}");
    let card_id = extract_seed_card_id(&first).expect("seed prints card id");

    let second = run_ok(
        &home,
        &[
            "work",
            "seed",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "manager   generated   card",
            "--priority",
            "p1",
            "--evidence-key",
            "roadmap:wording-changed",
        ],
    );
    assert!(second.contains("outcome=already_represented"), "{second}");
    assert!(second.contains(card_id), "{second}");

    let board = run_ok(&home, &["work", "board"]);
    assert_eq!(
        board.matches("manager generated card").count(),
        1,
        "{board}"
    );
}

#[test]
fn work_board_surfaces_stale_claims() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let create = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "do not let abandoned claims idle-lock a lane",
        ],
    );
    let card_id = extract_field(&create, "card_id:").expect("create prints card_id");

    let claim = run_ok(
        &home,
        &[
            "work",
            "claim",
            card_id,
            "--ttl-ms",
            "1",
            "--no-lease-required",
        ],
    );
    let claim_id = extract_field(&claim, "claim_id:").expect("claim prints claim_id");
    std::thread::sleep(std::time::Duration::from_millis(5));

    let stale_board = run_ok(&home, &["work", "board"]);
    assert!(stale_board.contains("stale claims: 1"), "{stale_board}");
    assert!(stale_board.contains(card_id), "{stale_board}");
    assert!(stale_board.contains(claim_id), "{stale_board}");
}

#[test]
fn work_availability_projects_to_board() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let availability = run_ok(
        &home,
        &[
            "work",
            "availability",
            "--repo",
            "CambrianTech/airc",
            "--state",
            "ready",
            "--note",
            "can take review",
            "--ttl-ms",
            "60000",
        ],
    );
    assert!(
        availability.contains("agent_availability"),
        "{availability}"
    );

    let board = run_ok(&home, &["work", "board"]);
    assert!(board.contains("agent availability: 1"), "{board}");
    assert!(board.contains("CambrianTech/airc"), "{board}");
    assert!(board.contains("state=Ready"), "{board}");
    assert!(board.contains("stale=false"), "{board}");
    assert!(board.contains("can take review"), "{board}");
}

#[test]
fn work_next_surfaces_availability_and_idle_guidance() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(
        &home,
        &[
            "work",
            "availability",
            "--repo",
            "CambrianTech/airc",
            "--state",
            "ready",
            "--note",
            "available for P0",
            "--ttl-ms",
            "60000",
        ],
    );

    let next = run_ok(&home, &["work", "next", "--event-limit", "128"]);
    assert!(next.contains("(no claimable work)"), "{next}");
    assert!(
        next.contains("agent availability: ready=1 busy=0 away=0 stale=0"),
        "{next}"
    );
    assert!(next.contains("available for P0"), "{next}");
}

#[test]
fn work_roster_surfaces_availability_and_claims() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(
        &home,
        &[
            "work",
            "availability",
            "--repo",
            "CambrianTech/airc",
            "--state",
            "ready",
            "--note",
            "ready for roster work",
            "--ttl-ms",
            "60000",
        ],
    );
    let create = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "show who is doing what",
            "--priority",
            "p1",
        ],
    );
    let card_id = extract_field(&create, "card_id:").expect("card id");
    run_ok(
        &home,
        &[
            "work",
            "claim",
            card_id,
            "--ttl-ms",
            "60000",
            "--no-lease-required",
        ],
    );

    let roster = run_ok(&home, &["work", "roster", "--event-limit", "128"]);
    assert!(roster.contains("work roster: 1 agent(s)"), "{roster}");
    assert!(roster.contains("ready=1"), "{roster}");
    assert!(roster.contains("availability=Ready"), "{roster}");
    assert!(roster.contains("ready for roster work"), "{roster}");
    assert!(roster.contains("claims=1"), "{roster}");
    assert!(roster.contains(card_id), "{roster}");
    assert!(roster.contains("show who is doing what"), "{roster}");
}

#[test]
fn work_next_suggests_claimable_priority_cards() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let p0 = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "claimable p0",
            "--priority",
            "p0",
        ],
    );
    let p0_id = extract_field(&p0, "card_id:").expect("p0 card id");
    let p2 = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "lower p2",
            "--priority",
            "p2",
        ],
    );
    let p2_id = extract_field(&p2, "card_id:").expect("p2 card id");

    let next = run_ok(&home, &["work", "next", "--event-limit", "128"]);
    assert!(next.contains("claimable work: 1"), "{next}");
    assert!(next.contains(p0_id), "{next}");
    assert!(next.contains("claimable p0"), "{next}");
    assert!(!next.contains(p2_id), "{next}");
}

#[test]
fn work_close_removes_card_from_claimable_next() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let create = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "done work should not stay claimable",
            "--priority",
            "p0",
        ],
    );
    let card_id = extract_field(&create, "card_id:").expect("card id");

    let close = run_ok(&home, &["work", "close", card_id]);
    assert!(close.contains("card_state_changed"), "{close}");
    assert!(close.contains("Closed"), "{close}");

    let board = run_ok(&home, &["work", "board"]);
    assert!(board.contains(card_id), "{board}");
    assert!(board.contains("Closed"), "{board}");

    let next = run_ok(&home, &["work", "next", "--event-limit", "128"]);
    assert!(!next.contains(card_id), "{next}");
}

#[test]
fn lane_create_status_and_state_drive_work_projection() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);

    let lane = run_ok(
        &home,
        &[
            "lane",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "rust lane commands",
            "--state",
            "planned",
        ],
    );
    let lane_id = extract_field(&lane, "lane_id:").expect("lane create prints lane_id");

    let status = run_ok(&home, &["lane", "status"]);
    assert!(status.contains(lane_id));
    assert!(status.contains("Planned"));
    assert!(status.contains("cards=0"));

    run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "card inside lane",
            "--lane-id",
            lane_id,
        ],
    );

    let status = run_ok(&home, &["lane", "status"]);
    assert!(status.contains("cards=1"));

    run_ok(&home, &["lane", "state", lane_id, "active"]);
    let status = run_ok(&home, &["lane", "status"]);
    assert!(status.contains("Active"));
}

#[test]
fn lane_manager_claim_status_and_release_project_from_board() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let claim = run_ok(
        &home,
        &[
            "lane",
            "manager",
            "claim",
            "--repo",
            "CambrianTech/airc",
            "--ttl-ms",
            "60000",
        ],
    );
    assert!(claim.contains("manager_hat_claimed"));

    let status = run_ok(&home, &["lane", "manager", "status"]);
    assert!(status.contains("manager hats: 1"));
    assert!(status.contains("CambrianTech/airc"));
    assert!(status.contains("manager="));
    assert!(status.contains("expires_at_ms="));

    let release = run_ok(
        &home,
        &["lane", "manager", "release", "--repo", "CambrianTech/airc"],
    );
    assert!(release.contains("manager_hat_released"));

    let status = run_ok(&home, &["lane", "manager", "status"]);
    assert!(status.contains("(no manager hats)"));
}

#[test]
fn work_claim_refuses_when_cwd_outside_lease_zone() {
    // Closes flaw #7 from GRID-SUBSTRATE-AUDIT #964: the CLI must
    // refuse `work claim` from outside `~/.airc/worktrees/`. The
    // test fixture's $HOME is a tempdir, and cwd is the source tree
    // (definitely not inside that tempdir's lease zone), so the
    // bare claim should fail with a clear refusal.
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let create = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "refusal test",
            "--priority",
            "p2",
        ],
    );
    let card_id = extract_field(&create, "card_id:").expect("card id");

    let stderr = run_expect_failure(&home, &["work", "claim", card_id, "--ttl-ms", "60000"]);
    assert!(
        stderr.contains("not under lease zone"),
        "expected lease-refusal stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("--no-lease-required"),
        "refusal must point at the override flag, got: {stderr}"
    );

    // With the override the same claim succeeds.
    let claim = run_ok(
        &home,
        &[
            "work",
            "claim",
            card_id,
            "--ttl-ms",
            "60000",
            "--no-lease-required",
        ],
    );
    assert!(
        extract_field(&claim, "claim_id:").is_some(),
        "claim with override should print a claim_id, got: {claim}"
    );
}

#[test]
fn work_board_empty_state_is_explicit() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let board = run_ok(&home, &["work", "board"]);

    assert!(
        board.contains("(no work cards)"),
        "empty board should be explicit, got: {board}"
    );
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let machine_home = home.parent().unwrap_or(home);
    let output = Command::new(airc_core())
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

fn run_expect_failure(home: &Path, args: &[&str]) -> String {
    let machine_home = home.parent().unwrap_or(home);
    let output = Command::new(airc_core())
        .env("HOME", machine_home)
        .env("USERPROFILE", machine_home)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        !output.status.success(),
        "expected airc-core {:?} to fail, but it succeeded: stdout={}",
        args,
        String::from_utf8_lossy(&output.stdout),
    );
    String::from_utf8(output.stderr).expect("stderr utf-8")
}

fn extract_field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
}

fn extract_seed_card_id(text: &str) -> Option<&str> {
    text.split_whitespace()
        .find_map(|field| field.strip_prefix("card_id="))
}
