//! GitHub-gist-backed [`AccountRegistryStore`] — the cross-machine
//! bootstrap path.
//!
//! Each machine publishes its [`AccountRegistryDocument`] to a gist
//! owned by the user's authenticated GitHub account. Discovery is
//! "list all of this user's gists with the magic description, pull
//! their contents, merge." Two machines on the same GitHub account
//! both running `airc join` thus auto-converge on each other's peer
//! records, route candidates, and channel subscriptions without any
//! pasted invite.
//!
//! ## Contract
//!
//! - **Filename:** `airc-account-mesh-registry.<writer-key>.json`
//!   (single file per gist; gist private). The writer key derives
//!   from MACHINE identity ([`writer_key`]) so a writer can re-find
//!   its own gist even after its local state rotates. Legacy gists
//!   named `airc-account-mesh-registry.json` stay readable — readers
//!   are filename-agnostic (they take the gist's first file) and
//!   sentinel-tracked edits reuse the gist's existing filename.
//! - **Description marker:** `airc-account-mesh-registry` (used for
//!   discovery filtering on `gh gist list`)
//! - **One gist per machine, find-or-update.** The per-mesh-identity
//!   gist-id sentinel row in `account_registry_gist_sentinel` records
//!   this machine's gist id so subsequent publishes update the same
//!   gist instead of creating duplicates. If the sentinel is missing,
//!   the publisher RE-FINDS its own gist by writer filename before
//!   ever creating one (card d793c242: create-on-miss proliferated
//!   duplicate registry gists every time local identity rotated).
//! - **Refresh merges all matching gists.** A scope joining on a
//!   third+ machine sees both other machines' beacons without any
//!   server-side coordination. The merge keeps the freshest beacon
//!   per peer and drops temp-scoped phantom test peers.
//!
//! ## Hermetic gate (card d793c242)
//!
//! Hermetic test daemons inherit the operator's working gh auth, so
//! without a gate they publish test identities to the PRODUCTION
//! account rendezvous. [`account_registry_block`] blocks the gh
//! transport when `AIRC_DISABLE_ACCOUNT_REGISTRY` is set (intentional
//! harness gate) or the scope home is temp-rooted (defense in depth).
//! Enforced at daemon startup, per refresh-loop tick, AND inside this
//! store — loudly, never silently.
//!
//! ## Why not a single shared gist
//!
//! Single-gist designs require concurrency control (read-modify-write
//! between machines on the same account). With per-machine gists,
//! each writer owns its own file and the reader merges — no write
//! contention, no race, no need for a refresh lock at the registry
//! layer.
//!
//! ## Boundary
//!
//! This is the ONLY gh-gist surface in the rust-rewrite cross-machine
//! path. Routine messages, transcript events, and media never touch
//! gh. The registry document carries beacons + route candidates only.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_store::{SqliteEventStore, StoredAccountRegistryGistSentinel};

use crate::account_registry::{
    merge_registry_documents, prune_stale_peers, scope_home_is_temp_rooted,
    AccountRegistryDocument, AccountRegistryError, AccountRegistryStore,
    DEFAULT_PEER_FRESHNESS_TTL_MS,
};
use crate::subscriptions::MeshIdentity;
use crate::time;

/// Description marker used by `gh gist list` for discovery. Stable
/// across versions — bumping this constant would orphan existing
/// registries.
const REGISTRY_DESCRIPTION: &str = "airc-account-mesh-registry";

/// Hermetic gate env var (card d793c242). When set (non-empty, not
/// `"0"`), this process must NEVER touch the gh account rendezvous —
/// neither publish nor refresh. Test-daemon spawn helpers set it
/// unconditionally; operators can set it to opt a box out.
pub const AIRC_DISABLE_ACCOUNT_REGISTRY_ENV: &str = "AIRC_DISABLE_ACCOUNT_REGISTRY";

/// Why the gh account-registry transport is blocked for this scope.
///
/// Live evidence for this gate (card d793c242, 2026-06-12 ~01:11Z): a
/// hermetic Windows test daemon with scope_home
/// `C:\Users\green\AppData\Local\Temp\tmp.YYavgmVUxz\.airc` inherited
/// the operator's gh auth and published itself to the REAL joelteply
/// rendezvous (gist 1214fb43d2c00d667c4712e6023b2165) — reader-merge
/// then enrolled the phantom test peer and auto-dialed its garbage
/// endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountRegistryBlock {
    /// `AIRC_DISABLE_ACCOUNT_REGISTRY` is set — the intentional gate
    /// the test harness arms.
    DisabledByEnv,
    /// The scope home resolves under a temp directory — defense in
    /// depth for hermetic daemons whose harness forgot the env var.
    TempScopeHome { scope_home: PathBuf },
}

impl std::fmt::Display for AccountRegistryBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DisabledByEnv => write!(
                f,
                "{AIRC_DISABLE_ACCOUNT_REGISTRY_ENV} is set — this scope must not touch the \
                 gh account rendezvous (hermetic gate, card d793c242)"
            ),
            Self::TempScopeHome { scope_home } => write!(
                f,
                "scope home {} is temp-rooted — hermetic test/CI daemons must NEVER publish \
                 to the production account rendezvous (card d793c242)",
                scope_home.display()
            ),
        }
    }
}

/// The hermetic gate: returns why the gh account-registry transport is
/// blocked for `scope_home`, or `None` for production scopes. Checked
/// at daemon startup (loop not spawned), per refresh-loop tick, and —
/// belt and braces — inside [`GhAccountRegistryStore`] itself so no
/// caller can route around it. The env check deliberately precedes the
/// temp check so a blocked publish names the INTENTIONAL gate when
/// both apply.
pub fn account_registry_block(scope_home: &Path) -> Option<AccountRegistryBlock> {
    if disable_env_set() {
        return Some(AccountRegistryBlock::DisabledByEnv);
    }
    if scope_home_is_temp_rooted(scope_home) {
        return Some(AccountRegistryBlock::TempScopeHome {
            scope_home: scope_home.to_path_buf(),
        });
    }
    None
}

fn disable_env_set() -> bool {
    match std::env::var_os(AIRC_DISABLE_ACCOUNT_REGISTRY_ENV) {
        Some(value) => {
            let value = value.to_string_lossy();
            let trimmed = value.trim();
            !trimmed.is_empty() && trimmed != "0"
        }
        None => false,
    }
}

/// Stable per-writer key: `<host>-<user>`, sanitized. Derives from
/// MACHINE identity (hostname + user), NOT from the rotating
/// peer/mesh identity — so the same box always maps to the same
/// registry gist even when its local sentinel or events.sqlite is
/// wiped. Create-on-miss keyed off rotating identity is exactly what
/// proliferated 5+ duplicate registry gists under joelteply.
pub fn writer_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let host = first_nonempty_env(&["HOSTNAME", "COMPUTERNAME"])
            .or_else(hostname_from_command)
            .unwrap_or_else(|| "unknown-host".to_string());
        let user = first_nonempty_env(&["USER", "LOGNAME", "USERNAME"])
            .unwrap_or_else(|| "unknown-user".to_string());
        format!(
            "{}-{}",
            sanitize_writer_component(&host),
            sanitize_writer_component(&user)
        )
    })
}

/// Per-writer registry filename: the stable marker this writer uses to
/// re-find its own gist among the account's registry gists.
pub fn writer_filename() -> String {
    format!("airc-account-mesh-registry.{}.json", writer_key())
}

/// What [`classify_registry_gc`] decided for one own-account gist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcAction {
    /// A real machine's gist (or an unrecognized file) — never deleted.
    Keep,
    /// Provable junk — safe to delete.
    Delete,
}

/// One gist's gc verdict: id + filename + action + a human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcVerdict {
    pub id: String,
    pub filename: String,
    pub action: GcAction,
    pub reason: String,
}

/// Outcome of [`GhAccountRegistryStore::gc`]: every gist's verdict plus
/// counts. `deleted` is what was actually removed (0 in a dry run);
/// `applied` says whether deletions were performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub verdicts: Vec<GcVerdict>,
    pub kept: usize,
    /// Number marked for deletion (the plan size, dry-run or not).
    pub to_delete: usize,
    /// Number actually deleted (== to_delete on a clean apply, 0 on dry run).
    pub deleted: usize,
    pub applied: bool,
}

