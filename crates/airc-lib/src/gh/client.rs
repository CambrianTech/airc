//! Typed `gh` CLI boundary as a trait.
//!
//! Card a094aa81. Card dec35ec7 (#1041) shipped the typed wrapper
//! as a concrete struct inside `airc-cli`. The substrate-vision
//! cards (continuum, openclaw, hermes, codex consumer embedding)
//! need the same boundary without dragging the entire CLI binary
//! along.
//!
//! This module is the consumer-facing half: the [`GhClient`] trait,
//! the typed [`GhError`] enum, the arg/result value types, and the
//! pure parse helpers ([`parse_pr_view`], [`parse_pr_url`]) that
//! describe the shape of `gh`'s output independent of any spawn
//! mechanism. The default shell-based implementation lives in
//! `airc-cli::gh_client::ShellGhClient`; alternative implementations
//! (a mock for tests, an HTTP-direct version when gh's REST surface
//! suffices, a remote-RPC version for headless workers) plug into
//! the same boundary.
//!
//! The split is the same lesson `airc-lib` itself encodes: the
//! consumer-facing API is the trait, the binary owns the
//! implementation, and embedders pick what they want.
//!
//! Note on args ownership: the arg structs ([`PrViewArgs`],
//! [`PrCreateArgs`], …) own their strings (no `&str`/`&Path`
//! borrows). Across an `async fn` call boundary, owning small
//! strings is cheaper than the lifetime gymnastics that
//! `#[async_trait]` imposes on borrowed-arg traits — and it
//! makes mocking trivial.
//!
//! Future cards: see card a094aa81 followups for the mock impl
//! and the codex consumer's HTTP-direct path.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

/// Typed errors from any `gh` operation. Variants are the *actions*
/// a caller can take in response, not the exit codes — so a future
/// migration to `gh api`'s structured error shape doesn't churn the
/// downstream `match`es.
#[derive(Debug, Error)]
pub enum GhError {
    /// `gh` binary not on PATH or not executable. Operator config
    /// problem; nothing to retry.
    #[error("gh binary not found on PATH: {0}")]
    GhNotFound(std::io::Error),

    /// `gh auth status` failed when the call attempted authentication.
    /// Operator needs `gh auth login`.
    #[error("gh authentication required: run `gh auth login` ({stderr})")]
    AuthRequired { stderr: String },

    /// `gh` ran but the cwd isn't a github checkout (no `origin`
    /// remote, non-github origin, etc.). Card 59243bee already gates
    /// some of this on the airc side; this is the gh-side surface.
    #[error("not in a github checkout: {stderr}")]
    NotInGithubRepo { stderr: String },

    /// Hit GitHub's rate limit. Callers should back off and retry.
    /// The merger's tick failure path should match this and skip
    /// rather than logging an opaque "tick failed".
    #[error("github rate limit reached: {stderr}")]
    RateLimited { stderr: String },

    /// The PR exists but cannot be merged in its current state —
    /// conflicts, branch protection failure, or non-OPEN state.
    /// Distinct from a JSON-parse failure so the merger can hold
    /// the card in Review and wait.
    #[error("pr not mergeable: {stderr}")]
    PrNotMergeable { stderr: String },

    /// Catch-all for `gh` exiting non-zero in a way we haven't
    /// classified yet. Includes the stderr so the operator can act,
    /// but downstream code should NOT pattern-match on the string —
    /// upgrade to a typed variant if the case recurs.
    #[error("gh exited {code:?}: {stderr}")]
    GhExited { code: Option<i32>, stderr: String },

    /// `gh`'s JSON output didn't deserialize. Either a gh schema
    /// drift or we passed the wrong --json fields. Bug, not runtime
    /// condition.
    #[error("could not parse gh json output: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// `gh`'s stdout was the wrong shape (e.g. expected a PR URL on
    /// the last non-empty line, got something else). Distinct from
    /// `JsonParse` because gh's pr-create output is plain text, not
    /// JSON.
    #[error("could not parse gh output: {0}")]
    OutputParse(String),

