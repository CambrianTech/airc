//! Card 00e3aa39 Sub-2 — `ReqwestGhClient`: direct GitHub REST via reqwest.
//!
//! Java-like module split per Joel's design directive: ShellGhClient lives
//! in `gh_client.rs` (subprocess path); this module owns the HTTP-direct
//! impl. Both implement the same `GhClient` trait from `airc-lib` so the
//! merger / work_commands can swap implementations without surface change.

use async_trait::async_trait;

use airc_lib::gh_client::{
    BranchCheckRollupArgs, GhCheck, GhClient, GhError, MergeReceipt, PrCreateArgs, PrCreated,
    PrEditBaseArgs, PrMergeArgs, PrView, PrViewArgs,
};
use tokio::process::Command;

// ============================================================================
// Card 00e3aa39 Sub-2 — ReqwestGhClient: direct GitHub REST via reqwest.
//
// Replaces the ShellGhClient's gh-subprocess cost (525ms/call measured on M2
// release; see Sub-1's bench at #1082) with HTTP/2 keep-alive against the
// GitHub REST API. Acceptance criterion: < 131ms/call on the same bench.
//
// Implementation surface mirrors ShellGhClient — same GhClient trait, same
// typed errors, so swap is a one-line change at the call site. Auth comes
// from GH_TOKEN/GITHUB_TOKEN env first, fallback to a one-time `gh auth
// token` spawn (cached for the process lifetime; on an HTTP 401 the chain
// re-runs ONCE and the request retries with the fresh token — card
// 09cd0afb Sub-3, because gh tokens rotate mid-session and a long-lived
// merger daemon must survive that without a restart).
// ============================================================================

/// Test seam for the 401-refresh tests: replaces the env/gh-spawn token
/// resolution chain so "stale cached token → fresh resolved token" is
/// observable without racy env mutation. Never compiled into production.
#[cfg(test)]
type TokenResolver = std::sync::Arc<dyn Fn() -> Result<String, GhError> + Send + Sync>;

/// Direct-HTTP [`GhClient`] backed by `reqwest`. One shared `reqwest::Client`
/// across calls so the TCP+TLS handshake amortises — that's where the
/// gh-spawn win over `ShellGhClient` comes from.
#[derive(Clone)]
pub struct ReqwestGhClient {
    http: reqwest::Client,
    /// Resolved bearer token. `RwLock<Option<…>>` (not `OnceLock`) so a
    /// 401 can evict and replace it — gh API tokens rotate mid-process
    /// (documented operational fact), and `OnceLock` made that fatal
    /// until restart. Happy path cost is one uncontended read-lock +
    /// one `String` clone per request, same as the old `get().clone()`.
    token: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    /// API origin. `https://api.github.com` in production; tests point
    /// it at a local listener so the wire shape (auth header, accept,
    /// api-version) is pinned without touching live GitHub.
    api_base: String,
    #[cfg(test)]
    test_resolver: Option<TokenResolver>,
}

// Card c1090a24: manual Debug — the derived impl would print the
// resolved bearer token through any `{:?}` of the client (merger logs,
// error contexts). The token is NEVER logged; only whether one has
// been resolved yet.
impl std::fmt::Debug for ReqwestGhClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let resolved = match self.token.read() {
            Ok(guard) => guard.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        };
        f.debug_struct("ReqwestGhClient")
            .field("api_base", &self.api_base)
            .field("token", &resolved.then_some("<redacted>"))
            .finish_non_exhaustive()
    }
}

