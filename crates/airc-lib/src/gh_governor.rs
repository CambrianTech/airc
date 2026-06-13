//! The single GitHub-request governor.
//!
//! Per `docs/architecture/ACCOUNT-MESH-JOIN-CONTRACT.md` ("Gist
//! Boundary"), nothing may call GitHub except through a coordinator that
//! owns "TTL, singleflight, and backoff." The budget/backoff state was
//! historically file-locked under `~/.airc/gh/` but only enforced by the
//! `airc gh` CLI subcommand — the periodic account-registry loop (the
//! biggest gh consumer, in this crate) spawned raw `gh` around it, so the
//! counter saw only a fraction of the traffic. This module is that state
//! and policy as ONE crate-agnostic source of truth, so both `airc-lib`
//! (registry store) and `airc-cli` route through the same file-locked
//! counter — a single, inspectable gh footprint with no bypass.
//!
//! ## What it does
//!
//! - **Counter / budget:** a sliding 60s window of request timestamps
//!   (`budget.jsonl`). `reserve` refuses once the window hits
//!   `max_requests_per_min` (default 30) and arms a local backoff.
//! - **Backoff:** a shared `backoff-until` epoch (`note_rate_limit`
//!   parses GitHub's own headers — `retry-after`,
//!   `x-ratelimit-remaining`/`reset` — so the governor honors GitHub's
//!   real quota instead of guessing).
//! - **Cross-process:** an `fs2` exclusive file lock serializes the
//!   read-modify-write so every tab/daemon/process on the machine shares
//!   ONE budget. The state dir defaults to `~/.airc/gh/`; tests point it
//!   at an isolated dir.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

/// Default per-minute request budget. Conservative on purpose: the
/// account-registry loop only needs a handful of calls per cadence, so a
/// healthy mesh never approaches this. Overridable via
/// `AIRC_GH_MAX_REQUESTS_PER_MIN` for operators with larger fleets.
pub const DEFAULT_MAX_REQUESTS_PER_MIN: usize = 30;

/// How long to self-throttle after blowing the local per-minute budget.
const LOCAL_THROTTLE_BACKOFF_SEC: f64 = 60.0;

/// Outcome of asking the governor for permission to make a gh request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// Caller may spawn gh now. The request was recorded against the
    /// budget if it is a quota-relevant command.
    Allowed,
    /// Caller must NOT call gh. `retry_after_secs` is how long until the
    /// governor would allow it; `reason` is human-facing.
    Denied {
        retry_after_secs: i64,
        reason: String,
    },
}

impl Reservation {
    pub fn allowed(&self) -> bool {
        matches!(self, Reservation::Allowed)
    }
}

/// The machine-wide gh budget rooted at a state directory. Construct with
/// [`GhBudget::account_default`] for the shared production budget (so all
/// processes coordinate) or [`GhBudget::at`] for an isolated test dir.
#[derive(Debug, Clone)]
pub struct GhBudget {
    dir: PathBuf,
}

impl GhBudget {
    /// Production budget under `~/.airc/gh/` — the SAME files the
    /// `airc-cli` governor uses, so cli and lib share one counter.
    pub fn account_default() -> Self {
        Self { dir: default_dir() }
    }

    /// Budget rooted at an explicit dir. Used by tests for isolation and
    /// by callers that pin a non-default account home.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Reserve a slot for a gh invocation described by `args` (the `gh`
    /// argv, e.g. `["api", "/gists/..."]`). Quota-relevant commands are
    /// recorded against the sliding window; non-quota commands (e.g. a
    /// local `gh --version`) are allowed without consuming budget.
    /// Cross-process safe: the whole check-and-record happens under the
    /// shared file lock.
    pub fn reserve(&self, args: &[String], now: f64) -> std::io::Result<Reservation> {
        let _lock = self.lock()?;
        let until = self.read_backoff();
        if now < until {
            return Ok(Reservation::Denied {
                retry_after_secs: (until - now).ceil() as i64,
                reason: format!("shared gh backoff active for {}s", (until - now) as i64),
            });
        }
        if !is_quota_relevant(args) {
            return Ok(Reservation::Allowed);
        }
        let count = self.recent_count(now)?;
        let limit = max_requests_per_min();
        if count >= limit {
            self.write_backoff(now + LOCAL_THROTTLE_BACKOFF_SEC)?;
            return Ok(Reservation::Denied {
                retry_after_secs: LOCAL_THROTTLE_BACKOFF_SEC as i64,
                reason: format!("gh request budget exceeded ({count}/{limit} in 60s)"),
            });
        }
        self.record(now)?;
        Ok(Reservation::Allowed)
    }