    /// Process management failed (couldn't spawn, couldn't read
    /// stdout). Underlying io::Error included.
    #[error("gh process failed: {0}")]
    Process(#[from] std::io::Error),
}

/// Typed boundary around GitHub operations the airc CLI and embedded
/// consumers need.
///
/// The trait is dyn-compatible (via `#[async_trait]`) so consumers
/// can store `Arc<dyn GhClient>` without spreading a generic
/// parameter through every callsite. The same shape works for the
/// shell-based implementation in `airc-cli`, mock implementations in
/// tests, and the future HTTP-direct path for headless consumers.
#[async_trait]
pub trait GhClient: Send + Sync {
    /// `gh pr view <number> --repo <owner/name> --json state,mergeable,statusCheckRollup`.
    ///
    /// Used by the continuous-merger (#1037) to gate auto-merge.
    /// `cwd` is the worktree the call should run from — gh resolves
    /// the origin remote from cwd, NOT from `-C` (card a4fe899f).
    async fn pr_view(&self, args: PrViewArgs) -> Result<PrView, GhError>;

    /// `gh pr create --fill --base <branch>` from `cwd`.
    ///
    /// gh's pr-create output is plain text (the PR URL on the last
    /// non-empty line of stdout), not JSON.
    async fn pr_create(&self, args: PrCreateArgs) -> Result<PrCreated, GhError>;

    /// `gh pr merge <number> --repo <owner/name> --squash --delete-branch`.
    /// The merger's terminal call once gate logic passes.
    async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError>;

    /// `gh api -X PATCH repos/{owner}/{repo}/pulls/{number}` with the
    /// supplied `base` ref. Workaround for the Projects-classic
    /// deprecation that breaks `gh pr edit --base` (card 3bf62fbb).
    async fn pr_edit_base(&self, args: PrEditBaseArgs) -> Result<(), GhError>;

    /// `gh api repos/{owner}/{repo}/commits/{branch}/check-runs`.
    /// Card d5b7b07d — fetches the check-run rollup for an arbitrary
    /// branch's HEAD so the merger can implement the
    /// strictly-less-red-than-base doctrine (failures inherited from
    /// the integration branch don't block per-PR gates).
    async fn branch_check_rollup(
        &self,
        args: BranchCheckRollupArgs,
    ) -> Result<Vec<GhCheck>, GhError>;

    /// `gh issue view <number> --repo <owner/name> --json number,title,body,state`.
    ///
    /// Card #356 — the queue-card closer needs the issue BODY to verify
    /// the `airc-queue-card-v1` envelope and to apply the status-log
    /// mutation before closing. Every verb above operates on pull
    /// requests and none of them reads a body, so this is the first
    /// ISSUE verb on the trait and the first that treats free-form
    /// content as the payload.
    ///
    /// That makes it the deliberate outlier-B check on this trait's
    /// shape (CLAUDE.md's methodical process): if `GhClient` only fit
    /// PR-shaped operations, this is where it would have to be forced.
    /// It is not forced — issues reuse `repo` + `number` addressing and
    /// the same `GhError` classification — so the remaining closer
    /// verbs (`issue_edit_body`, `issue_close`) are the same pattern
    /// and are deliberately NOT built until a caller needs them.
    async fn issue_view(&self, args: IssueViewArgs) -> Result<IssueView, GhError>;
}

#[derive(Debug, Clone)]
pub struct PrViewArgs {
    pub repo: String,
    pub number: u64,
    /// Optional cwd. None = process cwd; Some(p) = gh resolves
    /// origin remote from p.
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PrCreateArgs {
    /// Worktree directory the new PR is opened from. gh reads
    /// `git remote get-url origin` here.
    pub cwd: PathBuf,
    /// Integration branch the PR targets. For airc that's
    /// `rust-rewrite` (card 28f1440c).
    pub base: String,
}

#[derive(Debug, Clone)]
pub struct PrMergeArgs {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone)]
pub struct PrEditBaseArgs {
    pub repo: String,
    pub number: u64,
    pub base: String,
}

/// Address an issue the same way the PR verbs address a PR: `repo` +
/// `number`. No `cwd` field — unlike `pr_create`, nothing here resolves
/// a remote from a worktree, so offering one would be a lie.
#[derive(Debug, Clone)]
pub struct IssueViewArgs {
    pub repo: String,
    pub number: u64,
}

/// An issue as the queue-card closer needs it. `body` is the payload:
/// the `airc-queue-card-v1` envelope lives there, and the closer both
/// VERIFIES it (skip anything that isn't a queue card) and MUTATES it
/// (status=merged + a status-log line) before closing.
///
/// `state` is the idempotence guard — a card already `CLOSED` is a
/// silent skip, which is what makes the workflow safe to re-run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IssueView {
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub title: String,
    /// An issue with no body serializes as JSON `null` on the REST API
    /// (and as `""` from the gh CLI). Both mean "no envelope here,
    /// skip this card", so both must deserialize — a bare
    /// `#[serde(default)]` covers the MISSING key but still errors on
    /// an explicit null, which would abort the workflow on the most
    /// ordinary input there is.
    #[serde(default, deserialize_with = "null_to_empty_string")]
    pub body: String,
    /// NORMALIZED UPPERCASE by every implementation. The gh CLI reports
    /// `OPEN`/`CLOSED` (GraphQL-backed) while REST reports
    /// `open`/`closed`; leaving that divergence in place would make the
    /// closer's idempotence check silently implementation-dependent.
    /// The trait's contract is the uppercase form.
    #[serde(default, deserialize_with = "upper_state")]
    pub state: String,
}