impl ReqwestGhClient {
    /// Build a client with the standard user-agent and timeouts. Token
    /// resolution is deferred to the first call (`ensure_token`) so
    /// construction is cheap and infallible — operators who never call
    /// methods don't pay an auth-spawn cost.
    pub fn new() -> Result<Self, GhError> {
        let http = reqwest::Client::builder()
            .user_agent("airc-merger/1.0 (+https://github.com/CambrianTech/airc)")
            // Connection + per-call timeouts catch a hung GitHub endpoint
            // before it stalls the merger tick. 15s matches the worst-case
            // ShellGhClient round-trip we observed.
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| GhError::Process(std::io::Error::other(e.to_string())))?;
        Ok(Self {
            http,
            token: std::sync::Arc::new(std::sync::RwLock::new(None)),
            api_base: "https://api.github.com".to_string(),
            #[cfg(test)]
            test_resolver: None,
        })
    }

    /// Test seam: a client aimed at a local listener with a pre-resolved
    /// token. Lets the wire-shape tests pin the Authorization header
    /// without env mutation (racy under parallel tests) or live GitHub.
    #[cfg(test)]
    pub(crate) fn for_test(api_base: String, token: String) -> Result<Self, GhError> {
        let client = Self::new()?;
        client.store_token(token);
        Ok(Self { api_base, ..client })
    }

    /// Test seam for the 401-refresh path: like [`Self::for_test`] but
    /// the resolution chain is replaced by `resolver`, so a refresh
    /// observably swaps `initial_token` for whatever `resolver` yields.
    #[cfg(test)]
    pub(crate) fn for_test_with_resolver(
        api_base: String,
        initial_token: String,
        resolver: TokenResolver,
    ) -> Result<Self, GhError> {
        let mut client = Self::for_test(api_base, initial_token)?;
        client.test_resolver = Some(resolver);
        Ok(client)
    }

    /// Read the cached token, surviving lock poisoning (a panicked
    /// holder can't corrupt an `Option<String>` — the value is either
    /// the old token or the new one, both valid states).
    fn cached_token(&self) -> Option<String> {
        match self.token.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Replace the cached token.
    fn store_token(&self, token: String) {
        let mut guard = match self.token.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(token);
    }

    /// Run the token resolution chain, IGNORING the cache. Tries
    /// `GH_TOKEN` then `GITHUB_TOKEN` env first (cheap, what CI
    /// typically provides; same precedence as the gh CLI itself);
    /// falls back to a `gh auth token` spawn (~50ms). Used for both
    /// first resolution and the 401-triggered refresh — the keychain
    /// copy `gh auth token` reads usually outlives a rotated env copy,
    /// so the re-spawn is the part that actually rescues a long-lived
    /// daemon.
    async fn resolve_token_uncached(&self) -> Result<String, GhError> {
        #[cfg(test)]
        if let Some(resolver) = &self.test_resolver {
            return resolver();
        }
        for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
            if let Ok(env) = std::env::var(var) {
                if !env.is_empty() {
                    return Ok(env);
                }
            }
        }
        // gh-auth-token spawn. Same shape as ShellGhClient's subprocess
        // pattern; this is the only gh process spawn we do.
        let output = Command::new("gh")
            .args(["auth", "token"])
            .output()
            .await
            .map_err(crate::gh_client::map_spawn_error)?;
        if !output.status.success() {
            return Err(GhError::AuthRequired {
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(GhError::AuthRequired {
                stderr: "`gh auth token` returned empty".to_string(),
            });
        }
        Ok(token)
    }

    /// Cached-token fast path: no resolution cost per request once a
    /// token is held; first call resolves and caches.
    async fn ensure_token(&self) -> Result<String, GhError> {
        if let Some(token) = self.cached_token() {
            return Ok(token);
        }
        let token = self.resolve_token_uncached().await?;
        self.store_token(token.clone());
        Ok(token)
    }

    /// 401 path (card 09cd0afb Sub-3): bypass the cache, re-run the
    /// resolution chain once, replace the cached token, return it. A
    /// resolution failure here propagates loudly — there is no silent
    /// fallback to the shell backend.
    async fn refresh_token(&self) -> Result<String, GhError> {
        let token = self.resolve_token_uncached().await?;
        self.store_token(token.clone());
        Ok(token)
    }

    /// Build one authenticated request: GitHub Accept + api-version
    /// headers, bearer auth, optional JSON body.
    fn build_request<B>(
        &self,
        method: reqwest::Method,
        url: &str,
        token: &str,
        body: Option<&B>,
    ) -> reqwest::RequestBuilder
    where
        B: serde::Serialize + ?Sized,
    {
        let mut request = self
            .http
            .request(method, url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(body) = body {
            request = request.json(body);
        }
        request
    }

    /// Send an authenticated request with EXACTLY ONE 401-triggered
    /// token refresh + retry (card 09cd0afb Sub-3). On the first 401,
    /// the resolution chain re-runs (cache bypassed, `gh auth token`
    /// re-spawned) and the request retries with the fresh token. If the
    /// retry also 401s, fail loudly with an actionable error — never a
    /// retry storm, never a silent fallback. Non-401 responses (success
    /// or otherwise) are returned to the caller for status handling.
    async fn send_authed<B>(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<&B>,
    ) -> Result<reqwest::Response, GhError>
    where
        B: serde::Serialize + ?Sized + Sync,
    {
        let token = self.ensure_token().await?;
        let first = self
            .build_request(method.clone(), url, &token, body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if first.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(first);
        }
        // Token expired/rotated mid-process. One refresh, one retry.
        drop(first);
        let fresh = self.refresh_token().await?;
        let second = self
            .build_request(method, url, &fresh, body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if second.status() == reqwest::StatusCode::UNAUTHORIZED {
            // The error names the resolution chain and the fix, never
            // the token values themselves.
            let github_said = second.text().await.unwrap_or_default();
            return Err(GhError::AuthRequired {
                stderr: format!(
                    "GitHub returned 401 twice for {url} — the cached token AND a freshly \
                     re-resolved one (GH_TOKEN → GITHUB_TOKEN → `gh auth token`) were both \
                     rejected. Re-authenticate (`gh auth login`) or rotate the GH_TOKEN / \
                     GITHUB_TOKEN env var. GitHub said: {github_said}"
                ),
            });
        }
        Ok(second)
    }
}

/// `None` body for GET-shaped calls through [`ReqwestGhClient::send_authed`]
/// (a typed `None` so the generic parameter is inferable).
const NO_BODY: Option<&serde_json::Value> = None;

// Card 00e3aa39 Sub-2: deliberately no `Default` impl. `new()` returns
// `Result<Self, GhError>` because the reqwest builder can in principle
// fail (TLS init, etc.); callers should handle that result rather than
// hiding it behind a panicking `Default`. clippy::expect_used (denied
// workspace-wide) caught the original `.expect("…")` shortcut.

/// `POST /repos/{owner}/{repo}/pulls` request body — the single source
/// of truth for the wire field names (pinned by the pr_create
/// wire-shape test). Holds no token; field set mirrors what the gh CLI
/// sends for `gh pr create --base <base>` (no draft flag → false).
#[derive(Debug, serde::Serialize)]
struct PrCreateRequest<'a> {
    title: &'a str,
    body: &'a str,
    head: &'a str,
    base: &'a str,
    draft: bool,
}

/// The slice of GitHub's create-PR response we consume; unknown fields
/// are ignored (GitHub adds fields freely).
#[derive(Debug, serde::Deserialize)]
struct PrCreateResponse {
    html_url: String,
    number: u64,
}

#[async_trait]
impl GhClient for ReqwestGhClient {
    async fn pr_view(&self, args: PrViewArgs) -> Result<PrView, GhError> {
        // The gh-pr-view shape gh constructs via GraphQL projects two REST
        // calls: the PR object itself (for state + mergeable) and its
        // check-suite rollup. We mirror that shape by hitting both REST
        // endpoints; reqwest's HTTP/2 multiplexes them over the same TCP
        // connection so this is still ~half the wall-clock of the
        // ShellGhClient single-call.
        let pr_url = format!(
            "{}/repos/{}/pulls/{}",
            self.api_base, args.repo, args.number
        );
        let pr_resp = self
            .send_authed(reqwest::Method::GET, &pr_url, NO_BODY)
            .await?;
        let pr_json: serde_json::Value = handle_response(pr_resp).await?;

        let head_sha = pr_json
            .get("head")
            .and_then(|h| h.get("sha"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                GhError::OutputParse(format!(
                    "PR {} response missing head.sha — gh schema drift?",
                    args.number
                ))
            })?;

        let runs_url = format!(
            "{}/repos/{}/commits/{}/check-runs?per_page=100",
            self.api_base, args.repo, head_sha
        );
        let runs_resp = self
            .send_authed(reqwest::Method::GET, &runs_url, NO_BODY)
            .await?;
        let runs_bytes = runs_resp.bytes().await.map_err(map_reqwest_error)?;
        let check_runs = airc_lib::gh_client::parse_check_runs(&runs_bytes)?;

        let state = pr_json
            .get("state")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_uppercase();
        // GitHub's mergeable field is tri-state: null (computing), true
        // (MERGEABLE), false (CONFLICTING). gh exposes the same as the
        // string variants.
        let mergeable = match pr_json.get("mergeable") {
            Some(serde_json::Value::Bool(true)) => "MERGEABLE",
            Some(serde_json::Value::Bool(false)) => "CONFLICTING",
            _ => "UNKNOWN",
        }
        .to_string();

        // `mergedAt` is populated by GitHub's REST API only when the
        // PR has been merged. For OPEN PRs the field is absent or
        // null; we surface it as `None`. The merger's reconcile path
        // (card acd72c81 follow-up) reads this to emit
        // `PullRequestMerged` with the canonical GitHub timestamp.
        let merged_at = pr_json
            .get("merged_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(PrView {
            state,
            mergeable,
            status_check_rollup: Some(check_runs),
            merged_at,
        })
    }

    async fn pr_create(&self, args: PrCreateArgs) -> Result<PrCreated, GhError> {
        // Card 09cd0afb Sub-3: POST /repos/{owner}/{repo}/pulls.
        // Mirrors ShellGhClient::pr_create semantics (`gh pr create
        // --fill --base <base>` from args.cwd): gh resolves the repo
        // from the origin remote, the head from the current branch,
        // and the title/body from the HEAD commit — we do the same
        // resolution explicitly via the existing work_commands_git
        // helpers (single source of truth; deterministic HEAD
        // subject/body rather than --fill's branch-name heuristic,
        // see the card 13131f1c note on those helpers). Like the
        // shell path, no draft flag → draft: false. The git reads are
        // short-lived plumbing spawns; pr_create is off the per-tick
        // hot path (it runs once per PR open).
        let cwd = args.cwd.to_string_lossy().to_string();
        let repo = crate::work_commands_git::cwd_github_repo_id(&cwd).ok_or_else(|| {
            GhError::NotInGithubRepo {
                stderr: format!(
                    "{} has no github.com origin remote — cannot resolve owner/repo \
                     for POST /pulls",
                    args.cwd.display()
                ),
            }
        })?;
        let head = crate::work_commands_git::git_rev_parse_branch(&cwd)
            .map_err(|e| GhError::Process(std::io::Error::other(e.to_string())))?;
        let subject = crate::work_commands_git::git_show_format(&cwd, "%s")
            .map_err(|e| GhError::Process(std::io::Error::other(e.to_string())))?;
        let body_text = crate::work_commands_git::git_show_format(&cwd, "%b")
            .map_err(|e| GhError::Process(std::io::Error::other(e.to_string())))?;

        let url = format!("{}/repos/{}/pulls", self.api_base, repo);
        let request = PrCreateRequest {
            title: subject.trim(),
            body: body_text.trim(),
            head: &head,
            base: &args.base,
            draft: false,
        };
        let resp = self
            .send_authed(reqwest::Method::POST, &url, Some(&request))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(map_http_error_status(status, resp).await);
        }
        let created: PrCreateResponse = resp.json().await.map_err(map_reqwest_error)?;
        Ok(PrCreated {
            url: created.html_url,
            number: created.number,
        })
    }

    async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError> {
        let url = format!(
            "{}/repos/{}/pulls/{}/merge",
            self.api_base, args.repo, args.number
        );
        let body = serde_json::json!({ "merge_method": "squash" });
        let resp = self
            .send_authed(reqwest::Method::PUT, &url, Some(&body))
            .await?;
        // Successful merge returns 200 with `merged: true`. 405 (method
        // not allowed) and 409 (conflict) both map to PrNotMergeable.
        let status = resp.status();
        if status.is_success() {
            return Ok(MergeReceipt {
                repo: args.repo,
                number: args.number,
            });
        }
        if status == reqwest::StatusCode::METHOD_NOT_ALLOWED
            || status == reqwest::StatusCode::CONFLICT
        {
            return Err(GhError::PrNotMergeable {
                stderr: resp.text().await.unwrap_or_default(),
            });
        }
        Err(map_http_error_status(status, resp).await)
    }

    async fn pr_edit_base(&self, args: PrEditBaseArgs) -> Result<(), GhError> {
        let url = format!(
            "{}/repos/{}/pulls/{}",
            self.api_base, args.repo, args.number
        );
        let body = serde_json::json!({ "base": args.base });
        let resp = self
            .send_authed(reqwest::Method::PATCH, &url, Some(&body))
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(map_http_error_status(resp.status(), resp).await)
        }
    }

    async fn branch_check_rollup(
        &self,
        args: BranchCheckRollupArgs,
    ) -> Result<Vec<GhCheck>, GhError> {
        // The hot path the bench (#1082) measures. Single GET; no
        // process spawn; HTTP/2 keep-alive amortised across calls.
        let url = format!(
            "{}/repos/{}/commits/{}/check-runs?per_page=100",
            self.api_base, args.repo, args.branch
        );
        let resp = self
            .send_authed(reqwest::Method::GET, &url, NO_BODY)
            .await?;
        if !resp.status().is_success() {
            return Err(map_http_error_status(resp.status(), resp).await);
        }
        let bytes = resp.bytes().await.map_err(map_reqwest_error)?;
        airc_lib::gh_client::parse_check_runs(&bytes)
    }
}

