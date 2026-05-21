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
//! - **Filename:** `airc-account-mesh-registry.json` (single file per
//!   gist; gist private)
//! - **Description marker:** `airc-account-mesh-registry` (used for
//!   discovery filtering on `gh gist list`)
//! - **One gist per machine.** The local `airc-registry-gist-id`
//!   sentinel under `<wire_root>/` records this machine's gist id so
//!   subsequent publishes update the same gist instead of creating
//!   duplicates. If the sentinel is missing on first publish, a new
//!   gist is created and the id is persisted.
//! - **Refresh merges all matching gists.** A scope joining on a
//!   third+ machine sees both other machines' beacons without any
//!   server-side coordination.
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

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::account_registry::{
    AccountRegistryDocument, AccountRegistryError, AccountRegistryStore,
};
use crate::subscriptions::MeshIdentity;

/// Filename used for the registry blob inside each per-machine gist.
const REGISTRY_FILENAME: &str = "airc-account-mesh-registry.json";
/// Description marker used by `gh gist list` for discovery. Stable
/// across versions — bumping this constant would orphan existing
/// registries.
const REGISTRY_DESCRIPTION: &str = "airc-account-mesh-registry";
/// Local sentinel that records this machine's gist id so subsequent
/// publishes update the same gist instead of creating duplicates.
const GIST_ID_FILENAME: &str = "account-registry-gist-id";

/// gh-gist-backed account-registry store.
#[derive(Debug, Clone)]
pub struct GhAccountRegistryStore {
    gh_bin: PathBuf,
    sentinel_root: PathBuf,
}

impl GhAccountRegistryStore {
    /// Construct a new store. `sentinel_root` is the directory where
    /// the local-gist-id sentinel will be persisted — typically the
    /// machine-account home `~/.airc/`.
    pub fn new(sentinel_root: impl Into<PathBuf>) -> Self {
        Self {
            gh_bin: PathBuf::from(
                std::env::var_os("AIRC_GH_BIN")
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "gh".into()),
            ),
            sentinel_root: sentinel_root.into(),
        }
    }

    /// Override the `gh` binary path. Used in tests.
    pub fn with_bin(mut self, gh_bin: impl Into<PathBuf>) -> Self {
        self.gh_bin = gh_bin.into();
        self
    }

    fn sentinel_path(&self) -> PathBuf {
        self.sentinel_root.join(GIST_ID_FILENAME)
    }

    fn load_gist_id(&self) -> Option<String> {
        std::fs::read_to_string(self.sentinel_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn save_gist_id(&self, id: &str) -> Result<(), AccountRegistryError> {
        std::fs::create_dir_all(&self.sentinel_root).map_err(|error| {
            AccountRegistryError::Adapter(format!("create sentinel dir: {error}"))
        })?;
        let path = self.sentinel_path();
        std::fs::write(&path, id).map_err(|error| {
            AccountRegistryError::Adapter(format!("write sentinel {}: {error}", path.display()))
        })
    }

    async fn gh_run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<(bool, String, String), AccountRegistryError> {
        let mut cmd = Command::new(&self.gh_bin);
        cmd.args(args);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
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
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}

#[async_trait]
impl AccountRegistryStore for GhAccountRegistryStore {
    async fn publish(
        &self,
        document: &AccountRegistryDocument,
    ) -> Result<(), AccountRegistryError> {
        document.validate()?;
        let body = serde_json::to_string_pretty(document).map_err(|error| {
            AccountRegistryError::Adapter(format!("serialize registry: {error}"))
        })?;

        match self.load_gist_id() {
            Some(id) => {
                // Update existing gist. `gh gist edit <id> --filename
                // <name> -` reads new content from stdin.
                let (ok, _stdout, stderr) = self
                    .gh_run(
                        &["gist", "edit", &id, "--filename", REGISTRY_FILENAME, "-"],
                        Some(&body),
                    )
                    .await?;
                if !ok {
                    // Probably the recorded gist was deleted out-of-
                    // band. Drop the sentinel and retry as create.
                    let _ = std::fs::remove_file(self.sentinel_path());
                    return Err(AccountRegistryError::Adapter(format!(
                        "gh gist edit {id} failed; sentinel cleared so next publish will recreate. stderr: {stderr}"
                    )));
                }
                Ok(())
            }
            None => {
                // Create a new private gist. `gh gist create -` reads
                // content from stdin; the new gist's URL is on stdout.
                let (ok, stdout, stderr) = self
                    .gh_run(
                        &[
                            "gist",
                            "create",
                            "--filename",
                            REGISTRY_FILENAME,
                            "--desc",
                            REGISTRY_DESCRIPTION,
                            "-",
                        ],
                        Some(&body),
                    )
                    .await?;
                if !ok {
                    return Err(AccountRegistryError::Adapter(format!(
                        "gh gist create failed: {stderr}"
                    )));
                }
                let id = extract_gist_id(stdout.trim()).ok_or_else(|| {
                    AccountRegistryError::Adapter(format!(
                        "could not parse gist id from gh output: {stdout}"
                    ))
                })?;
                self.save_gist_id(&id)?;
                Ok(())
            }
        }
    }

    async fn refresh(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError> {
        // List the authenticated user's gists with our marker
        // description. Returns at most one gist per machine on the
        // account — refresh merges them all by picking the newest
        // matching the requested mesh_identity. Operator-supplied
        // identity mismatches (e.g., a stale gist from a previous
        // account) are ignored, not surfaced as errors.
        // Discovery scans only the user's MOST RECENT 100 gists. The
        // account-mesh-registry beacon is updated on every `airc
        // join`, so it stays at the top of the user's gist list
        // sorted by recency — there's no reason to iterate the
        // entire account's gist history (which can take minutes for
        // high-gist-count operators). Dropped `--paginate`
        // deliberately. If an operator legitimately needs deeper
        // discovery, that's a separate explicit verb later.
        let (ok, stdout, stderr) = self
            .gh_run(
                &[
                    "api",
                    "/gists?per_page=100",
                    "--jq",
                    // Filter to gists whose description matches and which
                    // contain our registry filename. Returns one line of
                    // JSON per match: {"id":"...","filename":"..."}.
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

        let mut best: Option<AccountRegistryDocument> = None;
        for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
            let entry: GistListEntry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Pull the gist content and parse as a registry document.
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
                continue;
            }
            let doc: AccountRegistryDocument = match serde_json::from_str(content.trim()) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if doc.mesh_identity != *mesh_identity {
                continue;
            }
            if doc.validate().is_err() {
                continue;
            }
            match &best {
                Some(prev) if prev.generated_at_ms >= doc.generated_at_ms => {}
                _ => best = Some(doc),
            }
        }
        Ok(best)
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

/// Probe whether the local `gh` is authenticated against GitHub.
/// Returns Ok(()) if `gh auth status` exits zero. Used by callers
/// (e.g., `Airc::join_default_context`) to skip publish/refresh
/// cleanly when the operator isn't logged in.
pub async fn gh_auth_ready(gh_bin: Option<&Path>) -> bool {
    let bin = gh_bin.unwrap_or_else(|| Path::new("gh"));
    let mut child = match Command::new(bin)
        .args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    match timeout(Duration::from_millis(750), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.kill().await;
            false
        }
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
    fn save_and_load_gist_id_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = GhAccountRegistryStore::new(dir.path());
        assert!(store.load_gist_id().is_none());
        store.save_gist_id("abc123").unwrap();
        assert_eq!(store.load_gist_id().as_deref(), Some("abc123"));
    }
}