fn null_to_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn upper_state<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .unwrap_or_default()
        .to_uppercase())
}

#[derive(Debug, Clone)]
pub struct BranchCheckRollupArgs {
    pub repo: String,
    /// Branch name (e.g. `"rust-rewrite"`). Resolved by gh against
    /// `repos/{owner}/{repo}/commits/{branch}/check-runs`. Card d5b7b07d.
    pub branch: String,
}

/// `gh pr view --json state,mergeable,statusCheckRollup,mergedAt` shape.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PrView {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub mergeable: String,
    #[serde(default, rename = "statusCheckRollup")]
    pub status_check_rollup: Option<Vec<GhCheck>>,
    /// ISO-8601 timestamp of the merge, populated by gh when
    /// `state == "MERGED"`. `None` for OPEN PRs. The merger's
    /// reconcile path (card acd72c81 follow-up) parses this to emit
    /// `PullRequestMerged` with the canonical GitHub timestamp
    /// instead of wall-clock-now when transitioning the kanban for
    /// an already-merged PR.
    #[serde(default, rename = "mergedAt")]
    pub merged_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GhCheck {
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// ISO-8601 timestamp gh returns for when the check started.
    /// Used by the merger's pending-too-long timeout (card
    /// 7ed1ac4f) to distinguish "genuinely slow CI" from "CI runner
    /// hung." `None` when gh doesn't supply it (older check types,
    /// future schema drift).
    #[serde(default, rename = "startedAt")]
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCreated {
    pub url: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReceipt {
    pub repo: String,
    pub number: u64,
}

// --- pure parsers (testable without spawning gh) ---

/// Decode the JSON body of `gh pr view --json ...` into [`PrView`].
/// Pure — synthetic JSON in, typed value out. The merger and the
/// close-guard both depend on this shape, so this is where the
/// schema round-trip is pinned in tests.
///
/// Alias for [`PrView::from_graphql`]; the constructor is the boundary
/// (card fc483e57) and this name is kept for existing callers.
pub fn parse_pr_view(json: &[u8]) -> Result<PrView, GhError> {
    PrView::from_graphql(json)
}

/// THE DIALECT BOUNDARY (card fc483e57).
///
/// GitHub speaks two dialects for the same facts: GraphQL (what
/// `gh pr view --json` emits — `MERGED`, `SUCCESS`, `COMPLETED`,
/// `mergedAt`) and REST (what `/repos/../pulls/N` emits — `closed` +
/// `merged_at`, `success`, `completed`, `started_at`). Consumers —
/// above all the merger's `evaluate_gh_view` — read ONE vocabulary and
/// must never branch on which client produced the value.
///
/// Both bugs behind card c03bb62f were dialect leaking past this line
/// at two different places: a merged PR read as `CLOSED` (so the
/// reconcile never fired and cards sat at Review forever), and green
/// checks read as in-flight (so the production merger could only ever
/// merge through the pending-timeout bypass). Patching each site
/// separately would have meant a third patch for the third difference.
///
/// So: [`PrView`] and [`GhCheck`] are constructed HERE, through
/// `from_graphql` / `from_rest`, and both emit the canonical
/// (GraphQL) vocabulary. Add a new dialect difference to
/// `canonicalize`, never to a consumer.
impl PrView {
    /// `gh pr view --json state,mergeable,statusCheckRollup,mergedAt`.
    /// Already canonical on the wire; canonicalized anyway so the two
    /// constructors are interchangeable by contract, not by luck.
    pub fn from_graphql(json: &[u8]) -> Result<PrView, GhError> {
        let mut view: PrView = serde_json::from_slice(json)?;
        view.canonicalize();
        Ok(view)
    }

    /// REST `/repos/{owner}/{repo}/pulls/{n}` plus the separately
    /// fetched check-runs for its head SHA (REST has no rollup field).
    pub fn from_rest(pr_json: &serde_json::Value, checks: Vec<GhCheck>) -> PrView {
        // GitHub's REST `mergeable` is tri-state: null (still
        // computing), true, false. GraphQL names those states.
        let mergeable = match pr_json.get("mergeable") {
            Some(serde_json::Value::Bool(true)) => "MERGEABLE",
            Some(serde_json::Value::Bool(false)) => "CONFLICTING",
            _ => "UNKNOWN",
        }
        .to_string();
        let mut view = PrView {
            state: pr_json
                .get("state")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            mergeable,
            status_check_rollup: Some(checks),
            merged_at: pr_json
                .get("merged_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        };
        view.canonicalize();
        view
    }

    /// The whole dialect translation, in one place.
    fn canonicalize(&mut self) {
        self.state = self.state.to_ascii_uppercase();
        self.mergeable = self.mergeable.to_ascii_uppercase();
        // A merged PR is `closed` in REST and `MERGED` in GraphQL. The
        // fact is `merged_at`; the state string follows it.
        if self.merged_at.is_some() {
            self.state = "MERGED".to_string();
        }
        for check in self.status_check_rollup.iter_mut().flatten() {
            check.canonicalize();
        }
    }
}

impl GhCheck {
    /// REST spells conclusions/statuses in lowercase (`success`,
    /// `completed`); GraphQL — and every consumer — in UPPERCASE.
    fn canonicalize(&mut self) {
        if let Some(c) = self.conclusion.as_mut() {
            c.make_ascii_uppercase();
        }
        if let Some(s) = self.status.as_mut() {
            s.make_ascii_uppercase();
        }
    }
}

/// Decode `gh issue view --json number,title,body,state`. Pure — JSON
/// in, typed value out — so the closer's envelope/idempotence logic is
/// unit-testable without a network or a `gh` binary. Card #356.
///
/// Every field is `#[serde(default)]` on [`IssueView`]: gh omits keys
/// it has no value for, and an issue with an empty body is ordinary
/// (a card whose envelope was stripped), not a parse failure. Failing
/// there would turn a skippable non-card into a workflow abort.
pub fn parse_issue_view(json: &[u8]) -> Result<IssueView, GhError> {
    Ok(serde_json::from_slice(json)?)
}

/// Decode `gh api /repos/.../check-runs` (REST shape) into the same
/// [`GhCheck`] type the merger uses for PR rollups. Pure — synthetic
/// JSON in, typed values out. The REST endpoint wraps results in
/// `{total_count, check_runs: [...]}`; we project to just the run
/// list since the merger doesn't care about pagination metadata.
/// Card d5b7b07d.
///
/// REST `started_at` uses the snake_case field name (the REST API
/// differs from the GraphQL `startedAt` here); we accept both via a
/// custom Deserialize because [`GhCheck`] is the one struct shared
/// across both code paths.
///
/// Canonicalizes each run through [`GhCheck::canonicalize`] — the same
/// dialect boundary [`PrView::from_rest`] uses (card fc483e57), so a
/// consumer never sees REST's lowercase.
pub fn parse_check_runs(json: &[u8]) -> Result<Vec<GhCheck>, GhError> {
    #[derive(Deserialize)]
    struct RestRun {
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        started_at: Option<String>,
    }
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        check_runs: Vec<RestRun>,
    }
    let env: Envelope = serde_json::from_slice(json)?;
    Ok(env
        .check_runs
        .into_iter()
        .map(|r| {
            let mut check = GhCheck {
                conclusion: r.conclusion,
                status: r.status,
                name: r.name,
                started_at: r.started_at,
            };
            check.canonicalize();
            check
        })
        .collect())
}

/// Card 7ed1ac4f — parse an ISO-8601 timestamp gh returns (e.g.
/// `"2026-05-29T03:29:44Z"`) into ms-since-epoch. Pure parser so the
/// merger's pending-too-long policy can be unit-tested without a
/// real clock. Returns `None` for any shape we don't recognise —
/// callers MUST treat that as "don't know the age, can't time out"
/// rather than "old enough to bypass," matching the fail-closed
/// bias of the rest of the gate.
///
/// Hand-rolled rather than pulling in chrono just for this one
/// parse — the gh shape is fixed (`YYYY-MM-DDTHH:MM:SSZ`, UTC only,
/// 'Z' literal). Anything else is a schema drift we'd want to see
/// rather than silently coerce.
pub fn parse_iso_timestamp_ms(ts: &str) -> Option<u64> {
    // Expected exact length: 20 chars including the trailing Z.
    if ts.len() != 20 || !ts.ends_with('Z') {
        return None;
    }
    let date = &ts[..10];
    let sep = ts.as_bytes()[10];
    let time = &ts[11..19];
    if sep != b'T' {
        return None;
    }
    let mut dparts = date.split('-');
    let y = dparts.next()?.parse::<i64>().ok()?;
    let mo = dparts.next()?.parse::<u32>().ok()?;
    let d = dparts.next()?.parse::<u32>().ok()?;
    if dparts.next().is_some() {
        return None;
    }
    let mut tparts = time.split(':');
    let h = tparts.next()?.parse::<u32>().ok()?;
    let mi = tparts.next()?.parse::<u32>().ok()?;
    let s = tparts.next()?.parse::<u32>().ok()?;
    if tparts.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days-from-epoch via the standard zeller-style civil calendar
    // ("howard hinnant date algorithms"). Validates against
    // 1970-01-01..=9999-12-31 implicitly via the bounds above.
    let yi = y - i64::from(mo <= 2);
    let era = if yi >= 0 { yi / 400 } else { (yi - 399) / 400 };
    let yoe = (yi - era * 400) as u64;
    let doy =
        (153 * (u64::from(mo) + (if mo > 2 { 0 } else { 12 }) - 3) + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe as i64 - 719468;
    if days_since_epoch < 0 {
        return None;
    }
    let secs =
        (days_since_epoch as u64) * 86400 + u64::from(h) * 3600 + u64::from(mi) * 60 + u64::from(s);
    Some(secs * 1000)
}

/// Extract the PR URL + number from `gh pr create`'s plain-text
/// stdout. gh prints the URL on the last non-empty line; the number
/// is the trailing path segment.
///
/// Returns [`GhError::OutputParse`] when the shape doesn't match —
/// callers should NOT fall back to "extract from any line that looks
/// URL-shaped" (gh's output evolves; bug surface should surface).
pub fn parse_pr_url(stdout: &str) -> Result<PrCreated, GhError> {
    let url = stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .ok_or_else(|| GhError::OutputParse("gh pr create produced no output lines".into()))?;
    let number = url
        .rsplit('/')
        .next()
        .and_then(|tail| tail.parse::<u64>().ok())
        .ok_or_else(|| {
            GhError::OutputParse(format!(
                "could not extract PR number from gh output line: {url:?}"
            ))
        })?;
    Ok(PrCreated {
        url: url.to_string(),
        number,
    })
}

#[cfg(test)]
mod tests {
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
            {"name":"cargo test","status":"completed","conclusion":"success","started_at":"2026-09-04T17:36:00Z"},
            {"name":"cargo check","status":"in_progress","conclusion":null}
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
            "merged_at": "2026-09-04T18:01:58Z"
        });
        let rest_checks = parse_check_runs(
            br#"{"check_runs":[{"name":"cargo test","status":"completed","conclusion":"success"}]}"#,
        )
        .expect("rest checks parse");
        let from_rest = PrView::from_rest(&rest_pr, rest_checks);

        let from_graphql = PrView::from_graphql(
            br#"{"state":"MERGED","mergeable":"MERGEABLE","mergedAt":"2026-09-04T18:01:58Z",
                 "statusCheckRollup":[{"name":"cargo test","status":"COMPLETED","conclusion":"SUCCESS"}]}"#,
        )
        .expect("graphql parses");

        assert_eq!(from_rest, from_graphql, "the dialects must converge");
        assert_eq!(from_rest.state, "MERGED");
        assert_eq!(from_rest.mergeable, "MERGEABLE");
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
}

/// Testable [`GhClient`] impl backed by canned response queues.
///
/// Card bac49e0c. Lets unit tests in airc-cli, the merger, and
/// downstream consumers (continuum/hermes/openclaw embeddings) drive
/// the gh boundary deterministically — inject pre-shaped [`PrView`] /
/// [`PrCreated`] / [`GhError`] outcomes, then assert the gate logic
/// and dispatch behavior reacted as intended. No `gh` binary spawn,
/// no network, no rate-limit windows.
///
/// ## Usage shape
///
/// ```ignore
/// let mock = MockGhClient::new();
/// mock.queue_pr_view(Ok(make_view("OPEN", "MERGEABLE", &[])));
/// mock.queue_pr_view(Err(GhError::RateLimited { stderr: "...".into() }));
///
/// let view = mock.pr_view(args(42)).await.unwrap();
/// let err = mock.pr_view(args(43)).await.unwrap_err();
///
/// assert_eq!(mock.pr_view_call_count(), 2);
/// assert_eq!(mock.received_pr_view_calls()[0].number, 42);
/// ```
///
/// ## Empty-queue policy
///
/// If a test forgets to queue a response and the trait method is
/// called anyway, the mock returns
/// [`GhError::Process`] wrapping an `io::Error` with `Other` kind
/// and a message naming the method. This makes test failures
/// loud and actionable rather than silently returning a default
/// "successful" value — the test author meant to set up a
/// response and didn't.
#[cfg(any(test, feature = "mock"))]
pub mod mock {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::{
        BranchCheckRollupArgs, GhCheck, GhClient, GhError, IssueView, IssueViewArgs, MergeReceipt,
        PrCreateArgs, PrCreated, PrEditBaseArgs, PrMergeArgs, PrView, PrViewArgs,
    };

    /// Per-method response queue + per-method call record. All state
    /// is `Mutex`-protected for `&self` interior mutability (the
    /// trait methods take `&self`).
    #[derive(Default)]
    pub struct MockGhClient {
        pr_view_queue: Mutex<VecDeque<Result<PrView, GhError>>>,
        pr_create_queue: Mutex<VecDeque<Result<PrCreated, GhError>>>,
        pr_merge_queue: Mutex<VecDeque<Result<MergeReceipt, GhError>>>,
        pr_edit_base_queue: Mutex<VecDeque<Result<(), GhError>>>,
        branch_check_rollup_queue: Mutex<VecDeque<Result<Vec<GhCheck>, GhError>>>,
        issue_view_queue: Mutex<VecDeque<Result<IssueView, GhError>>>,

        pr_view_calls: Mutex<Vec<PrViewArgs>>,
        pr_create_calls: Mutex<Vec<PrCreateArgs>>,
        pr_merge_calls: Mutex<Vec<PrMergeArgs>>,
        pr_edit_base_calls: Mutex<Vec<PrEditBaseArgs>>,
        branch_check_rollup_calls: Mutex<Vec<BranchCheckRollupArgs>>,
        issue_view_calls: Mutex<Vec<IssueViewArgs>>,
    }

    impl MockGhClient {
        pub fn new() -> Self {
            Self::default()
        }

        // --- queue responses ---

        /// Queue the next [`GhClient::pr_view`] outcome. FIFO.
        pub fn queue_pr_view(&self, result: Result<PrView, GhError>) {
            self.pr_view_queue.lock().unwrap().push_back(result);
        }

        pub fn queue_pr_create(&self, result: Result<PrCreated, GhError>) {
            self.pr_create_queue.lock().unwrap().push_back(result);
        }

        pub fn queue_pr_merge(&self, result: Result<MergeReceipt, GhError>) {
            self.pr_merge_queue.lock().unwrap().push_back(result);
        }

        pub fn queue_pr_edit_base(&self, result: Result<(), GhError>) {
            self.pr_edit_base_queue.lock().unwrap().push_back(result);
        }

        pub fn queue_branch_check_rollup(&self, result: Result<Vec<GhCheck>, GhError>) {
            self.branch_check_rollup_queue
                .lock()
                .unwrap()
                .push_back(result);
        }

        /// Queue the next [`GhClient::issue_view`] outcome. FIFO. Card #356.
        pub fn queue_issue_view(&self, result: Result<IssueView, GhError>) {
            self.issue_view_queue.lock().unwrap().push_back(result);
        }

        // --- call records ---

        pub fn pr_view_call_count(&self) -> usize {
            self.pr_view_calls.lock().unwrap().len()
        }
        pub fn pr_create_call_count(&self) -> usize {
            self.pr_create_calls.lock().unwrap().len()
        }
        pub fn pr_merge_call_count(&self) -> usize {
            self.pr_merge_calls.lock().unwrap().len()
        }
        pub fn pr_edit_base_call_count(&self) -> usize {
            self.pr_edit_base_calls.lock().unwrap().len()
        }
        pub fn branch_check_rollup_call_count(&self) -> usize {
            self.branch_check_rollup_calls.lock().unwrap().len()
        }

        /// Snapshot of every [`GhClient::pr_view`] call's args, in
        /// invocation order. Useful for asserting "the merger
        /// queried PR 42 first, then PR 43" patterns.
        pub fn received_pr_view_calls(&self) -> Vec<PrViewArgs> {
            self.pr_view_calls.lock().unwrap().clone()
        }
        pub fn received_pr_create_calls(&self) -> Vec<PrCreateArgs> {
            self.pr_create_calls.lock().unwrap().clone()
        }
        pub fn received_pr_merge_calls(&self) -> Vec<PrMergeArgs> {
            self.pr_merge_calls.lock().unwrap().clone()
        }
        pub fn received_pr_edit_base_calls(&self) -> Vec<PrEditBaseArgs> {
            self.pr_edit_base_calls.lock().unwrap().clone()
        }
        pub fn received_branch_check_rollup_calls(&self) -> Vec<BranchCheckRollupArgs> {
            self.branch_check_rollup_calls.lock().unwrap().clone()
        }
        pub fn received_issue_view_calls(&self) -> Vec<IssueViewArgs> {
            self.issue_view_calls.lock().unwrap().clone()
        }
    }

    /// Build the "no response queued" error so test failures name
    /// the method that wasn't set up. Centralized so the message
    /// shape is consistent.
    fn unqueued(method: &str) -> GhError {
        GhError::Process(std::io::Error::other(format!(
            "MockGhClient::{method} called but no response was queued — \
             set one via queue_{method}() before invoking",
        )))
    }

    #[async_trait]
    impl GhClient for MockGhClient {
        async fn pr_view(&self, args: PrViewArgs) -> Result<PrView, GhError> {
            self.pr_view_calls.lock().unwrap().push(args);
            self.pr_view_queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(unqueued("pr_view")))
        }

        async fn pr_create(&self, args: PrCreateArgs) -> Result<PrCreated, GhError> {
            self.pr_create_calls.lock().unwrap().push(args);
            self.pr_create_queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(unqueued("pr_create")))
        }

        async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError> {
            self.pr_merge_calls.lock().unwrap().push(args);
            self.pr_merge_queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(unqueued("pr_merge")))
        }

        async fn pr_edit_base(&self, args: PrEditBaseArgs) -> Result<(), GhError> {
            self.pr_edit_base_calls.lock().unwrap().push(args);
            self.pr_edit_base_queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(unqueued("pr_edit_base")))
        }

        async fn branch_check_rollup(
            &self,
            args: BranchCheckRollupArgs,
        ) -> Result<Vec<GhCheck>, GhError> {
            self.branch_check_rollup_calls.lock().unwrap().push(args);
            self.branch_check_rollup_queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(unqueued("branch_check_rollup")))
        }

        async fn issue_view(&self, args: IssueViewArgs) -> Result<IssueView, GhError> {
            self.issue_view_calls.lock().unwrap().push(args);
            self.issue_view_queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(unqueued("issue_view")))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args_for_pr(number: u64) -> PrViewArgs {
            PrViewArgs {
                repo: "CambrianTech/airc".to_string(),
                number,
                cwd: None,
            }
        }

        fn ok_view(state: &str) -> PrView {
            PrView {
                state: state.to_string(),
                mergeable: "MERGEABLE".to_string(),
                status_check_rollup: Some(Vec::new()),
                merged_at: None,
            }
        }

        #[tokio::test]
        async fn queued_pr_view_responses_fire_in_order() {
            let mock = MockGhClient::new();
            mock.queue_pr_view(Ok(ok_view("OPEN")));
            mock.queue_pr_view(Ok(ok_view("MERGED")));

            let first = mock.pr_view(args_for_pr(42)).await.unwrap();
            let second = mock.pr_view(args_for_pr(43)).await.unwrap();

            assert_eq!(first.state, "OPEN");
            assert_eq!(second.state, "MERGED");
        }

        #[tokio::test]
        async fn unqueued_response_returns_loud_error_with_method_name() {
            let mock = MockGhClient::new();
            let err = mock.pr_view(args_for_pr(1)).await.unwrap_err();
            let GhError::Process(io) = err else {
                panic!("expected GhError::Process for empty queue, got something else");
            };
            assert!(
                io.to_string().contains("MockGhClient::pr_view"),
                "error must name the unset method: {io}"
            );
            assert!(
                io.to_string().contains("queue_pr_view"),
                "error must point at the queue method: {io}"
            );
        }

        #[tokio::test]
        async fn call_record_captures_args_in_invocation_order() {
            let mock = MockGhClient::new();
            mock.queue_pr_view(Ok(ok_view("OPEN")));
            mock.queue_pr_view(Ok(ok_view("OPEN")));

            mock.pr_view(args_for_pr(101)).await.unwrap();
            mock.pr_view(args_for_pr(202)).await.unwrap();

            assert_eq!(mock.pr_view_call_count(), 2);
            let received = mock.received_pr_view_calls();
            assert_eq!(received[0].number, 101);
            assert_eq!(received[1].number, 202);
        }

        #[tokio::test]
        async fn queued_pr_merge_can_return_typed_error() {
            let mock = MockGhClient::new();
            mock.queue_pr_merge(Err(GhError::PrNotMergeable {
                stderr: "conflicts".to_string(),
            }));

            let result = mock
                .pr_merge(PrMergeArgs {
                    repo: "CambrianTech/airc".to_string(),
                    number: 99,
                })
                .await;

            assert!(matches!(result, Err(GhError::PrNotMergeable { .. })));
            assert_eq!(mock.pr_merge_call_count(), 1);
        }

        #[tokio::test]
        async fn each_method_has_independent_queue_and_call_record() {
            let mock = MockGhClient::new();
            mock.queue_pr_view(Ok(ok_view("OPEN")));
            mock.queue_branch_check_rollup(Ok(vec![GhCheck {
                conclusion: Some("FAILURE".to_string()),
                status: Some("COMPLETED".to_string()),
                name: Some("flaky-base-check".to_string()),
                started_at: None,
            }]));

            mock.pr_view(args_for_pr(1)).await.unwrap();
            mock.branch_check_rollup(BranchCheckRollupArgs {
                repo: "CambrianTech/airc".to_string(),
                branch: "rust-rewrite".to_string(),
            })
            .await
            .unwrap();

            // Independent: pr_create's queue is still empty, the
            // call record is still zero.
            assert_eq!(mock.pr_view_call_count(), 1);
            assert_eq!(mock.branch_check_rollup_call_count(), 1);
            assert_eq!(mock.pr_create_call_count(), 0);
        }

        /// Trait-object usage — exercises that `Arc<dyn GhClient>`
        /// works against the mock. This is the merger / work_commands
        /// shape: code holds `&dyn GhClient`, tests inject
        /// `Arc::new(MockGhClient::new())`.
        #[tokio::test]
        async fn dyn_gh_client_dispatches_through_mock() {
            use std::sync::Arc;
            let mock = Arc::new(MockGhClient::new());
            mock.queue_pr_view(Ok(ok_view("OPEN")));

            let client: Arc<dyn GhClient> = mock.clone();
            let view = client.pr_view(args_for_pr(7)).await.unwrap();

            assert_eq!(view.state, "OPEN");
            assert_eq!(mock.pr_view_call_count(), 1);
        }
    }
}