// ============================================================================
// Card c1090a24 — production backend selection. The merger and the
// work merge path call this instead of constructing ShellGhClient
// directly, so the gh-spawn cost is off the hot path by default.
// ============================================================================

/// Which `GhClient` implementation the production paths should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhBackend {
    /// Direct REST via reqwest (default — no per-call gh spawn).
    Reqwest,
    /// gh-CLI subprocess path (explicit operator opt-out).
    Shell,
}

/// Parse the `AIRC_GH_BACKEND` value. Pure so it's testable without
/// env mutation. `None`/empty → default (Reqwest). Unknown values are
/// an `Err` — the caller warns loudly and uses the default rather
/// than silently honouring a typo.
pub fn parse_backend(value: Option<&str>) -> Result<GhBackend, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(GhBackend::Reqwest),
        Some("reqwest") => Ok(GhBackend::Reqwest),
        Some("shell") => Ok(GhBackend::Shell),
        Some(other) => Err(format!(
            "unknown AIRC_GH_BACKEND value {other:?} (expected \"reqwest\" or \"shell\")"
        )),
    }
}

/// Build the `GhClient` the production paths (merger tick, `work
/// merge`) use. Default is [`ReqwestGhClient`]; `AIRC_GH_BACKEND=shell`
/// opts back into the subprocess path explicitly.
///
/// No-silent-fallback contract (card c1090a24): every path that ends
/// in ShellGhClient other than the explicit opt-out prints a loud
/// warning saying WHY, so a fleet quietly paying 525ms/call again is
/// visible in the merger log, never silent.
pub fn production_gh_client() -> Box<dyn GhClient> {
    let raw = std::env::var("AIRC_GH_BACKEND").ok();
    let backend = match parse_backend(raw.as_deref()) {
        Ok(backend) => backend,
        Err(message) => {
            eprintln!("airc: WARN {message}; defaulting to the reqwest backend");
            GhBackend::Reqwest
        }
    };
    match backend {
        GhBackend::Shell => {
            eprintln!(
                "airc: gh backend = shell (AIRC_GH_BACKEND=shell) — \
                 per-call gh subprocess cost (~525ms) applies"
            );
            Box::new(crate::gh_client::ShellGhClient::new())
        }
        GhBackend::Reqwest => match ReqwestGhClient::new() {
            Ok(client) => Box::new(client),
            Err(error) => {
                // Loud fallback: construction can only fail in the
                // reqwest builder (TLS init). The merger must keep
                // working, but never silently — this line is the
                // tripwire for "why is the merger slow again?".
                eprintln!(
                    "airc: WARN ReqwestGhClient unavailable ({error}); \
                     falling back to ShellGhClient (gh subprocess, ~525ms/call)"
                );
                Box::new(crate::gh_client::ShellGhClient::new())
            }
        },
    }
}

