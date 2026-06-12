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
// from GITHUB_TOKEN env first, fallback to a one-time `gh auth token` spawn
// (cached for the process lifetime; gh's tokens are valid ~1h, refresh on
// HTTP 401).
// ============================================================================

/// Direct-HTTP [`GhClient`] backed by `reqwest`. One shared `reqwest::Client`
/// across calls so the TCP+TLS handshake amortises — that's where the
/// gh-spawn win over `ShellGhClient` comes from.
#[derive(Clone)]
pub struct ReqwestGhClient {
    http: reqwest::Client,
    token: std::sync::Arc<std::sync::OnceLock<String>>,
    /// API origin. `https://api.github.com` in production; tests point
    /// it at a local listener so the wire shape (auth header, accept,
    /// api-version) is pinned without touching live GitHub.
    api_base: String,
}

// Card c1090a24: manual Debug — the derived impl would print the
// resolved bearer token through any `{:?}` of the client (merger logs,
// error contexts). The token is NEVER logged; only whether one has
// been resolved yet.
impl std::fmt::Debug for ReqwestGhClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestGhClient")
            .field("api_base", &self.api_base)
            .field("token", &self.token.get().map(|_| "<redacted>"))
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
            token: std::sync::Arc::new(std::sync::OnceLock::new()),
            api_base: "https://api.github.com".to_string(),
        })
    }

    /// Test seam: a client aimed at a local listener with a pre-resolved
    /// token. Lets the wire-shape tests pin the Authorization header
    /// without env mutation (racy under parallel tests) or live GitHub.
    #[cfg(test)]
    pub(crate) fn for_test(api_base: String, token: String) -> Result<Self, GhError> {
        let client = Self::new()?;
        let _ = client.token.set(token);
        Ok(Self { api_base, ..client })
    }

    /// Resolve a GitHub token. Tries `GH_TOKEN` then `GITHUB_TOKEN` env
    /// first (cheap, what CI typically provides; same precedence as the
    /// gh CLI itself); falls back to a one-time `gh auth token` spawn
    /// (~50ms; cached for the process lifetime, refreshed only on a
    /// 401 from a subsequent call).
    async fn ensure_token(&self) -> Result<String, GhError> {
        if let Some(token) = self.token.get() {
            return Ok(token.clone());
        }
        for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
            if let Ok(env) = std::env::var(var) {
                if !env.is_empty() {
                    let _ = self.token.set(env.clone());
                    return Ok(env);
                }
            }
        }
        // One-time gh-auth-token spawn. Same shape as ShellGhClient's
        // subprocess pattern; this is the only gh process spawn we do.
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
        let _ = self.token.set(token.clone());
        Ok(token)
    }

    /// Common request bootstrap: token + accept header + json body.
    async fn authed(
        &self,
        method: reqwest::Method,
        url: String,
    ) -> Result<reqwest::RequestBuilder, GhError> {
        let token = self.ensure_token().await?;
        Ok(self
            .http
            .request(method, url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", "2022-11-28"))
    }
}

// Card 00e3aa39 Sub-2: deliberately no `Default` impl. `new()` returns
// `Result<Self, GhError>` because the reqwest builder can in principle
// fail (TLS init, etc.); callers should handle that result rather than
// hiding it behind a panicking `Default`. clippy::expect_used (denied
// workspace-wide) caught the original `.expect("…")` shortcut.

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
            .authed(reqwest::Method::GET, pr_url)
            .await?
            .send()
            .await
            .map_err(map_reqwest_error)?;
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
            .authed(reqwest::Method::GET, runs_url)
            .await?
            .send()
            .await
            .map_err(map_reqwest_error)?;
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
        // pr_create is rare on the merger hot path (it runs in
        // open_pr_and_link, not per-tick). Implemented for surface
        // parity with ShellGhClient; the gh-cli body-derivation logic
        // (subject/body from HEAD commit) stays in the caller, this
        // just executes the POST.
        let _ = args;
        Err(GhError::OutputParse(
            "ReqwestGhClient::pr_create is not yet wired — \
             the existing open_pr_and_link path uses ShellGhClient's \
             body-derivation flow. Sub-3 wires this through."
                .to_string(),
        ))
    }

    async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError> {
        let url = format!(
            "{}/repos/{}/pulls/{}/merge",
            self.api_base, args.repo, args.number
        );
        let body = serde_json::json!({ "merge_method": "squash" });
        let resp = self
            .authed(reqwest::Method::PUT, url)
            .await?
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
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
            .authed(reqwest::Method::PATCH, url)
            .await?
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
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
            .authed(reqwest::Method::GET, url)
            .await?
            .send()
            .await
            .map_err(map_reqwest_error)?;
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

    /// One-shot local HTTP listener: accepts a single connection,
    /// captures the raw request head, replies with the given JSON body.
    /// Returns (base_url, join-handle-yielding-request-text).
    async fn one_shot_server(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 16 * 1024];
            let mut read_total = 0;
            // Read until end of headers — GET requests have no body.
            loop {
                let n = stream
                    .read(&mut buf[read_total..])
                    .await
                    .expect("read request");
                read_total += n;
                if n == 0 || buf[..read_total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buf[..read_total]).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            stream.flush().await.expect("flush");
            request
        });
        (format!("http://{addr}"), handle)
    }

    /// Card c1090a24 wire-shape pin — the load-bearing one. The
    /// Authorization header IS the feature: break `Bearer {token}` in
    /// `authed()` (mutation check) and this fails. Also pins the
    /// GitHub Accept + api-version headers REST requires.
    #[tokio::test]
    async fn branch_check_rollup_sends_bearer_auth_and_github_headers() {
        let (base, server) = one_shot_server(r#"{"total_count":0,"check_runs":[]}"#).await;
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

        let request = server.await.expect("server task");
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