/// Shape of a registry gist's first filename.
enum RegistryFilename {
    /// Legacy `airc-account-mesh-registry.json` (pre-writer-key).
    Legacy,
    /// `airc-account-mesh-registry.<key>.json` — `<key>` is `<host>-<user>`.
    Keyed(String),
    /// Anything else — not a recognized registry filename.
    Other,
}

fn parse_registry_filename(filename: &str) -> RegistryFilename {
    if filename == "airc-account-mesh-registry.json" {
        return RegistryFilename::Legacy;
    }
    if let Some(stem) = filename.strip_suffix(".json") {
        if let Some(key) = stem.strip_prefix("airc-account-mesh-registry.") {
            if !key.is_empty() {
                return RegistryFilename::Keyed(key.to_string());
            }
        }
    }
    RegistryFilename::Other
}

/// Pure, reader-side classification of own-account registry gists into
/// keep vs delete. The conservative v1 policy deletes only the two
/// PROVABLY-junk categories and keeps every real machine's gist:
///
/// - `airc-account-mesh-registry.<hex>-unknown-user.json` — an
///   identity-less publisher (a CI runner or throwaway container with
///   no resolvable host/user; cf the mesh-converge harness). A real
///   desktop resolves a `<host>-<user>` key, so an `unknown-user` gist
///   is never a real machine.
/// - `airc-account-mesh-registry.json` — a LEGACY, pre-writer-key gist.
///   The find-or-update scheme (card d793c242) keys gists by machine,
///   so these unnamed ones are superseded duplicates; any live machine
///   republishes under its `<host>-<user>` filename.
/// - `airc-account-mesh-registry.<host>-<user>.json` — a real machine's
///   gist. KEPT. (Deduping multiple gists for the SAME real key, and
///   pruning gists whose beacons are all stale, are future opt-in
///   passes — v1 never touches a real writer-keyed gist.)
/// - anything unrecognized — KEPT (never delete a file we don't model).
///
/// Filename-only and side-effect-free, so the policy is unit-testable
/// and mutation-verifiable without any gh I/O.
pub fn classify_registry_gc(gists: &[(String, String)]) -> Vec<GcVerdict> {
    gists
        .iter()
        .map(|(id, filename)| {
            let (action, reason) = match parse_registry_filename(filename) {
                RegistryFilename::Legacy => (
                    GcAction::Delete,
                    "legacy pre-writer-key gist (superseded duplicate)".to_string(),
                ),
                RegistryFilename::Keyed(key) if key.ends_with("-unknown-user") => (
                    GcAction::Delete,
                    format!("identity-less publisher ({key}) — never a real machine"),
                ),
                RegistryFilename::Keyed(key) => {
                    (GcAction::Keep, format!("real machine gist ({key})"))
                }
                RegistryFilename::Other => (
                    GcAction::Keep,
                    "unrecognized filename — left untouched".to_string(),
                ),
            };
            GcVerdict {
                id: id.clone(),
                filename: filename.clone(),
                action,
                reason,
            }
        })
        .collect()
}

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn hostname_from_command() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let host = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn sanitize_writer_component(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let mut component: String = trimmed.chars().take(24).collect();
    while component.ends_with('-') {
        component.pop();
    }
    if component.is_empty() {
        "unknown".to_string()
    } else {
        component
    }
}

/// Shared fresh-token slot for a daemon's gh transport (card 1f2cbffa).
///
/// `GH_TOKEN` is injected into the daemon's environment ONCE at spawn
/// (the CLI's `inject_gh_token`) because a detached daemon can't always
/// reach the OS keyring. But gh tokens rotate mid-session (documented
/// operational fact on the shared joelteply identity) and process env
/// is immutable — so a long-lived daemon holding a rotated/revoked
/// snapshot fails every registry tick until restart. When the gate's
/// auth probe fails, it makes ONE recovery attempt per tick
/// ([`re_resolve_gh_token`], mirroring `ReqwestGhClient`'s #1147
/// 401-refresh); a recovered token lands here, and every subsequent gh
/// spawn — the gate's `gh auth status` probe AND the store's
/// publish/refresh commands — overrides the stale env copy with it via
/// `cmd.env("GH_TOKEN", ...)`.
#[derive(Clone, Default)]
pub struct GhTokenOverride {
    inner: Arc<std::sync::RwLock<Option<String>>>,
}

impl GhTokenOverride {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the recovered token, surviving lock poisoning (a panicked
    /// holder can't corrupt an `Option<String>` — either value is a
    /// valid state; same posture as `ReqwestGhClient::cached_token`).
    pub fn get(&self) -> Option<String> {
        match self.inner.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Replace the recovered token.
    pub fn set(&self, token: String) {
        let mut guard = match self.inner.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(token);
    }
}

// Manual Debug — the derived impl would print the live token through
// any `{:?}` (RegistryRefreshGate derives Debug and carries this).
// Tokens are NEVER logged; only whether one has been recovered.
impl std::fmt::Debug for GhTokenOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhTokenOverride")
            .field("token", &self.get().map(|_| "<redacted>"))
            .finish()
    }
}

/// gh-gist-backed account-registry store.
#[derive(Clone)]
pub struct GhAccountRegistryStore {
    gh_bin: PathBuf,
    store: Arc<SqliteEventStore>,
    /// The publishing scope's AIRC_HOME — the hermetic gate's input.
    /// Required at construction so no gh-backed store can exist
    /// without a gate (no silent ungated path).
    scope_home: PathBuf,
    /// Daemon-shared recovered-token slot (card 1f2cbffa). When
    /// populated, every gh spawn carries it as `GH_TOKEN`, overriding
    /// a stale spawn-time env snapshot. `None` outside the daemon loop
    /// (a manual `registry sync` runs in the operator's live session,
    /// where env/keyring are already current).
    token_override: Option<GhTokenOverride>,
    /// The single gh request governor. Every `gh_run` reserves against
    /// this before spawning and feeds the response back through
    /// `note_rate_limit`, so the registry loop — the biggest gh consumer
    /// — can no longer spam GitHub around the counter. Defaults to the
    /// shared account budget (`~/.airc/gh/`) so it coordinates with the
    /// cli governor and every other scope; tests inject an isolated one.
    budget: crate::gh::governor::GhBudget,
}

