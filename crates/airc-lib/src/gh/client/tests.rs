use super::*;

/// what this catches (#356): the two `GhClient` impls read the SAME
/// issue from DIFFERENT APIs — gh's CLI is GraphQL-backed and
/// reports `OPEN`/`CLOSED`, REST reports `open`/`closed`. The
/// closer's idempotence check ("already closed? skip") would then
/// silently depend on which backend was configured. The trait's
/// contract is the uppercase form; this pins it for both shapes.
#[test]
fn issue_view_state_is_normalized_uppercase_across_backends() {
    let rest = parse_issue_view(br#"{"number":7,"title":"t","body":"b","state":"closed"}"#)
        .expect("rest shape parses");
    let cli = parse_issue_view(br#"{"number":7,"title":"t","body":"b","state":"CLOSED"}"#)
        .expect("cli shape parses");
    assert_eq!(rest.state, "CLOSED");
    assert_eq!(cli.state, "CLOSED");
    assert_eq!(rest, cli, "both backends must yield an identical IssueView");
}

/// what this catches (#356): REST sends `"body": null` for an issue
/// with no description, and a bare `#[serde(default)]` covers a
/// MISSING key but still errors on an explicit null. That would
/// abort the whole close-merged run on the most ordinary input
/// there is — an issue that simply has no body (hence no
/// queue-card envelope, hence a card the closer should SKIP).
#[test]
fn issue_view_tolerates_null_and_missing_body() {
    let null_body = parse_issue_view(br#"{"number":1,"title":"t","body":null,"state":"open"}"#)
        .expect("null body must parse, not abort the run");
    assert_eq!(null_body.body, "");

    let missing_body = parse_issue_view(br#"{"number":1,"title":"t","state":"open"}"#)
        .expect("missing body parses");
    assert_eq!(missing_body.body, "");
}

#[test]
fn parse_pr_view_decodes_all_fields() {
    let json = br#"{
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"name": "fmt",     "status": "COMPLETED",  "conclusion": "SUCCESS"},
                {"name": "tests",   "status": "IN_PROGRESS", "conclusion": null},
                {"name": "clippy",  "status": "COMPLETED",  "conclusion": "FAILURE"}
            ]
        }"#;
    let view = parse_pr_view(json).expect("parse");
    assert_eq!(view.state, "OPEN");
    assert_eq!(view.mergeable, "MERGEABLE");
    let checks = view.status_check_rollup.unwrap();
    assert_eq!(checks.len(), 3);
    assert_eq!(
        checks[1].conclusion, None,
        "in-flight check has null conclusion"
    );
}

#[test]
fn parse_pr_view_tolerates_missing_optional_fields() {
    let json = br#"{}"#;
    let view = parse_pr_view(json).expect("empty object decodes");
    assert!(view.state.is_empty());
    assert!(view.mergeable.is_empty());
    assert!(view.status_check_rollup.is_none());
    assert!(view.base_ref_name.is_none());
    assert!(view.base_ref_oid.is_none());
}

#[test]
fn parse_pr_url_extracts_last_url_and_number() {
    let stdout = "Creating draft pull request for refs/heads/feat/x into rust-rewrite\n\
                      https://github.com/CambrianTech/airc/pull/1038\n";
    let created = parse_pr_url(stdout).expect("parse");
    assert_eq!(created.number, 1038);
    assert_eq!(
        created.url,
        "https://github.com/CambrianTech/airc/pull/1038"
    );
}

#[test]
fn parse_pr_url_rejects_unparseable_tail() {
    let stdout = "https://github.com/owner/repo/pull/not-a-number\n";
    assert!(matches!(parse_pr_url(stdout), Err(GhError::OutputParse(_))));
}

#[test]
fn parse_pr_url_rejects_empty_output() {
    assert!(matches!(parse_pr_url(""), Err(GhError::OutputParse(_))));
    assert!(matches!(
        parse_pr_url("\n\n  \n"),
        Err(GhError::OutputParse(_))
    ));
}

// -------------------------------------------------------------------
// Card 7ed1ac4f — ISO timestamp parser for the pending-too-long gate
// -------------------------------------------------------------------

#[test]
fn parse_iso_timestamp_ms_matches_known_anchors() {
    // 1970-01-01T00:00:00Z is by definition 0 ms since epoch.
    assert_eq!(parse_iso_timestamp_ms("1970-01-01T00:00:00Z"), Some(0));
    // 1970-01-01T00:00:01Z is 1 second later.
    assert_eq!(parse_iso_timestamp_ms("1970-01-01T00:00:01Z"), Some(1_000));
    // 2000-01-01T00:00:00Z is the well-known 946_684_800 epoch
    // seconds — Y2K anchor every date library agrees on. Pin
    // that the hand-rolled civil-calendar math lines up.
    assert_eq!(
        parse_iso_timestamp_ms("2000-01-01T00:00:00Z"),
        Some(946_684_800_000)
    );
    // 2026-05-29T03:29:44Z — from a real gh statusCheckRollup
    // sample observed this session. 1_780_025_384 seconds.
    assert_eq!(
        parse_iso_timestamp_ms("2026-05-29T03:29:44Z"),
        Some(1_780_025_384_000)
    );
}

#[test]
fn parse_iso_timestamp_ms_rejects_malformed() {
    // Missing trailing Z — gh always emits UTC; refusing a
    // local-time variant is intentional (don't guess the offset).
    assert_eq!(parse_iso_timestamp_ms("2026-05-29T03:29:44"), None);
    // Wrong length.
    assert_eq!(parse_iso_timestamp_ms(""), None);
    assert_eq!(parse_iso_timestamp_ms("2026-05-29"), None);
    // Wrong separator.
    assert_eq!(parse_iso_timestamp_ms("2026-05-29 03:29:44Z"), None);
    // Out-of-range month / day / hour. These would silently
    // round in a permissive parser, which would corrupt the
    // pending-too-long check.
    assert_eq!(parse_iso_timestamp_ms("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_iso_timestamp_ms("2026-01-32T00:00:00Z"), None);
    assert_eq!(parse_iso_timestamp_ms("2026-01-01T24:00:00Z"), None);
    // Non-numeric.
    assert_eq!(parse_iso_timestamp_ms("XXXX-XX-XXTXX:XX:XXZ"), None);
}

#[test]
fn gh_check_deserializes_started_at_from_pr_view_shape() {
    // gh pr view --json statusCheckRollup uses camelCase. Pin
    // that the serde rename catches it — a regression would
    // silently leave started_at = None on every PR-side check
    // and the pending-too-long policy would degenerate to "no
    // timestamps known, never time out anything."
    let json = serde_json::json!({
        "conclusion": null,
        "status": "IN_PROGRESS",
        "name": "cargo test (windows-latest)",
        "startedAt": "2026-05-29T03:29:46Z",
    });
    let parsed: GhCheck = serde_json::from_value(json).expect("PR-shape decodes");
    assert_eq!(parsed.started_at.as_deref(), Some("2026-05-29T03:29:46Z"));
    assert_eq!(parsed.status.as_deref(), Some("IN_PROGRESS"));
}

// what this catches: REST check-runs are lowercase (`success`/`completed`)
// and the merge gate matches GraphQL's `SUCCESS`/`COMPLETED`; passed
// through verbatim, five green checks read as "5 check(s) still
// running" (card c03bb62f, 2026-09-04: continuum #3693/#3696/#3699
// all-green on GitHub, `airc work merge` refusing). GhCheck must be
// GraphQL-cased regardless of which client produced it.
#[test]
fn parse_check_runs_normalizes_rest_lowercase_to_graphql_case() {
    let json = br#"{"total_count":2,"check_runs":[
            {"id":1,"name":"cargo test","status":"completed","conclusion":"success","started_at":"2026-09-04T17:36:00Z"},
            {"id":2,"name":"cargo check","status":"in_progress","conclusion":null}
        ]}"#;
    let runs = parse_check_runs(json).expect("parse");
    assert_eq!(runs[0].status.as_deref(), Some("COMPLETED"));
    assert_eq!(runs[0].conclusion.as_deref(), Some("SUCCESS"));
    assert_eq!(runs[1].status.as_deref(), Some("IN_PROGRESS"));
    assert_eq!(runs[1].conclusion, None);
}

// what this catches: the dialect boundary must make the two
// constructors interchangeable — a merged PR is `closed` + merged_at in
// REST and `MERGED` in GraphQL, and checks are lowercase in REST. Both
// must land on the canonical vocabulary the merge gate matches, or the
// gate silently misreads (card c03bb62f: merged PRs left cards at
// Review; green checks read as in-flight). Card fc483e57.
#[test]
fn from_rest_and_from_graphql_agree_on_the_canonical_vocabulary() {
    let rest_pr = serde_json::json!({
        "state": "closed",
        "mergeable": true,
        "merged_at": "2026-09-04T18:01:58Z",
        "base": {"ref": "release/topic", "sha": "0123456789abcdef0123456789abcdef01234567"}
    });
    let rest_checks = parse_check_runs(
            br#"{"total_count":1,"check_runs":[{"id":1,"name":"cargo test","status":"completed","conclusion":"success"}]}"#,
        )
        .expect("rest checks parse");
    let from_rest = PrView::from_rest(&rest_pr, rest_checks);

    let from_graphql = PrView::from_graphql(
            br#"{"state":"MERGED","mergeable":"MERGEABLE","mergedAt":"2026-09-04T18:01:58Z",
                 "baseRefName":"release/topic","baseRefOid":"0123456789abcdef0123456789abcdef01234567",
                 "statusCheckRollup":[{"name":"cargo test","status":"COMPLETED","conclusion":"SUCCESS"}]}"#,
        )
        .expect("graphql parses");

    assert_eq!(from_rest, from_graphql, "the dialects must converge");
    assert_eq!(from_rest.state, "MERGED");
    assert_eq!(from_rest.mergeable, "MERGEABLE");
    assert_eq!(from_rest.base_ref_name.as_deref(), Some("release/topic"));
    assert_eq!(
        from_rest.base_ref_oid.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    let check = &from_rest.status_check_rollup.as_ref().unwrap()[0];
    assert_eq!(check.status.as_deref(), Some("COMPLETED"));
    assert_eq!(check.conclusion.as_deref(), Some("SUCCESS"));
}

// what this catches: a PR closed WITHOUT merging keeps its real state —
// the boundary normalizes vocabulary, it must not invent the merge fact.
#[test]
fn from_rest_keeps_closed_when_never_merged() {
    let view = PrView::from_rest(
        &serde_json::json!({"state": "closed", "mergeable": false}),
        Vec::new(),
    );
    assert_eq!(view.state, "CLOSED");
    assert_eq!(view.mergeable, "CONFLICTING");
    assert_eq!(view.merged_at, None);
}

#[test]
fn parse_check_runs_extracts_rest_started_at() {
    // The REST endpoint uses snake_case `started_at` (different
    // from the GraphQL `startedAt`). parse_check_runs bridges
    // both into the same GhCheck so the merger doesn't care
    // which path supplied the rollup. A regression here would
    // make baseline-side timeouts invisible.
    let json = serde_json::json!({
        "total_count": 1,
        "check_runs": [
            {
                "id": 1,
                "name": "cargo test (windows-latest)",
                "status": "in_progress",
                "conclusion": null,
                "started_at": "2026-05-29T03:29:46Z",
            }
        ]
    });
    let runs = parse_check_runs(json.to_string().as_bytes()).expect("REST envelope decodes");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].started_at.as_deref(), Some("2026-05-29T03:29:46Z"));
}