    /// `(used_in_last_60s, limit)`. For `airc gh budget` / tests /
    /// the churn harness's footprint assertion.
    pub fn snapshot(&self, now: f64) -> std::io::Result<(usize, usize)> {
        let _lock = self.lock()?;
        Ok((self.recent_count(now)?, max_requests_per_min()))
    }

    /// Feed GitHub's OWN signal back into the governor: parse a gh
    /// `--include` response (status line + headers) or any text carrying
    /// `retry-after` / `x-ratelimit-remaining` / `x-ratelimit-reset` and,
    /// when GitHub says we're out of quota, arm the shared backoff until
    /// its reset. This is how the governor honors the real limit instead
    /// of only its local 30/min guess.
    pub fn note_rate_limit(&self, response_text: &str) {
        let body = response_text.to_ascii_lowercase();
        if body.is_empty() {
            return;
        }
        let now = now_seconds();
        let mut until = 0.0;
        if let Some(retry) =
            header_value(&body, "retry-after").and_then(|v| v.trim().parse::<f64>().ok())
        {
            until = now + retry.max(1.0);
        } else {
            let remaining = header_value(&body, "x-ratelimit-remaining");
            let reset =
                header_value(&body, "x-ratelimit-reset").and_then(|v| v.trim().parse::<f64>().ok());
            if remaining.as_deref().map(str::trim) == Some("0") {
                if let Some(reset) = reset {
                    until = reset;
                }
            } else if body.contains("secondary rate limit")
                || body.contains("rate limit exceeded")
                || body.contains("abuse detection")
            {
                until = now + 60.0;
            }
        }
        if until > now {
            let _ = self.write_backoff(until);
        }
    }

    /// Seconds until the shared backoff clears (0 if none).
    pub fn backoff_wait_secs(&self, now: f64) -> i64 {
        (self.read_backoff() - now).max(0.0).ceil() as i64
    }

    // --- internals (file-locked state under `dir`) ---

    fn lock(&self) -> std::io::Result<GuardLock> {
        fs::create_dir_all(&self.dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(self.dir.join("guard.lock"))?;
        file.lock_exclusive()?;
        Ok(GuardLock(file))
    }

    fn read_backoff(&self) -> f64 {
        fs::read_to_string(self.dir.join("backoff-until"))
            .ok()
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    fn write_backoff(&self, until: f64) -> std::io::Result<()> {
        if until <= now_seconds() {
            return Ok(());
        }
        let until = until.max(self.read_backoff());
        let path = self.dir.join("backoff-until");
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, format!("{}", until as i64))?;
        fs::rename(tmp, path)
    }

    /// Count timestamps within the last 60s, rewriting the file pruned to
    /// the window (keeps `budget.jsonl` bounded).
    fn recent_count(&self, now: f64) -> std::io::Result<usize> {
        let path = self.dir.join("budget.jsonl");
        let cutoff = now - 60.0;
        let kept: Vec<f64> = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<f64>().ok())
            .filter(|ts| *ts >= cutoff)
            .collect();
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = File::create(&tmp)?;
        for ts in &kept {
            writeln!(file, "{ts:.3}")?;
        }
        fs::rename(tmp, path)?;
        Ok(kept.len())
    }

    fn record(&self, now: f64) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("budget.jsonl"))?;
        writeln!(file, "{now:.3}")
    }
}

struct GuardLock(File);
impl Drop for GuardLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Which gh commands consume rate-limit budget. `api` and `gist` are real
/// REST traffic; `auth status` is the cheap-but-throttleable probe the
/// gate runs. Everything else (`--version`, local config) is free.
pub fn is_quota_relevant(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("api" | "gist"))
        || matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("auth"), Some("status"))
        )
}

fn header_value(body: &str, name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

fn max_requests_per_min() -> usize {
    std::env::var("AIRC_GH_MAX_REQUESTS_PER_MIN")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_REQUESTS_PER_MIN)
}