impl GhAccountRegistryStore {
    /// Construct a new store. The `store` handle is where this
    /// adapter persists the per-mesh-identity gist-id sentinel that
    /// lets subsequent publishes update the same gist rather than
    /// creating duplicates. Typically the same SqliteEventStore the
    /// machine-account home (`~/.airc/events.sqlite`) uses, but any
    /// store with the account_registry tables works. `scope_home` is
    /// the publishing scope's AIRC_HOME, fed to the hermetic gate
    /// ([`account_registry_block`]) on every publish/refresh.
    pub fn new(store: Arc<SqliteEventStore>, scope_home: impl Into<PathBuf>) -> Self {
        Self {
            gh_bin: PathBuf::from(
                std::env::var_os("AIRC_GH_BIN")
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "gh".into()),
            ),
            store,
            scope_home: scope_home.into(),
            token_override: None,
            budget: crate::gh::governor::GhBudget::account_default(),
        }
    }

    /// Override the `gh` binary path. Used in tests.
    pub fn with_bin(mut self, gh_bin: impl Into<PathBuf>) -> Self {
        self.gh_bin = gh_bin.into();
        self
    }

    /// Inject an isolated gh budget (test-only): point the governor at a
    /// throwaway dir so a test asserts the registry store's gh footprint
    /// without touching the operator's real `~/.airc/gh/` counter.
    pub fn with_budget(mut self, budget: crate::gh::governor::GhBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Attach the daemon-shared recovered-token slot (card 1f2cbffa).
    /// The daemon hands the SAME slot to its `RegistryRefreshGate`, so
    /// a token the gate recovers after a stale-auth tick reaches this
    /// store's gh spawns too.
    pub fn with_token_override(mut self, token_override: GhTokenOverride) -> Self {
        self.token_override = Some(token_override);
        self
    }

    async fn load_gist_id(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<String>, AccountRegistryError> {
        let row = self
            .store
            .load_account_registry_gist_sentinel(mesh_identity.as_str())
            .await
            .map_err(|error| {
                AccountRegistryError::Adapter(format!("load gist sentinel: {error}"))
            })?;
        Ok(row.and_then(|s| {
            let trimmed = s.gist_id.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }))
    }

    async fn save_gist_id(
        &self,
        mesh_identity: &MeshIdentity,
        id: &str,
    ) -> Result<(), AccountRegistryError> {
        let now_ms = time::now_ms().map_err(|error| {
            AccountRegistryError::Adapter(format!("clock for gist sentinel save: {error}"))
        })?;
        self.store
            .save_account_registry_gist_sentinel(StoredAccountRegistryGistSentinel {
                mesh_identity: mesh_identity.as_str().to_string(),
                gist_id: id.to_string(),
                updated_at_ms: now_ms,
            })
            .await
            .map_err(|error| AccountRegistryError::Adapter(format!("save gist sentinel: {error}")))
    }

    async fn clear_gist_id(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<(), AccountRegistryError> {
        self.store
            .clear_account_registry_gist_sentinel(mesh_identity.as_str())
            .await
            .map_err(|error| AccountRegistryError::Adapter(format!("clear gist sentinel: {error}")))
    }

    async fn gh_run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<(bool, String, String), AccountRegistryError> {
        // Single chokepoint: reserve against the governor before any gh
        // spawn. A denial (local 60s budget blown, or GitHub's own
        // rate-limit backoff active) skips the call — the refresh loop
        // catches the error and waits for the next tick, which is the
        // correct "don't spam gh" behavior, not a hard failure.
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        match self
            .budget
            .reserve(&owned, crate::gh::governor::now_seconds())
        {
            Ok(crate::gh::governor::Reservation::Denied {
                retry_after_secs,
                reason,
            }) => {
                return Err(AccountRegistryError::Adapter(format!(
                    "gh governor: {reason}; retry in {retry_after_secs}s"
                )));
            }
            // Allowed, or a governor I/O glitch: fail OPEN so a filesystem
            // hiccup can't brick the mesh — the 60s budget + GitHub's
            // headers remain the backstop.
            Ok(crate::gh::governor::Reservation::Allowed) | Err(_) => {}
        }
        let mut cmd = Command::new(&self.gh_bin);
        cmd.args(args);
        // Recovered-token override (card 1f2cbffa): a fresh token the
        // gate re-resolved after a stale-auth tick beats the daemon's
        // immutable spawn-time `GH_TOKEN` env snapshot.
        if let Some(token) = self.token_override.as_ref().and_then(GhTokenOverride::get) {
            cmd.env("GH_TOKEN", token);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        gh_no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|error| {
            AccountRegistryError::Adapter(format!("spawn gh {}: {error}", args.join(" ")))
        })?;
        if let (Some(input), Some(mut handle)) = (stdin, child.stdin.take()) {
            handle.write_all(input.as_bytes()).await.map_err(|error| {
                AccountRegistryError::Adapter(format!("write gh stdin: {error}"))
            })?;
            handle.shutdown().await.map_err(|error| {
                AccountRegistryError::Adapter(format!("close gh stdin: {error}"))
            })?;
        }
        let output = child.wait_with_output().await.map_err(|error| {
            AccountRegistryError::Adapter(format!("wait gh {}: {error}", args.join(" ")))
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // Feed GitHub's own signal back into the shared governor on EVERY
        // response (success included): with `--include`, gh surfaces the
        // `x-ratelimit-*` headers GitHub returns for all requests, so the
        // governor keeps a live per-machine quota snapshot and throttles
        // at the floor; on a limit response the `retry-after` /
        // secondary-limit text arms the shared backoff until reset. Every
        // scope on the machine then honors GitHub's real quota, not a guess.
        self.budget.note_rate_limit(&stdout);
        self.budget.note_rate_limit(&stderr);
        Ok((output.status.success(), stdout, stderr))
    }

    /// List the authenticated account's registry gists (one per
    /// writer/machine). Shared by refresh (merge them all) and by the
    /// publish find-or-update path (re-find OUR gist by writer
    /// filename when the local sentinel is gone).
    ///
    /// Discovery scans only the user's MOST RECENT 100 gists. The
    /// account-mesh-registry beacons are updated on a cadence, so they
    /// stay at the top of the recency-sorted list — no `--paginate`
    /// (which can take minutes for high-gist-count operators).
    async fn list_registry_gists(&self) -> Result<Vec<GistListEntry>, AccountRegistryError> {
        let (ok, stdout, stderr) = self
            .gh_run(
                &[
                    "api",
                    "/gists?per_page=100",
                    "--jq",
                    // Filter to gists whose description matches and which
                    // contain a registry file. Returns one line of JSON
                    // per match: {"id":"...","filename":"..."}.
                    "[.[] | select(.description == \"airc-account-mesh-registry\") | {id, filename: (.files | keys | .[0])}] | .[]",
                ],
                None,
            )
            .await?;
        if !ok {
            return Err(AccountRegistryError::Adapter(format!(
                "gh api /gists failed: {stderr}"
            )));
        }
        Ok(stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }

    /// Fetch one registry gist's document. Individual fetch/parse
    /// failures return `None` (foreign or corrupt gists are skipped,
    /// not fatal — the reader merges what it can read).
    async fn fetch_gist_document(
        &self,
        entry: &GistListEntry,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError> {
        let (ok, content, _stderr) = self
            .gh_run(
                &[
                    "api",
                    &format!("/gists/{}", entry.id),
                    "--jq",
                    &format!(".files[\"{}\"].content", entry.filename),
                ],
                None,
            )
            .await?;
        if !ok {
            return Ok(None);
        }
        Ok(serde_json::from_str(content.trim()).ok())
    }

    /// Garbage-collect this account's registry gists: delete the
    /// provably-junk ones ([`classify_registry_gc`]) so a converging
    /// reader fetches one gist per real machine instead of a swamp of
    /// identity-less / legacy duplicates (each extra gist is a per-tick
    /// gh fetch). Dry-run when `apply == false`: classify + report, no
    /// deletes. Honors the hermetic gate — a hermetic scope must not
    /// mutate the production rendezvous.
    pub async fn gc(&self, apply: bool) -> Result<GcReport, AccountRegistryError> {
        if let Some(block) = account_registry_block(&self.scope_home) {
            return Err(AccountRegistryError::Adapter(format!(
                "HERMETIC GATE: refusing account-registry gc — {block}"
            )));
        }
        let entries = self.list_registry_gists().await?;
        let pairs: Vec<(String, String)> = entries
            .iter()
            .map(|e| (e.id.clone(), e.filename.clone()))
            .collect();
        // Self-guard: never delete THIS machine's own current gist,
        // whatever its key shape. The classifier deletes
        // `*-unknown-user` gists, and a real machine with every identity
        // env (USER/LOGNAME/USERNAME) unset would publish exactly such a
        // key — so force-Keep our own writer filename before any delete.
        let self_filename = writer_filename();
        let verdicts: Vec<GcVerdict> = classify_registry_gc(&pairs)
            .into_iter()
            .map(|mut verdict| {
                if verdict.action == GcAction::Delete && verdict.filename == self_filename {
                    verdict.action = GcAction::Keep;
                    verdict.reason =
                        "this machine's own current gist (never self-delete)".to_string();
                }
                verdict
            })
            .collect();
        let kept = verdicts
            .iter()
            .filter(|v| v.action == GcAction::Keep)
            .count();
        let to_delete = verdicts.len() - kept;
        let mut deleted = 0usize;
        if apply {
            for verdict in verdicts.iter().filter(|v| v.action == GcAction::Delete) {
                if self.delete_gist(&verdict.id).await.is_ok() {
                    deleted += 1;
                }
            }
        }
        Ok(GcReport {
            verdicts,
            kept,
            to_delete,
            deleted,
            applied: apply,
        })
    }

    /// Delete one gist by id via `gh api --method DELETE /gists/<id>`.
    async fn delete_gist(&self, id: &str) -> Result<(), AccountRegistryError> {
        let (ok, _stdout, stderr) = self
            .gh_run(
                &["api", "--method", "DELETE", &format!("/gists/{id}")],
                None,
            )
            .await?;
        if ok {
            Ok(())
        } else {
            Err(AccountRegistryError::Adapter(format!(
                "gh api DELETE /gists/{id} failed: {stderr}"
            )))
        }
    }

    /// Probe the sentinel-recorded gist: `Found(filename)` when it
    /// still exists (filename = the file to edit in place — legacy
    /// gists keep their legacy filename), `Gone` when GitHub says 404
    /// (deleted out-of-band). Any OTHER failure is an error: clearing
    /// the sentinel on a transient network blip would recreate a
    /// duplicate gist, which is the proliferation this card kills.
    async fn probe_gist(&self, id: &str) -> Result<GistProbe, AccountRegistryError> {
        let (ok, stdout, stderr) = self
            .gh_run(
                &[
                    "api",
                    &format!("/gists/{id}"),
                    "--jq",
                    ".files | keys | .[0]",
                ],
                None,
            )
            .await?;
        if !ok {
            if stderr.contains("404") || stderr.contains("Not Found") {
                return Ok(GistProbe::Gone);
            }
            return Err(AccountRegistryError::Adapter(format!(
                "gh api /gists/{id} probe failed (sentinel kept): {stderr}"
            )));
        }
        let filename = stdout.trim();
        if filename.is_empty() || filename == "null" {
            return Ok(GistProbe::Gone);
        }
        Ok(GistProbe::Found(filename.to_string()))
    }

    async fn edit_gist(
        &self,
        id: &str,
        filename: &str,
        body: &str,
    ) -> Result<(), AccountRegistryError> {
        // `gh gist edit <id> --filename <name> -` reads new content
        // from stdin.
        let (ok, _stdout, stderr) = self
            .gh_run(
                &["gist", "edit", id, "--filename", filename, "-"],
                Some(body),
            )
            .await?;
        if !ok {
            return Err(AccountRegistryError::Adapter(format!(
                "gh gist edit {id} failed: {stderr}"
            )));
        }
        Ok(())
    }

    async fn create_gist(&self, body: &str) -> Result<String, AccountRegistryError> {
        // Create a new private gist with this writer's stable
        // filename. `gh gist create -` reads content from stdin; the
        // new gist's URL is on stdout.
        let filename = writer_filename();
        let (ok, stdout, stderr) = self
            .gh_run(
                &[
                    "gist",
                    "create",
                    "--filename",
                    &filename,
                    "--desc",
                    REGISTRY_DESCRIPTION,
                    "-",
                ],
                Some(body),
            )
            .await?;
        if !ok {
            return Err(AccountRegistryError::Adapter(format!(
                "gh gist create failed: {stderr}"
            )));
        }
        extract_gist_id(stdout.trim()).ok_or_else(|| {
            AccountRegistryError::Adapter(format!(
                "could not parse gist id from gh output: {stdout}"
            ))
        })
    }
}

/// Outcome of probing the sentinel-recorded gist.
enum GistProbe {
    /// Gist exists; payload is the filename to edit in place.
    Found(String),
    /// GitHub reports the gist gone (404) — recreate is safe.
    Gone,
}

#[async_trait]
impl AccountRegistryStore for GhAccountRegistryStore {
    async fn publish(
        &self,
        document: &AccountRegistryDocument,
    ) -> Result<(), AccountRegistryError> {
        // HERMETIC GATE (card d793c242), innermost layer: even if a
        // caller skips the daemon-startup and per-tick gates, the gh
        // transport itself refuses. A refusal is an ERROR, not a
        // silent no-op — the caller's diagnostics say exactly why
        // nothing was published.
        if let Some(block) = account_registry_block(&self.scope_home) {
            return Err(AccountRegistryError::Adapter(format!(
                "HERMETIC GATE: refusing account-registry publish — {block}"
            )));
        }
        document.validate()?;
        let body = serde_json::to_string_pretty(document).map_err(|error| {
            AccountRegistryError::Adapter(format!("serialize registry: {error}"))
        })?;

        // Find-or-update (card d793c242 item 2): same writer -> same
        // gist, update in place. Resolution order:
        //   1. local sentinel (fast path) — probe it still exists and
        //      edit using its CURRENT filename (legacy gists keep
        //      their legacy filename; no second file is ever added);
        //   2. sentinel missing/stale -> re-find OUR gist on the
        //      account by this writer's stable filename marker;
        //   3. genuinely absent -> create ONE gist with the writer
        //      filename and persist the sentinel.
        if let Some(id) = self.load_gist_id(&document.mesh_identity).await? {
            match self.probe_gist(&id).await? {
                GistProbe::Found(filename) => {
                    return match self.edit_gist(&id, &filename, &body).await {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            // Edit failed on a gist that just probed as
                            // present — clear the sentinel so the next
                            // publish runs find-or-create, and surface
                            // the failure loudly.
                            let _ = self.clear_gist_id(&document.mesh_identity).await;
                            Err(AccountRegistryError::Adapter(format!(
                                "{error}; sentinel cleared so next publish will re-find or recreate"
                            )))
                        }
                    };
                }
                GistProbe::Gone => {
                    // Deleted out-of-band; fall through to find-or-create.
                    self.clear_gist_id(&document.mesh_identity).await?;
                }
            }
        }

        let own_filename = writer_filename();
        if let Some(entry) = self
            .list_registry_gists()
            .await?
            .into_iter()
            .find(|entry| entry.filename == own_filename)
        {
            // This writer already has a gist on the account (sentinel
            // was lost — e.g. wiped events.sqlite). Adopt it instead
            // of creating a duplicate.
            self.edit_gist(&entry.id, &entry.filename, &body).await?;
            self.save_gist_id(&document.mesh_identity, &entry.id)
                .await?;
            return Ok(());
        }

        let id = self.create_gist(&body).await?;
        self.save_gist_id(&document.mesh_identity, &id).await?;
        Ok(())
    }

    async fn refresh(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError> {
        // HERMETIC GATE (card d793c242): a hermetic scope must not
        // READ the production rendezvous either — importing real
        // peers into a test daemon (or vice versa) is the same class
        // of cross-contamination as publishing.
        if let Some(block) = account_registry_block(&self.scope_home) {
            return Err(AccountRegistryError::Adapter(format!(
                "HERMETIC GATE: refusing account-registry refresh — {block}"
            )));
        }
        // One registry gist per writer/machine on the account: fetch
        // every readable document and MERGE (freshest beacon per
        // peer_id; temp-scoped beacons ignored with a counted, loud
        // line — see merge_registry_documents). Identity mismatches
        // (e.g. a stale gist from a previous account) are skipped by
        // the merge, not surfaced as errors.
        let entries = self.list_registry_gists().await?;
        let mut documents = Vec::with_capacity(entries.len());
        for entry in &entries {
            if let Some(document) = self.fetch_gist_document(entry).await? {
                documents.push(document);
            }
        }
        let outcome = merge_registry_documents(documents, mesh_identity);
        if outcome.ignored_temp_beacons > 0 {
            StderrJsonDiagnosticSink.emit(
                DiagnosticEvent::warn(
                    DiagnosticComponent::Daemon,
                    DiagnosticCode::AccountRegistryTempBeaconsIgnored,
                    "account-registry reader-merge ignored temp-scoped beacon(s): phantom \
                     hermetic-test peers are never enrolled or dialed (card d793c242)",
                )
                .with_field("ignored_count", outcome.ignored_temp_beacons),
            );
        }
        // Freshness pass: even the freshest beacon per peer can be
        // ancient (its publisher died; the gist was never cleaned).
        // Prune those before enrol so we never dial a route we already
        // know is dead — the stale-route orphan path.
        let Some(mut document) = outcome.document else {
            return Ok(None);
        };
        let now_ms = time::now_ms().map_err(|error| {
            AccountRegistryError::Adapter(format!("system clock before unix epoch: {error}"))
        })?;
        let pruned = prune_stale_peers(&mut document.peers, now_ms, DEFAULT_PEER_FRESHNESS_TTL_MS);
        if pruned > 0 {
            StderrJsonDiagnosticSink.emit(
                DiagnosticEvent::warn(
                    DiagnosticComponent::Daemon,
                    DiagnosticCode::AccountRegistryStaleBeaconsPruned,
                    "account-registry reader-merge pruned stale peer beacon(s): the freshest \
                     beacon for each was older than the freshness TTL — dead routes, never enrolled",
                )
                .with_field("pruned_count", pruned)
                .with_field("ttl_ms", DEFAULT_PEER_FRESHNESS_TTL_MS),
            );
        }
        Ok(Some(document))
    }
}

#[derive(Debug, Deserialize)]
struct GistListEntry {
    id: String,
    filename: String,
}

/// Pull the gist id out of `gh gist create`'s stdout. The CLI
/// generally returns `https://gist.github.com/<user>/<id>` on a
/// single line; tolerate trailing newlines and surrounding whitespace.
fn extract_gist_id(stdout: &str) -> Option<String> {
    let line = stdout.lines().last()?.trim();
    if let Some(idx) = line.rfind('/') {
        let id = &line[idx + 1..];
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

/// Suppress the console window when spawning the `gh` console app on Windows.
///
/// The account-registry daemon is spawned `DETACHED_PROCESS` (no console), and
/// on Windows a console subsystem app launched from a console-less parent gets
/// a brand-new console *window* allocated unless `CREATE_NO_WINDOW` is set. The
/// daemon shells out to `gh` on every registry tick (`gh auth status`, publish),
/// so without this each tick flashes a window — and if `gh` stalls (e.g. a slow
/// keyring lookup) the window lingers and piles up. gh's stdout/stderr are
/// always captured or nulled here, so suppressing the console hides nothing.
/// No-op off Windows.
#[inline]
fn gh_no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Budget for the `gh auth status` probe in [`gh_auth_ready`] (and the
/// `gh auth token` re-resolve in [`re_resolve_gh_token`]).
///
/// `gh auth status` is slower than it looks: gh's startup + an OS
/// keyring lookup runs ~900ms on Windows (measured on bigmama:
/// 881–950ms), well past the previous 750ms budget — so the gate timed
/// out on EVERY tick and reported "not authenticated" despite valid
/// auth, which is why same-account cross-machine discovery never
/// published a beacon and peers could enrol but never route (the
/// days-long keystone blocker, fixed in #1145). A genuinely
/// unauthenticated `gh auth status` still fails fast (well under a
/// second), so the wider budget only ever costs latency on the
/// slow-but-authed path it exists to allow. 5s = the measured ~1s
/// worst case with generous headroom for cold starts / loaded CI.
pub const GH_AUTH_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Probe whether the local `gh` is authenticated against GitHub.
/// Returns true if `gh auth status` exits zero within
/// [`GH_AUTH_READY_TIMEOUT`]. Used by callers (e.g.,
/// `Airc::join_default_context`) to skip publish/refresh cleanly when
/// the operator isn't logged in.
pub async fn gh_auth_ready(gh_bin: Option<&Path>) -> bool {
    gh_auth_ready_with_token(gh_bin, None).await
}

/// Like [`gh_auth_ready`], but the probe carries an explicit
/// `GH_TOKEN` (card 1f2cbffa): after a stale-token recovery the gate
/// must re-probe with the FRESH token — gh prefers an env token over
/// its keyring copy, so without the override the probe would keep
/// failing on the daemon's immutable spawn-time snapshot.
pub async fn gh_auth_ready_with_token(gh_bin: Option<&Path>, gh_token: Option<&str>) -> bool {
    let bin = gh_bin.unwrap_or_else(|| Path::new("gh"));
    let mut cmd = Command::new(bin);
    cmd.args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(token) = gh_token {
        cmd.env("GH_TOKEN", token);
    }
    gh_no_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    match timeout(GH_AUTH_READY_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.kill().await;
            false
        }
    }
}

/// ONE stale-token recovery attempt for a long-lived daemon (card
/// 1f2cbffa, mirroring `ReqwestGhClient`'s #1147 401-refresh): spawn
/// `gh auth token` with `GH_TOKEN`/`GITHUB_TOKEN` REMOVED from the
/// child env. gh prefers an env token over its keyring copy, so with
/// the daemon's stale injected env in place `gh auth token` would just
/// echo the stale token back — stripping the env is what makes the
/// keyring copy (which usually outlives a rotated env snapshot)
/// reachable at all.
///
/// The env re-read leg of #1147's chain (GH_TOKEN → GITHUB_TOKEN →
/// spawn) is deliberately absent here: gh child processes already
/// inherit the daemon's env token, so re-reading our own immutable
/// process env can never change the probe outcome — that frozen
/// snapshot IS the bug being recovered from.
///
/// A detached daemon may lack keychain access entirely (that is WHY
/// spawn-time injection exists); then this returns `None` and the
/// caller falls through to the existing loud skip diagnostic — one
/// extra recovery attempt, no new failure modes.
pub async fn re_resolve_gh_token(gh_bin: Option<&Path>) -> Option<String> {
    let bin = gh_bin.unwrap_or_else(|| Path::new("gh"));
    let mut cmd = Command::new(bin);
    cmd.args(["auth", "token"])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    gh_no_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Bounded like the auth probe; a hung gh is killed, never awaited
    // indefinitely (hazard d2ba719c — all waits bounded).
    let status = match timeout(GH_AUTH_READY_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => return None,
        Err(_) => {
            let _ = child.kill().await;
            return None;
        }
    };
    if !status.success() {
        return None;
    }
    // The child has exited; the pipe holds at most a token-sized line.
    let mut buf = String::new();
    use tokio::io::AsyncReadExt;
    stdout.read_to_string(&mut buf).await.ok()?;
    let token = buf.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Discard a Value into nothing — used in jq filter responses we
/// don't actually consume. Suppresses dead-code lints if the
/// underlying parser brings the type in.
#[allow(dead_code)]
fn _drop_value(_: Value) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_gist_id_handles_url_form() {
        assert_eq!(
            extract_gist_id("https://gist.github.com/joelteply/abc123def"),
            Some("abc123def".to_string())
        );
    }

    #[test]
    fn extract_gist_id_handles_trailing_newline() {
        assert_eq!(
            extract_gist_id("https://gist.github.com/joelteply/xyz\n"),
            Some("xyz".to_string())
        );
    }

    #[test]
    fn extract_gist_id_returns_none_for_empty_or_trailing_slash() {
        assert!(extract_gist_id("").is_none());
        assert!(extract_gist_id("https://gist.github.com/joelteply/").is_none());
    }

    #[test]
    fn sanitize_writer_component_is_stable_and_safe() {
        assert_eq!(
            sanitize_writer_component("Joels-MacBook-Pro.local"),
            "joels-macbook-pro-local"
        );
        assert_eq!(sanitize_writer_component("BIGMAMA"), "bigmama");
        assert_eq!(sanitize_writer_component("  weird host!! "), "weird-host");
        assert_eq!(sanitize_writer_component(""), "unknown");
        assert_eq!(sanitize_writer_component("---"), "unknown");
    }

    #[test]
    fn writer_filename_is_marked_and_stable() {
        let filename = writer_filename();
        assert!(filename.starts_with("airc-account-mesh-registry."));
        assert!(filename.ends_with(".json"));
        // Stable across calls in one process — the find-or-update key.
        assert_eq!(filename, writer_filename());
    }

    // what this catches: registry gc must delete ONLY the two provably-
    // junk categories (identity-less `unknown-user` publishers + legacy
    // pre-writer-key duplicates) and never a real machine's gist — the
    // exact policy that cleaned 80 phantom gists off a real account.
    // Mutation check: flipping any Keep<->Delete arm fails an assert.
    #[test]
    fn classify_registry_gc_deletes_only_provable_junk() {
        let gists = vec![
            (
                "real".to_string(),
                "airc-account-mesh-registry.bigmama-joelt.json".to_string(),
            ),
            (
                "ci".to_string(),
                "airc-account-mesh-registry.05c0fc93ce40-unknown-user.json".to_string(),
            ),
            (
                "legacy".to_string(),
                "airc-account-mesh-registry.json".to_string(),
            ),
            ("weird".to_string(), "something-else.json".to_string()),
        ];
        let verdicts = classify_registry_gc(&gists);
        let action_of = |id: &str| {
            verdicts
                .iter()
                .find(|v| v.id == id)
                .unwrap_or_else(|| panic!("missing verdict for {id}"))
                .action
                .clone()
        };
        assert_eq!(action_of("real"), GcAction::Keep, "real machine gist kept");
        assert_eq!(action_of("ci"), GcAction::Delete, "unknown-user is junk");
        assert_eq!(
            action_of("legacy"),
            GcAction::Delete,
            "legacy unnamed is junk"
        );
        assert_eq!(
            action_of("weird"),
            GcAction::Keep,
            "unrecognized filename must NOT be deleted"
        );
        let to_delete = verdicts
            .iter()
            .filter(|v| v.action == GcAction::Delete)
            .count();
        assert_eq!(to_delete, 2);
    }

    // Hermetic gate inputs (env-set arm is pinned by the CLI
    // subprocess tests in crates/airc-cli/tests/registry_hermetic.rs,
    // where the env can be set without racing parallel tests).
    #[test]
    fn account_registry_block_states() {
        assert_eq!(account_registry_block(Path::new("/Users/joel/.airc")), None);
        let dir = tempfile::tempdir().unwrap();
        match account_registry_block(dir.path()) {
            Some(AccountRegistryBlock::TempScopeHome { scope_home }) => {
                assert_eq!(scope_home, dir.path());
            }
            other => panic!("temp scope must be blocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn save_and_load_gist_id_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let event_store =
            airc_store::SqliteEventStore::open_path(&dir.path().join("events.sqlite"))
                .await
                .unwrap();
        let store = GhAccountRegistryStore::new(Arc::new(event_store), "/machine/a/.airc");
        let mesh = MeshIdentity::new("joelteply");
        assert!(store.load_gist_id(&mesh).await.unwrap().is_none());
        store.save_gist_id(&mesh, "abc123").await.unwrap();
        assert_eq!(
            store.load_gist_id(&mesh).await.unwrap().as_deref(),
            Some("abc123")
        );
        store.clear_gist_id(&mesh).await.unwrap();
        assert!(store.load_gist_id(&mesh).await.unwrap().is_none());
    }

    // Stub-gh integration tests (unix: the stub is a shell script).
    // The stub implements a filesystem-backed mini gist service so the
    // FULL publish/refresh round-trips run with ZERO real gh — hitting
    // the real gh from tests is the very bug this card fixes.
    #[cfg(unix)]
    mod gh_stub {
        use super::*;
        use crate::account_registry::AccountPeerBeacon;
        use crate::registry::PeerSpec;
        use crate::route::RouteEndpoint;
        use airc_core::PeerId;
        use airc_protocol::PeerKeypair;
        use std::os::unix::fs::PermissionsExt;

        const STUB_SCRIPT: &str = r#"#!/bin/sh
S="__STATE__"
printf '%s\n' "$*" >> "$S/calls.log"
printf '%s\n' "${GH_TOKEN-unset}" >> "$S/token.log"
case "$1" in
  auth)
    exit 0
    ;;
  gist)
    if [ "$2" = "create" ]; then
      fn=""
      prev=""
      for a in "$@"; do
        [ "$prev" = "--filename" ] && fn="$a"
        prev="$a"
      done
      n=$(cat "$S/count" 2>/dev/null || echo 0)
      n=$((n+1))
      echo "$n" > "$S/count"
      id="stubgist$n"
      cat > "$S/$id.content"
      printf '%s' "$fn" > "$S/$id.filename"
      echo "https://gist.github.com/stub/$id"
    elif [ "$2" = "edit" ]; then
      id="$3"
      if [ ! -f "$S/$id.filename" ]; then
        echo "HTTP 404: Not Found" >&2
        exit 1
      fi
      cat > "$S/$id.content"
    fi
    ;;
  api)
    # Two arg shapes: `api <path> [--jq expr]` (GET) and
    # `api --method DELETE <path>` (delete).
    if [ "$2" = "--method" ] || [ "$2" = "-X" ]; then
      method="$3"; path="$4"
    else
      method="GET"; path="$2"
    fi
    if [ "$method" = "DELETE" ]; then
      id="${path#/gists/}"
      rm -f "$S/$id.filename" "$S/$id.content"
      exit 0
    fi
    if [ "$path" = "/gists?per_page=100" ]; then
      for f in "$S"/*.filename; do
        [ -e "$f" ] || continue
        id=$(basename "$f" .filename)
        fn=$(cat "$f")
        printf '{"id":"%s","filename":"%s"}\n' "$id" "$fn"
      done
    else
      id="${path#/gists/}"
      if [ ! -f "$S/$id.filename" ]; then
        echo "HTTP 404: Not Found" >&2
        exit 1
      fi
      if [ "$4" = ".files | keys | .[0]" ]; then
        cat "$S/$id.filename"
        echo
      else
        cat "$S/$id.content"
      fi
    fi
    ;;
esac
exit 0
"#;

        struct StubGh {
            bin: std::path::PathBuf,
            state: std::path::PathBuf,
        }

        impl StubGh {
            fn install(dir: &Path) -> Self {
                let state = dir.join("gh-state");
                std::fs::create_dir_all(&state).unwrap();
                let bin = dir.join("gh");
                std::fs::write(
                    &bin,
                    STUB_SCRIPT.replace("__STATE__", &state.to_string_lossy()),
                )
                .unwrap();
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
                Self { bin, state }
            }

            fn calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.state.join("calls.log"))
                    .map(|log| log.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            }

            /// The `GH_TOKEN` each gh spawn carried (`unset` when absent),
            /// one line per call — the card-1f2cbffa recovered-token pin.
            fn tokens(&self) -> Vec<String> {
                std::fs::read_to_string(self.state.join("token.log"))
                    .map(|log| log.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            }

            fn create_count(&self) -> usize {
                self.calls()
                    .iter()
                    .filter(|line| line.starts_with("gist create"))
                    .count()
            }

            fn gist_content(&self, id: &str) -> String {
                std::fs::read_to_string(self.state.join(format!("{id}.content"))).unwrap()
            }

            fn seed_gist(&self, id: &str, filename: &str, document: &AccountRegistryDocument) {
                std::fs::write(self.state.join(format!("{id}.filename")), filename).unwrap();
                std::fs::write(
                    self.state.join(format!("{id}.content")),
                    serde_json::to_string(document).unwrap(),
                )
                .unwrap();
            }
        }

        async fn store_at(
            stub: &StubGh,
            db_dir: &Path,
            scope_home: &str,
        ) -> GhAccountRegistryStore {
            std::fs::create_dir_all(db_dir).unwrap();
            let event_store =
                airc_store::SqliteEventStore::open_path(&db_dir.join("events.sqlite"))
                    .await
                    .unwrap();
            GhAccountRegistryStore::new(Arc::new(event_store), scope_home).with_bin(&stub.bin)
        }

        fn mesh() -> MeshIdentity {
            MeshIdentity::new("joelteply")
        }

        /// Real wall-clock now, in ms. `refresh` prunes peers whose
        /// freshest beacon is older than `DEFAULT_PEER_FRESHNESS_TTL_MS`
        /// against this clock, so a beacon that must SURVIVE a refresh
        /// has to carry a near-now heartbeat (a live peer). Tests anchor
        /// surviving beacons at `now() - small_offset`.
        fn now() -> u64 {
            crate::time::now_ms().expect("system clock available in test")
        }

        fn beacon(scope_home: &str, heartbeat_ms: u64, relay: &str) -> AccountPeerBeacon {
            let peer_id = PeerId::new();
            beacon_for(peer_id, scope_home, heartbeat_ms, relay)
        }

        fn beacon_for(
            peer_id: PeerId,
            scope_home: &str,
            heartbeat_ms: u64,
            relay: &str,
        ) -> AccountPeerBeacon {
            let keypair = PeerKeypair::generate();
            AccountPeerBeacon {
                presence: crate::coordinator::beacon_now(
                    peer_id,
                    scope_home.into(),
                    Vec::new(),
                    123,
                    heartbeat_ms,
                ),
                peer_spec: PeerSpec {
                    peer_id,
                    pubkey: keypair.public_bytes(),
                },
                endpoints: vec![RouteEndpoint::Relay {
                    url: relay.to_string(),
                }],
            }
        }

        fn document(
            generated_at_ms: u64,
            peers: Vec<AccountPeerBeacon>,
        ) -> AccountRegistryDocument {
            AccountRegistryDocument::new(mesh(), generated_at_ms, Vec::new(), peers)
        }

        // what this catches: `gc` dry-run reports the junk plan without
        // deleting; `gc --apply` removes ONLY the junk (unknown-user +
        // legacy) and leaves the real machine gist — the first-class
        // version of the manual cleanup that cleared 80 phantom gists off
        // a real account. Mutation check: dropping the apply delete-loop
        // leaves all 3 gists and `deleted` == 0.
        #[tokio::test]
        async fn gc_deletes_only_junk_on_apply_not_dry_run() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let store = store_at(&stub, &dir.path().join("db"), "/machine/prod/.airc").await;

            stub.seed_gist(
                "real",
                "airc-account-mesh-registry.bigmama-joelt.json",
                &document(1, vec![]),
            );
            stub.seed_gist(
                "ci",
                "airc-account-mesh-registry.05c0fc93ce40-unknown-user.json",
                &document(1, vec![]),
            );
            stub.seed_gist(
                "legacy",
                "airc-account-mesh-registry.json",
                &document(1, vec![]),
            );

            // Dry run: plan only, nothing deleted, all 3 still present.
            let dry = store.gc(false).await.unwrap();
            assert_eq!(dry.to_delete, 2, "two junk gists planned for deletion");
            assert_eq!(dry.kept, 1, "the real machine gist is kept");
            assert_eq!(dry.deleted, 0, "dry run deletes nothing");
            assert!(!dry.applied);
            assert_eq!(store.list_registry_gists().await.unwrap().len(), 3);

            // Apply: junk gone, real machine gist survives.
            let applied = store.gc(true).await.unwrap();
            assert_eq!(applied.deleted, 2);
            assert_eq!(applied.kept, 1);
            assert!(applied.applied);
            let remaining = store.list_registry_gists().await.unwrap();
            assert_eq!(remaining.len(), 1, "only the real machine gist remains");
            assert_eq!(
                remaining[0].filename,
                "airc-account-mesh-registry.bigmama-joelt.json"
            );
        }

        // Production-shaped home + env unset → publish goes through
        // (stubbed), AND two ticks land on the SAME gist: one create,
        // then edit in place. Mutation check (find-or-update broken to
        // always-create): the second tick creates stubgist2 and the
        // create_count assert fails.
        #[tokio::test]
        async fn publish_creates_once_then_edits_same_gist() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let store = store_at(&stub, &dir.path().join("db"), "/machine/prod/.airc").await;

            store.publish(&document(1_000, Vec::new())).await.unwrap();
            let tick_two = document(2_000, Vec::new());
            store.publish(&tick_two).await.unwrap();

            assert_eq!(
                stub.create_count(),
                1,
                "same writer -> same gist, no duplicates"
            );
            assert!(
                stub.calls()
                    .iter()
                    .any(|line| line.starts_with("gist edit stubgist1")),
                "second tick must edit the first gist in place: {:?}",
                stub.calls()
            );
            // The gist carries the latest document, under this
            // writer's stable filename marker.
            let stored: AccountRegistryDocument =
                serde_json::from_str(&stub.gist_content("stubgist1")).unwrap();
            assert_eq!(stored, tick_two);
            assert_eq!(
                std::fs::read_to_string(stub.state.join("stubgist1.filename")).unwrap(),
                writer_filename()
            );
        }

        // PROLIFERATION PIN (card d793c242 item 2): when the local
        // sentinel is GONE (fresh events.sqlite — the rotated-identity
        // case that minted 5+ duplicate gists under joelteply), the
        // publisher re-finds its own gist by writer filename and
        // updates it instead of creating another. Mutation check:
        // removing the find step creates stubgist2 here.
        #[tokio::test]
        async fn sentinel_loss_refinds_own_gist_instead_of_creating() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());

            let first = store_at(&stub, &dir.path().join("db-a"), "/machine/prod/.airc").await;
            first.publish(&document(1_000, Vec::new())).await.unwrap();
            assert_eq!(stub.create_count(), 1);

            // Same machine, same stub state — but a FRESH sqlite store,
            // so the gist-id sentinel is gone.
            let reborn = store_at(&stub, &dir.path().join("db-b"), "/machine/prod/.airc").await;
            let tick = document(2_000, Vec::new());
            reborn.publish(&tick).await.unwrap();

            assert_eq!(
                stub.create_count(),
                1,
                "sentinel loss must NOT create a duplicate gist: {:?}",
                stub.calls()
            );
            assert!(
                stub.calls()
                    .iter()
                    .any(|line| line.starts_with("gist edit stubgist1")),
                "the writer's existing gist must be adopted and edited: {:?}",
                stub.calls()
            );
            let stored: AccountRegistryDocument =
                serde_json::from_str(&stub.gist_content("stubgist1")).unwrap();
            assert_eq!(stored, tick);
        }

        // HERMETIC GATE, innermost layer (card d793c242 item 1): a
        // temp-rooted scope home refuses BOTH publish and refresh with
        // a loud, reasoned error — and gh is NEVER spawned. Mutation
        // check: removing the gate from publish/refresh makes the stub
        // log non-empty (and publish succeed), failing every assert.
        #[tokio::test]
        async fn temp_scope_home_never_reaches_gh() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let scope_home = dir.path().join("scope/.airc");
            let store =
                store_at(&stub, &dir.path().join("db"), &scope_home.to_string_lossy()).await;

            let publish_error = store
                .publish(&document(1_000, Vec::new()))
                .await
                .expect_err("temp-rooted scope must refuse to publish");
            let message = publish_error.to_string();
            assert!(
                message.contains("HERMETIC GATE"),
                "loud refusal, got: {message}"
            );
            assert!(
                message.contains("temp-rooted"),
                "reason named, got: {message}"
            );

            let refresh_error = store
                .refresh(&mesh())
                .await
                .expect_err("temp-rooted scope must refuse to refresh");
            assert!(refresh_error.to_string().contains("HERMETIC GATE"));

            assert!(
                stub.calls().is_empty(),
                "gh must never be spawned for a hermetic scope: {:?}",
                stub.calls()
            );
        }

        // Reader-side (card d793c242 item 3) through the REAL gh-store
        // refresh: documents from multiple writer gists merge with
        // freshest-beacon-per-peer, and temp-scoped phantom beacons in
        // old polluted documents are dropped.
        #[tokio::test]
        async fn refresh_merges_writer_gists_and_drops_temp_beacons() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let store = store_at(&stub, &dir.path().join("db"), "/machine/prod/.airc").await;

            let shared = PeerId::new();
            // Both heartbeats are near-now (live peer): refresh prunes
            // beacons older than the freshness TTL, so the peer must be
            // fresh to survive — `fresh` is more recent than `stale` so
            // the freshest-wins dedup is still exercised.
            let stale = beacon_for(
                shared,
                "/machine/a/.airc",
                now() - 5_000,
                "https://stale.example.test",
            );
            let fresh = beacon_for(
                shared,
                "/machine/a/.airc",
                now() - 1_000,
                "https://fresh.example.test",
            );
            // Phantom is temp-scoped → dropped by the merge temp filter
            // before prune, so its heartbeat is irrelevant.
            let phantom = beacon(
                r"C:\Users\green\AppData\Local\Temp\tmp.YYavgmVUxz\.airc",
                9_000,
                "https://phantom.example.test",
            );

            // Legacy-filename gist (old writer) with a stale beacon +
            // a temp-scoped phantom; writer-keyed gist with the fresh
            // beacon.
            stub.seed_gist(
                "g1",
                "airc-account-mesh-registry.json",
                &document(2_000, vec![stale, phantom]),
            );
            stub.seed_gist(
                "g2",
                &writer_filename(),
                &document(6_000, vec![fresh.clone()]),
            );

            let merged = store
                .refresh(&mesh())
                .await
                .unwrap()
                .expect("documents must merge");

            assert_eq!(
                merged.peers.len(),
                1,
                "phantom dropped, shared peer deduped"
            );
            assert_eq!(merged.peers[0].peer_id(), shared);
            assert_eq!(
                merged.peers[0].endpoints, fresh.endpoints,
                "freshest beacon wins"
            );
        }

        // Card 4b6a0ffa item 1 (post-#1146 audit): refresh is a UNION
        // across per-machine writer gists, exactly as the module doc
        // promises — a peer present only in an OLDER writer's document
        // (with its endpoints) survives a refresh even when a newer
        // writer's document lacks it. Mutation check: reverting
        // `refresh` to pick the single newest document drops peer A
        // entirely and both asserts fail.
        #[tokio::test]
        async fn refresh_unions_peers_across_writer_gists_keeping_endpoints() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let store = store_at(&stub, &dir.path().join("db"), "/machine/prod/.airc").await;

            // Both live (near-now heartbeats) so neither is pruned by
            // the refresh freshness pass — the test is about the UNION
            // across writer gists, not freshness.
            let peer_a = beacon_for(
                PeerId::new(),
                "/machine/a/.airc",
                now() - 5_000,
                "https://machine-a.example.test",
            );
            let peer_b = beacon_for(
                PeerId::new(),
                "/machine/b/.airc",
                now() - 1_000,
                "https://machine-b.example.test",
            );

            // Older writer gist carries peer A (with endpoints); the
            // NEWER writer gist does not mention A at all.
            stub.seed_gist(
                "older-writer",
                "airc-account-mesh-registry.older-machine.json",
                &document(2_000, vec![peer_a.clone()]),
            );
            stub.seed_gist(
                "newer-writer",
                "airc-account-mesh-registry.newer-machine.json",
                &document(6_000, vec![peer_b.clone()]),
            );

            let merged = store
                .refresh(&mesh())
                .await
                .unwrap()
                .expect("documents must merge");

            assert_eq!(
                merged.peers.len(),
                2,
                "refresh must union per-machine documents, not pick the newest"
            );
            let merged_a = merged
                .peers
                .iter()
                .find(|peer| peer.peer_id() == peer_a.peer_id())
                .expect("peer A from the older writer must survive the merge");
            assert_eq!(
                merged_a.endpoints, peer_a.endpoints,
                "peer A's dialable endpoints must survive the merge"
            );
        }

        // what this catches: refresh PRUNES a peer whose freshest
        // surviving gist beacon is older than the freshness TTL (a dead
        // publisher whose gist was never cleaned) while keeping a live
        // peer — the stale-route orphan path. Mutation check: removing
        // the prune wiring in `refresh` keeps the dead peer and the
        // len/identity asserts fail.
        #[tokio::test]
        async fn refresh_prunes_stale_peer_keeping_live_one() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let store = store_at(&stub, &dir.path().join("db"), "/machine/prod/.airc").await;

            let live = beacon_for(
                PeerId::new(),
                "/machine/live/.airc",
                now() - 30_000, // 30s old — well inside the TTL
                "https://live.example.test",
            );
            let dead = beacon_for(
                PeerId::new(),
                "/machine/dead/.airc",
                // Older than DEFAULT_PEER_FRESHNESS_TTL_MS (10min) — a
                // publisher that has been gone for an hour.
                now() - (DEFAULT_PEER_FRESHNESS_TTL_MS + 3_600_000),
                "https://dead.example.test",
            );
            stub.seed_gist(
                "live-writer",
                "airc-account-mesh-registry.live.json",
                &document(2_000, vec![live.clone()]),
            );
            stub.seed_gist(
                "dead-writer",
                "airc-account-mesh-registry.dead.json",
                &document(2_000, vec![dead.clone()]),
            );

            let merged = store
                .refresh(&mesh())
                .await
                .unwrap()
                .expect("documents must merge");

            assert_eq!(merged.peers.len(), 1, "the dead-route peer is pruned");
            assert_eq!(
                merged.peers[0].peer_id(),
                live.peer_id(),
                "only the live peer is enrolled"
            );
        }

        // Card 1f2cbffa item 4: once the gate's stale-token recovery
        // lands a fresh token in the shared slot, EVERY gh spawn the
        // store makes must carry it as GH_TOKEN — otherwise recovery
        // passes the gate and then the publish fails with the same
        // stale spawn-time env snapshot. Mutation check: removing the
        // `cmd.env("GH_TOKEN", …)` application in `gh_run` makes every
        // token.log line read `unset` (or the host's ambient token),
        // failing the assert.
        #[tokio::test]
        async fn store_gh_spawns_carry_the_recovered_token() {
            let dir = tempfile::tempdir().unwrap();
            let stub = StubGh::install(dir.path());
            let slot = GhTokenOverride::new();
            slot.set("tok-fresh-after-rotation".to_string());
            let store = store_at(&stub, &dir.path().join("db"), "/machine/prod/.airc")
                .await
                .with_token_override(slot);

            store.publish(&document(1_000, Vec::new())).await.unwrap();

            let tokens = stub.tokens();
            assert!(
                !tokens.is_empty(),
                "publish must have spawned gh at least once"
            );
            assert!(
                tokens.iter().all(|t| t == "tok-fresh-after-rotation"),
                "every gh spawn must carry the recovered token, got: {tokens:?}"
            );
        }
    }

    // gh-auth gate stub tests (card 1f2cbffa item 1, upstreamed from
    // the #1145 post-merge audit's throwaway evidence). cfg(unix): the
    // stub is a shell script; hosted ubuntu + macos legs run these
    // (precedent: #1150).
    #[cfg(unix)]
    mod gh_auth_gate_stub {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;

        fn install_stub(dir: &Path, name: &str, body: &str) -> PathBuf {
            let bin = dir.join(name);
            std::fs::write(&bin, body).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        }

        // THE #1145 PIN: a gh that takes ~1s to answer but IS
        // authenticated must pass the gate. This is the measured
        // 881–950ms Windows `gh auth status` class that the previous
        // 750ms budget timed out on EVERY tick, breaking same-account
        // cross-machine discovery for days. Mutation check (verified
        // in the audit): shrink GH_AUTH_READY_TIMEOUT to 750ms and
        // this fails in ~0.76s.
        #[tokio::test]
        async fn gate_allows_slow_but_authed_gh_900ms_class() {
            let dir = tempfile::tempdir().unwrap();
            let bin = install_stub(dir.path(), "gh", "#!/bin/sh\nsleep 1\nexit 0\n");
            assert!(
                gh_auth_ready(Some(&bin)).await,
                "slow-but-authed gh (the measured Windows ~900ms class) must pass the gate"
            );
        }

        // A genuinely-unauthenticated gh exits non-zero fast; the gate
        // must report not-ready WITHOUT consuming the timeout budget
        // (the budget exists only for the slow-but-authed path).
        #[tokio::test]
        async fn gate_fails_fast_for_unauthed_gh() {
            let dir = tempfile::tempdir().unwrap();
            let bin = install_stub(dir.path(), "gh", "#!/bin/sh\nexit 1\n");
            let start = Instant::now();
            assert!(
                !gh_auth_ready(Some(&bin)).await,
                "unauthed gh must fail the gate"
            );
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "unauthed gh must fail FAST, not burn the {GH_AUTH_READY_TIMEOUT:?} budget; took {:?}",
                start.elapsed()
            );
        }

        // A hung gh (stalled keyring, dead network) is KILLED at the
        // deadline — the gate answers not-ready in ~GH_AUTH_READY_TIMEOUT,
        // never the stub's 30s. `exec` makes the kill hit the sleep
        // itself (no orphaned child outliving the test).
        #[tokio::test]
        async fn gate_kills_hung_gh_at_the_deadline() {
            let dir = tempfile::tempdir().unwrap();
            let bin = install_stub(dir.path(), "gh", "#!/bin/sh\nexec sleep 30\n");
            let start = Instant::now();
            assert!(
                !gh_auth_ready(Some(&bin)).await,
                "hung gh must fail the gate"
            );
            let elapsed = start.elapsed();
            assert!(
                elapsed >= GH_AUTH_READY_TIMEOUT - Duration::from_millis(100),
                "the gate must grant the full budget before giving up; took {elapsed:?}"
            );
            assert!(
                elapsed < GH_AUTH_READY_TIMEOUT + Duration::from_secs(10),
                "hung gh must be killed at the deadline, not awaited to completion; took {elapsed:?}"
            );
        }

        // Card 1f2cbffa item 4: the recovery spawn strips the stale
        // env so gh reads its keyring copy instead of echoing the
        // injected token back. The stub mirrors real gh precedence
        // (env token wins when present): if the test environment (or a
        // mutation removing `env_remove`) leaks a GH_TOKEN/GITHUB_TOKEN
        // into the spawn, the stub echoes THAT instead of the keyring
        // copy and the assert bites.
        #[tokio::test]
        async fn re_resolve_strips_stale_env_and_reads_keyring_copy() {
            let dir = tempfile::tempdir().unwrap();
            let bin = install_stub(
                dir.path(),
                "gh",
                r#"#!/bin/sh
if [ "$1 $2" = "auth token" ]; then
  if [ -n "${GH_TOKEN-}" ]; then printf '%s\n' "$GH_TOKEN"; exit 0; fi
  if [ -n "${GITHUB_TOKEN-}" ]; then printf '%s\n' "$GITHUB_TOKEN"; exit 0; fi
  printf 'fresh-keyring-token\n'
  exit 0
fi
exit 1
"#,
            );
            assert_eq!(
                re_resolve_gh_token(Some(&bin)).await.as_deref(),
                Some("fresh-keyring-token"),
                "re-resolve must reach the keyring copy, never echo an env token"
            );
        }

        // The keychain-less daemon case (the reason injection exists):
        // `gh auth token` fails → the recovery attempt yields None and
        // the caller falls through to the existing loud skip. No new
        // failure modes.
        #[tokio::test]
        async fn re_resolve_returns_none_when_keyring_unreachable() {
            let dir = tempfile::tempdir().unwrap();
            let bin = install_stub(
                dir.path(),
                "gh",
                "#!/bin/sh\necho 'keyring unavailable' >&2\nexit 1\n",
            );
            assert_eq!(re_resolve_gh_token(Some(&bin)).await, None);
        }
    }
}