/// Decode a 2xx JSON response into a serde_json::Value (the pr_view
/// shape is two endpoints; we project them into PrView at the call site).
async fn handle_response(resp: reqwest::Response) -> Result<serde_json::Value, GhError> {
    let status = resp.status();
    if !status.is_success() {
        return Err(map_http_error_status(status, resp).await);
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(map_reqwest_error)
}

/// Map a reqwest error into the typed GhError surface. Errors here
/// look like the gh-subprocess errors so callers don't have to
/// pattern-match on backend identity.
fn map_reqwest_error(error: reqwest::Error) -> GhError {
    if error.is_timeout() {
        GhError::Process(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            error.to_string(),
        ))
    } else if error.is_connect() {
        GhError::Process(std::io::Error::other(error.to_string()))
    } else if error.is_decode() {
        GhError::OutputParse(error.to_string())
    } else {
        GhError::GhExited {
            code: error.status().map(|s| s.as_u16() as i32),
            stderr: error.to_string(),
        }
    }
}

/// Map a non-2xx HTTP response to the typed GhError. Mirrors
/// ShellGhClient's classify_gh_failure shape so the merger's
/// existing error-handling paths work unchanged.
async fn map_http_error_status(status: reqwest::StatusCode, resp: reqwest::Response) -> GhError {
    let stderr = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        if stderr.to_lowercase().contains("rate") {
            return GhError::RateLimited { stderr };
        }
        return GhError::AuthRequired { stderr };
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return GhError::NotInGithubRepo { stderr };
    }
    GhError::GhExited {
        code: Some(status.as_u16() as i32),
        stderr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Scripted local HTTP listener: accepts ONE connection per
    /// scripted (status, body) response, captures each raw request
    /// (head + body, honouring Content-Length), replies, closes.
    /// Returns (base_url, join-handle-yielding-captured-requests).
    /// The listener drops after the last scripted response, so any
    /// extra request (a retry storm) fails to connect — loud in the
    /// caller's error, and visible as `captured.len()` to asserts.
    async fn scripted_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let mut captured = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read headers, then Content-Length more bytes (POST/PUT
                // bodies); GETs have no body and stop at the blank line.
                let request = loop {
                    let n = stream.read(&mut chunk).await.expect("read request");
                    if n == 0 {
                        break String::from_utf8_lossy(&buf).to_string();
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(headers_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..headers_end]).to_lowercase();
                        let content_length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= headers_end + 4 + content_length {
                            break String::from_utf8_lossy(&buf).to_string();
                        }
                    }
                };
                captured.push(request);
                let reason = if status < 400 { "OK" } else { "NOPE" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
                stream.flush().await.expect("flush");
            }
            captured
        });
        (format!("http://{addr}"), handle)
    }

    /// Await the scripted server with a deadline so a client that makes
    /// FEWER requests than scripted hangs the test loudly instead of
    /// forever.
    async fn join_server(handle: tokio::task::JoinHandle<Vec<String>>) -> Vec<String> {
        tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("server saw fewer requests than scripted (client gave up early?)")
            .expect("server task panicked")
    }

    /// Card c1090a24 wire-shape pin — the load-bearing one. The
    /// Authorization header IS the feature: break `Bearer {token}` in
    /// `authed()` (mutation check) and this fails. Also pins the
    /// GitHub Accept + api-version headers REST requires.
    #[tokio::test]
    async fn branch_check_rollup_sends_bearer_auth_and_github_headers() {
        let (base, server) =
            scripted_server(vec![(200, r#"{"total_count":0,"check_runs":[]}"#)]).await;
        let client = ReqwestGhClient::for_test(base, "test-token-sekrit".to_string())
            .expect("client builds");
        let runs = client
            .branch_check_rollup(BranchCheckRollupArgs {
                repo: "octo/repo".to_string(),
                branch: "main".to_string(),
            })
            .await
            .expect("rollup against local listener");
        assert!(runs.is_empty(), "empty check_runs envelope → empty vec");

        let mut requests = join_server(server).await;
        assert_eq!(requests.len(), 1, "exactly one request expected");
        let request = requests.remove(0);
        let lowered = request.to_lowercase();
        assert!(
            lowered.contains("authorization: bearer test-token-sekrit"),
            "Authorization: Bearer <token> header missing or malformed; request was:\n{request}"
        );
        assert!(
            lowered.contains("accept: application/vnd.github+json"),
            "GitHub Accept header missing; request was:\n{request}"
        );
        assert!(
            lowered.contains("x-github-api-version: 2022-11-28"),
            "X-GitHub-Api-Version header missing; request was:\n{request}"
        );
        assert!(
            request.starts_with("GET /repos/octo/repo/commits/main/check-runs"),
            "unexpected request line; request was:\n{request}"
        );
    }

    /// Card 09cd0afb Sub-3 pin (a): a 401 triggers EXACTLY ONE token
    /// re-resolution, and the retry carries the REFRESHED token, not
    /// the stale one. Mutation check: make the retry reuse the stale
    /// token and the second-request assert fails.
    #[tokio::test]
    async fn refresh_on_401_retries_once_with_fresh_token() {
        let (base, server) = scripted_server(vec![
            (401, r#"{"message":"Bad credentials"}"#),
            (200, r#"{"total_count":0,"check_runs":[]}"#),
        ])
        .await;
        let resolver: TokenResolver =
            std::sync::Arc::new(|| Ok("fresh-token-after-rotation".to_string()));
        let client = ReqwestGhClient::for_test_with_resolver(
            base,
            "stale-token-expired".to_string(),
            resolver,
        )
        .expect("client builds");
        let runs = client
            .branch_check_rollup(BranchCheckRollupArgs {
                repo: "octo/repo".to_string(),
                branch: "main".to_string(),
            })
            .await
            .expect("401 then 200 must succeed via one refresh");
        assert!(runs.is_empty());

        let requests = join_server(server).await;
        assert_eq!(requests.len(), 2, "one original + one retry, nothing more");
        let first = requests[0].to_lowercase();
        let second = requests[1].to_lowercase();
        assert!(
            first.contains("authorization: bearer stale-token-expired"),
            "first request must carry the cached (stale) token; was:\n{}",
            requests[0]
        );
        assert!(
            second.contains("authorization: bearer fresh-token-after-rotation"),
            "retry must carry the REFRESHED token — retrying with the stale \
             token is the exact bug this pins; was:\n{}",
            requests[1]
        );
        assert!(
            !second.contains("stale-token-expired"),
            "stale token must not appear on the retry; was:\n{}",
            requests[1]
        );
    }

    /// Card 09cd0afb Sub-3 pin (b): when the refreshed token ALSO 401s,
    /// fail loudly with AuthRequired after exactly two requests — one
    /// refresh attempt per request, never a retry storm. Mutation
    /// check: loop the refresh and the third connect fails (listener
    /// closed), turning the error non-AuthRequired → this test fails.
    #[tokio::test]
    async fn double_401_fails_loud_after_exactly_two_requests() {
        let (base, server) = scripted_server(vec![
            (401, r#"{"message":"Bad credentials"}"#),
            (401, r#"{"message":"Bad credentials"}"#),
        ])
        .await;
        let resolver: TokenResolver =
            std::sync::Arc::new(|| Ok("sekrit-fresh-rejected".to_string()));
        let client = ReqwestGhClient::for_test_with_resolver(
            base,
            "sekrit-stale-rejected".to_string(),
            resolver,
        )
        .expect("client builds");
        let error = client
            .branch_check_rollup(BranchCheckRollupArgs {
                repo: "octo/repo".to_string(),
                branch: "main".to_string(),
            })
            .await
            .expect_err("double 401 must be a loud error, not a hang or a storm");
        match error {
            GhError::AuthRequired { ref stderr } => {
                assert!(
                    stderr.contains("401 twice"),
                    "error must say the refresh was attempted and also rejected: {stderr}"
                );
                assert!(
                    stderr.contains("gh auth login"),
                    "error must be actionable: {stderr}"
                );
                assert!(
                    !stderr.contains("sekrit-stale-rejected")
                        && !stderr.contains("sekrit-fresh-rejected"),
                    "token values must NEVER appear in error messages: {stderr}"
                );
            }
            ref other => panic!("expected GhError::AuthRequired, got: {other:?}"),
        }
        let requests = join_server(server).await;
        assert_eq!(
            requests.len(),
            2,
            "exactly one refresh attempt per request — no retry storm"
        );
    }

    /// Card 09cd0afb Sub-3 pin (c): pr_create wire shape. POST to
    /// /repos/{owner}/{repo}/pulls with the literal JSON field names
    /// GitHub's create-PR endpoint requires, values resolved from the
    /// worktree exactly like the shell backend (origin remote → repo,
    /// current branch → head, HEAD commit → title/body, no draft).
    #[tokio::test]
    async fn pr_create_posts_typed_body_to_pulls_endpoint() {
        // A real throwaway git repo: pr_create resolves repo/head/
        // title/body from worktree state, mirroring ShellGhClient.
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_string_lossy().to_string();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "--initial-branch", "agent/feature-branch"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Wire Shape Test"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/octo/repo.git",
        ]);
        std::fs::write(temp.path().join("file.txt"), "payload").expect("write file");
        git(&["add", "."]);
        git(&[
            "commit",
            "-m",
            "feat(gh): wire pr_create\n\nBody of the commit.",
        ]);

        let (base, server) = scripted_server(vec![(
            201,
            r#"{"html_url":"https://github.com/octo/repo/pull/77","number":77,"state":"open"}"#,
        )])
        .await;
        let client =
            ReqwestGhClient::for_test(base, "test-token-sekrit".to_string()).expect("client");
        let created = client
            .pr_create(PrCreateArgs {
                cwd: temp.path().to_path_buf(),
                base: "rust-rewrite".to_string(),
            })
            .await
            .expect("pr_create against local listener");
        assert_eq!(created.number, 77);
        assert_eq!(created.url, "https://github.com/octo/repo/pull/77");

        let requests = join_server(server).await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request.starts_with("POST /repos/octo/repo/pulls HTTP/1.1"),
            "method + path pin; request was:\n{request}"
        );
        // Literal JSON field-name pins — the GitHub REST contract.
        for expected in [
            r#""title":"feat(gh): wire pr_create""#,
            r#""body":"Body of the commit.""#,
            r#""head":"agent/feature-branch""#,
            r#""base":"rust-rewrite""#,
            r#""draft":false"#,
        ] {
            assert!(
                request.contains(expected),
                "pr_create body missing {expected}; request was:\n{request}"
            );
        }
    }

    /// Card c1090a24 token-never-logged pin: `{:?}` of the client must
    /// not leak the resolved bearer token (the derived Debug did).
    #[tokio::test]
    async fn debug_format_redacts_token() {
        let client = ReqwestGhClient::for_test(
            "http://127.0.0.1:1".to_string(),
            "ghp_super_secret_value".to_string(),
        )
        .expect("client builds");
        let rendered = format!("{client:?}");
        assert!(
            !rendered.contains("ghp_super_secret_value"),
            "Debug output leaked the token: {rendered}"
        );
        assert!(
            rendered.contains("<redacted>"),
            "Debug output should show the token slot as <redacted>: {rendered}"
        );
    }

    /// AIRC_GH_BACKEND parsing: default is reqwest, shell is an
    /// explicit opt-out, typos are an error (the caller warns loudly
    /// and uses the default — never a silent guess).
    #[test]
    fn backend_parse_default_shell_and_unknown() {
        assert_eq!(parse_backend(None), Ok(GhBackend::Reqwest));
        assert_eq!(parse_backend(Some("")), Ok(GhBackend::Reqwest));
        assert_eq!(parse_backend(Some("reqwest")), Ok(GhBackend::Reqwest));
        assert_eq!(parse_backend(Some("shell")), Ok(GhBackend::Shell));
        assert_eq!(parse_backend(Some(" shell ")), Ok(GhBackend::Shell));
        let err = parse_backend(Some("curl")).expect_err("typo must not be honoured");
        assert!(
            err.contains("curl"),
            "error should name the bad value: {err}"
        );
    }
}