/// `~/.airc/gh/` — the shared production state dir (same as the cli
/// governor). Falls back to a temp `.airc` when no home env exists so the
/// governor still functions headlessly rather than panicking.
fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".airc"))
        .unwrap_or_else(|| std::env::temp_dir().join(".airc"))
        .join("gh")
}

pub fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Split a gh `--include` response into `(headers, body)`. Exposed so the
/// store can hand headers to [`GhBudget::note_rate_limit`] and the body to
/// its JSON parse.
pub fn split_include_output(raw: &str) -> (String, String) {
    let normalized = raw.replace("\r\n", "\n");
    if normalized.starts_with("HTTP/") {
        if let Some(index) = normalized.find("\n\n") {
            let (headers, body) = normalized.split_at(index);
            return (headers.to_string(), body.trim_start().to_string());
        }
    }
    (String::new(), raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn budget(dir: &Path) -> GhBudget {
        GhBudget::at(dir)
    }

    #[test]
    fn quota_relevant_only_for_api_gist_authstatus() {
        // what this catches: free commands (--version) must not consume
        // budget; only real REST traffic does.
        assert!(is_quota_relevant(&["api".into(), "/gists".into()]));
        assert!(is_quota_relevant(&["gist".into(), "edit".into()]));
        assert!(is_quota_relevant(&["auth".into(), "status".into()]));
        assert!(!is_quota_relevant(&["--version".into()]));
        assert!(!is_quota_relevant(&["auth".into(), "token".into()]));
    }

    #[test]
    fn reserve_allows_until_budget_then_denies_and_backs_off() {
        // what this catches: the per-minute cap actually fires, and once
        // tripped arms a backoff so callers stop hammering.
        let tmp = tempfile::TempDir::new().unwrap();
        let b = budget(tmp.path());
        std::env::set_var("AIRC_GH_MAX_REQUESTS_PER_MIN", "3");
        let args = vec!["api".to_string(), "/gists".to_string()];
        let now = 1_000.0;
        for _ in 0..3 {
            assert!(b.reserve(&args, now).unwrap().allowed());
        }
        let denied = b.reserve(&args, now).unwrap();
        assert!(!denied.allowed(), "4th call over a budget of 3 must deny");
        // a follow-up call is still denied by the armed backoff
        assert!(!b.reserve(&args, now).unwrap().allowed());
        std::env::remove_var("AIRC_GH_MAX_REQUESTS_PER_MIN");
    }

    #[test]
    fn non_quota_command_never_consumes_budget() {
        // what this catches: spamming `gh --version` can't exhaust the
        // REST budget.
        let tmp = tempfile::TempDir::new().unwrap();
        let b = budget(tmp.path());
        let args = vec!["--version".to_string()];
        for _ in 0..100 {
            assert!(b.reserve(&args, 1_000.0).unwrap().allowed());
        }
        assert_eq!(b.snapshot(1_000.0).unwrap().0, 0);
    }

    #[test]
    fn note_rate_limit_arms_backoff_until_reset_when_remaining_zero() {
        // what this catches: GitHub saying "0 remaining, reset at T" must
        // block all gh until T — honoring the real quota, not our guess.
        let tmp = tempfile::TempDir::new().unwrap();
        let b = budget(tmp.path());
        let future = now_seconds() + 300.0;
        b.note_rate_limit(&format!(
            "x-ratelimit-remaining: 0\nx-ratelimit-reset: {}\n",
            future as i64
        ));
        let denied = b
            .reserve(&["api".into(), "/gists".into()], now_seconds())
            .unwrap();
        assert!(!denied.allowed(), "must respect GitHub's reset window");
        assert!(b.backoff_wait_secs(now_seconds()) > 0);
    }

    #[test]
    fn note_rate_limit_honors_retry_after_seconds() {
        // what this catches: secondary-limit `retry-after` header.
        let tmp = tempfile::TempDir::new().unwrap();
        let b = budget(tmp.path());
        b.note_rate_limit("retry-after: 120\n");
        assert!(b.backoff_wait_secs(now_seconds()) >= 100);
    }

    #[test]
    fn split_include_separates_headers_from_body() {
        let (h, body) = split_include_output("HTTP/2 200\nx-ratelimit-remaining: 9\n\n{\"ok\":1}");
        assert!(h.contains("x-ratelimit-remaining: 9"));
        assert_eq!(body, "{\"ok\":1}");
    }
}