// What this catches (045083e8): error-shaped, missing, oversized,
// truncated, duplicate, or changing pages cannot form a complete proof.
#[test]
fn parse_check_runs_requires_complete_consistent_pages() {
    for invalid in [
        "",
        r#"{"message":"Not Found"}"#,
        r#"{"check_runs":[]}"#,
        r#"{"total_count":0}"#,
        r#"{"total_count":1,"check_runs":[]}"#,
        r#"{"total_count":10001,"check_runs":[]}"#,
        r#"{"total_count":1,"check_runs":[{"name":"missing id"}]}"#,
        r#"{"total_count":1,"check_runs":[{"id":1,"name":"missing status","conclusion":"success"}]}"#,
        r#"{"total_count":1,"check_runs":[{"id":1,"name":"missing verdict","status":"completed","conclusion":null}]}"#,
    ] {
        assert!(parse_check_runs(invalid.as_bytes()).is_err(), "{invalid}");
    }
    let first = serde_json::json!({
        "total_count":101,
        "check_runs": (1..=100).map(|id| serde_json::json!({
            "id":id, "name":format!("check-{id}"), "status":"completed", "conclusion":"success"
        })).collect::<Vec<_>>()
    })
    .to_string();
    assert!(
        parse_check_runs(first.as_bytes()).is_err(),
        "first page is incomplete"
    );
    for (total, id) in [(101, 1), (102, 101)] {
        let last = serde_json::json!({
            "total_count":total,
            "check_runs":[{"id":id,"name":"last","status":"completed","conclusion":"failure"}]
        });
        assert!(parse_check_runs(format!("{first}\n{last}").as_bytes()).is_err());
    }
    let complete = format!(
        "{first}\n{}",
        serde_json::json!({
            "total_count":101,
            "check_runs":[{"id":101,"name":"last","status":"completed","conclusion":"failure"}]
        })
    );
    let checks = parse_check_runs(complete.as_bytes()).unwrap();
    assert_eq!(checks.len(), 101);
    assert_eq!(checks[100].conclusion.as_deref(), Some("FAILURE"));
    assert!(parse_check_runs(
        format!("{complete}\n{{\"total_count\":0,\"check_runs\":[]}}").as_bytes()
    )
    .is_err());
}
