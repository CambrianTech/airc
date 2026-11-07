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

    /// The PR's head is no longer the commit the merge pinned (REST 409, gh "head branch was
    /// modified"): something was pushed after the gate judged it. Nothing merged; the card waits
    /// for a submission and verdict at the new head.
    #[error("pr head moved since it was judged: {stderr}")]
    HeadMoved { stderr: String },

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
    /// The head commit the merge must land: GitHub refuses the merge when the PR's head is no
    /// longer this (gh `--match-head-commit`, REST `sha`), so a push after the gate judged the PR
    /// never lands unreviewed (card 950b3093).
    pub head: airc_work::GitObjectId,
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
    /// Git ref or object id, resolved against
    /// `repos/{owner}/{repo}/commits/{branch}/check-runs`. Merge gates use
    /// the PR's immutable base object id, so a moving branch cannot change
    /// the baseline between the PR read and the check lookup.
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
    /// Actual target from the live PR, not the possibly stale work-card link.
    #[serde(default, rename = "baseRefName")]
    pub base_ref_name: Option<String>,
    /// Target revision observed in the same PR response.
    #[serde(default, rename = "baseRefOid")]
    pub base_ref_oid: Option<String>,
    /// The PR's head commit in the same response: what a merge would land, and what a verdict
    /// must have been read at (the merge gate, card 950b3093).
    #[serde(default, rename = "headRefOid")]
    pub head_ref_oid: Option<String>,
    /// The default branch of the repository the PR targets, from the REST response's
    /// `base.repo.default_branch`: where a merge must land. `gh pr view --json` has no such field,
    /// so a GraphQL view leaves it unknown and the merge gate refuses rather than guess.
    #[serde(skip)]
    pub base_default_branch: Option<String>,
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
            base_ref_name: pr_json
                .pointer("/base/ref")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            base_ref_oid: pr_json
                .pointer("/base/sha")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            head_ref_oid: pr_json
                .pointer("/head/sha")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            base_default_branch: pr_json
                .pointer("/base/repo/default_branch")
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

/// GitHub's maximum requested page size; shared by both HTTP adapters.
pub const CHECK_RUN_PAGE_SIZE: usize = 100;
/// Bound work per rollup to 100 pages. Larger rollups are unverifiable by
/// this reader and fail closed; they are never silently truncated to green.
const MAX_CHECK_RUNS: usize = 10_000;

/// A required REST envelope. Error JSON or missing pagination metadata must
/// not deserialize to an apparently successful empty check list (045083e8).
#[derive(Deserialize)]
pub struct CheckRunsPage {
    total_count: usize,
    check_runs: Vec<RestCheckRun>,
}

#[derive(Deserialize)]
struct RestCheckRun {
    id: u64,
    #[serde(default)]
    conclusion: Option<String>,
    status: String,
    name: String,
    #[serde(default, alias = "startedAt")]
    started_at: Option<String>,
}

/// Accumulates one bounded, complete check rollup. Run IDs detect duplicate
/// or overlapping pages; a changed total or short page requires a fresh read.
/// Values move into the canonical projection without re-serialization.
#[derive(Default)]
pub struct CheckRunRollup {
    expected: Option<usize>,
    seen: std::collections::HashSet<u64>,
    checks: Vec<GhCheck>,
}

impl CheckRunRollup {
    pub fn push_page(&mut self, page: CheckRunsPage) -> Result<bool, GhError> {
        if page.total_count > MAX_CHECK_RUNS {
            return Err(GhError::OutputParse(format!(
                "check rollup reports {} runs, exceeding reader bound {MAX_CHECK_RUNS}",
                page.total_count
            )));
        }
        if let Some(expected) = self.expected {
            if expected != page.total_count || self.checks.len() == expected {
                return Err(GhError::OutputParse(
                    "check rollup total changed or another page followed completion".into(),
                ));
            }
        }
        let remaining = page.total_count.saturating_sub(self.checks.len());
        if page.check_runs.len() != remaining.min(CHECK_RUN_PAGE_SIZE) {
            return Err(GhError::OutputParse(format!(
                "incomplete check page: received {}, expected {} of {} remaining runs",
                page.check_runs.len(),
                remaining.min(CHECK_RUN_PAGE_SIZE),
                remaining
            )));
        }
        self.expected = Some(page.total_count);
        for run in page.check_runs {
            if !self.seen.insert(run.id) {
                return Err(GhError::OutputParse(format!(
                    "check rollup repeated run id {}",
                    run.id
                )));
            }
            let mut check = GhCheck {
                conclusion: run.conclusion,
                status: Some(run.status),
                name: Some(run.name),
                started_at: run.started_at,
            };
            check.canonicalize();
            if check.status.as_deref() == Some("COMPLETED") && check.conclusion.is_none() {
                return Err(GhError::OutputParse(
                    "completed check run is missing its conclusion".into(),
                ));
            }
            self.checks.push(check);
        }
        Ok(self.checks.len() == page.total_count)
    }

    pub fn into_checks(self) -> Result<Vec<GhCheck>, GhError> {
        if self.expected != Some(self.checks.len()) {
            return Err(GhError::OutputParse(
                "check rollup ended before all pages arrived".into(),
            ));
        }
        Ok(self.checks)
    }
}

/// Decode a complete REST envelope or gh's concatenated `--paginate` page
/// stream. Missing/error envelopes and incomplete streams fail closed.
pub fn parse_check_runs(json: &[u8]) -> Result<Vec<GhCheck>, GhError> {
    let mut rollup = CheckRunRollup::default();
    for page in serde_json::Deserializer::from_slice(json).into_iter::<CheckRunsPage>() {
        rollup.push_page(page?)?;
    }
    rollup.into_checks()
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
mod tests;

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
pub mod mock;
