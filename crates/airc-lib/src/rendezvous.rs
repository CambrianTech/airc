//! Rendezvous provider SELECTION — the one place that picks which door
//! the account-mesh converges through, so a fresh clone auto-picks a
//! working rendezvous with zero manual network ops.
//!
//! Rendezvous is the untrusted meeting point that INITIATES the exchange;
//! it does NOT supply security. Security lives in the E2E data plane
//! (X25519 + ChaCha20-Poly1305, pinned peer keys, trust tiers) regardless
//! of which door a node came through — the Tailscale model. That is why a
//! shared folder is a legitimate peer of a GitHub gist here: swapping the
//! meeting point changes nothing about the trust boundary.
//!
//! [`AccountRegistryStore`] is already swappable (`SqliteAccountRegistryStore`
//! local cache, `GhAccountRegistryStore` gist, `FsAccountRegistryStore`
//! shared folder). This module wires the SELECTION on top: given a
//! [`RendezvousChoice`], [`resolve_account_registry_store`] hands back the
//! store paired with the [`RegistryRefreshGate`] that matches it — the
//! coupling that keeps a gist store from being handed a no-auth gate (or a
//! folder store a gh-auth gate). The daemon refresh loop then runs against
//! the boxed store without ever learning which door was chosen.

use std::path::PathBuf;
use std::sync::Arc;

use airc_store::SqliteEventStore;

use crate::account_registry::AccountRegistryStore;
use crate::account_registry_fs::FsAccountRegistryStore;
use crate::gh::account_registry::{writer_filename, GhAccountRegistryStore, GhTokenOverride};
use crate::registry_refresh::RegistryRefreshGate;

/// Env var naming a shared-folder rendezvous directory (iCloud Drive /
/// Syncthing / NFS mount). Its PRESENCE selects the folder door; absent →
/// the default gist door. Agents already carry `gh` for kanban/PRs, so
/// gist is the zero-config default; the folder is the on-prem /
/// behind-firewall (hospital) escape hatch that needs no GitHub at all.
pub const RENDEZVOUS_DIR_ENV: &str = "AIRC_RENDEZVOUS_DIR";

/// Which rendezvous transport this machine converges through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendezvousChoice {
    /// GitHub gist — the default (no extra config; `gh` is already present
    /// for kanban/PRs).
    Gist,
    /// Shared folder — no gh, no token, no network. The on-prem door.
    Folder { dir: PathBuf },
}

/// A rendezvous env var was SET but unusable. Fail loud rather than
/// silently falling back to gist — an operator who set the var meant to
/// name a folder, and swallowing that would hide the misconfiguration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendezvousConfigError {
    /// `AIRC_RENDEZVOUS_DIR` was set to an empty/whitespace value.
    EmptyDir,
    /// `AIRC_RENDEZVOUS_DIR` held non-UTF-8 bytes.
    NonUnicodeDir,
}

impl std::fmt::Display for RendezvousConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyDir => write!(
                f,
                "{RENDEZVOUS_DIR_ENV} was set but empty — name a shared-folder path or unset it to use the gist rendezvous"
            ),
            Self::NonUnicodeDir => write!(
                f,
                "{RENDEZVOUS_DIR_ENV} held non-UTF-8 bytes — set it to a valid filesystem path"
            ),
        }
    }
}

impl std::error::Error for RendezvousConfigError {}

/// Pure parse of the `AIRC_RENDEZVOUS_DIR` env read into a choice, split
/// out from [`RendezvousChoice::from_env`] so it is testable without
/// process-global env mutation (which would break test parallelism).
pub fn parse_rendezvous_dir(
    var: Result<String, std::env::VarError>,
) -> Result<RendezvousChoice, RendezvousConfigError> {
    match var {
        Ok(dir) if dir.trim().is_empty() => Err(RendezvousConfigError::EmptyDir),
        Ok(dir) => Ok(RendezvousChoice::Folder {
            dir: PathBuf::from(dir),
        }),
        Err(std::env::VarError::NotPresent) => Ok(RendezvousChoice::Gist),
        Err(std::env::VarError::NotUnicode(_)) => Err(RendezvousConfigError::NonUnicodeDir),
    }
}

impl RendezvousChoice {
    /// Resolve from the environment: `AIRC_RENDEZVOUS_DIR=<path>` selects a
    /// shared-folder rendezvous; unset → the default gist. Set-but-empty /
    /// non-UTF-8 fail loud.
    pub fn from_env() -> Result<Self, RendezvousConfigError> {
        parse_rendezvous_dir(std::env::var(RENDEZVOUS_DIR_ENV))
    }
}

/// The gh-specific inputs the gist door needs. Supplied unconditionally by
/// the caller; ignored by the folder door (which needs no auth, no token,
/// no local event store).
pub struct GistRendezvous {
    /// Local cache store the gh adapter records "what we last sent/received"
    /// into.
    pub event_store: Arc<SqliteEventStore>,
    /// Scope home the gh-auth gate probes and the store publishes under.
    pub scope_home: PathBuf,
    /// Explicit `gh` binary path (a detached daemon can't rely on PATH).
    pub gh_bin: Option<PathBuf>,
    /// Stale-token recovery slot shared by store + gate (card 1f2cbffa).
    pub token_override: GhTokenOverride,
}

/// Pick the store for `choice` and pair it with the gate that MATCHES it.
///
/// Returning a `Box<dyn AccountRegistryStore>` (which itself impls the
/// trait — see the delegating impl in `account_registry`) lets the two
/// doors flow through `run_loop`'s generic `S: AccountRegistryStore` bound
/// unchanged. The gate is the load-bearing pairing: gist → `GhAuth`
/// (hermetic + `gh auth status` probe), folder → `Always` (no external
/// auth to gate on).
pub fn resolve_account_registry_store(
    choice: RendezvousChoice,
    gist: GistRendezvous,
) -> (Box<dyn AccountRegistryStore>, RegistryRefreshGate) {
    match choice {
        RendezvousChoice::Gist => {
            let store = match &gist.gh_bin {
                Some(bin) => GhAccountRegistryStore::new(gist.event_store, &gist.scope_home)
                    .with_bin(bin.clone()),
                None => GhAccountRegistryStore::new(gist.event_store, &gist.scope_home),
            }
            .with_token_override(gist.token_override.clone());
            let gate = RegistryRefreshGate::GhAuth {
                gh_bin: gist.gh_bin,
                scope_home: gist.scope_home,
                token_override: Some(gist.token_override),
            };
            (Box::new(store), gate)
        }
        RendezvousChoice::Folder { dir } => {
            // Same per-writer filename discipline the gist door uses, so
            // writer identity is stable if a machine ever switches doors.
            let store = FsAccountRegistryStore::new(dir, writer_filename());
            (Box::new(store), RegistryRefreshGate::Always)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_registry::AccountRegistryStore;
    use crate::subscriptions::MeshIdentity;
    use tempfile::TempDir;

    // what this catches: the env→choice contract — unset is the gist
    // default, a set path selects the folder door, and a set-but-empty
    // value fails loud instead of silently defaulting to gist (which would
    // hide an operator's misconfiguration).
    #[test]
    fn unset_env_selects_the_gist_default() {
        let choice =
            parse_rendezvous_dir(Err(std::env::VarError::NotPresent)).expect("unset is valid");
        assert_eq!(choice, RendezvousChoice::Gist);
    }

    #[test]
    fn a_set_path_selects_the_folder_door() {
        let choice =
            parse_rendezvous_dir(Ok("/mnt/mesh-share".to_string())).expect("a path is valid");
        assert_eq!(
            choice,
            RendezvousChoice::Folder {
                dir: PathBuf::from("/mnt/mesh-share")
            }
        );
    }

    #[test]
    fn an_empty_value_fails_loud_not_silent_gist() {
        assert_eq!(
            parse_rendezvous_dir(Ok("   ".to_string())),
            Err(RendezvousConfigError::EmptyDir)
        );
    }

    async fn temp_event_store() -> (TempDir, Arc<SqliteEventStore>) {
        let home = TempDir::new().expect("event-store home");
        let store = SqliteEventStore::open_path(&home.path().join("events.sqlite"))
            .await
            .expect("open temp event store");
        (home, Arc::new(store))
    }

    // what this catches: the folder door pairs an `Always` gate (never a
    // gh-auth gate that would block a no-GitHub rendezvous), and the boxed
    // store it hands back is a REAL working fs store — publishing routes a
    // file into the shared folder. This is the zero-network on-prem path.
    #[tokio::test]
    async fn folder_choice_pairs_always_gate_and_writes_to_the_folder() {
        let (_home, event_store) = temp_event_store().await;
        let share = TempDir::new().expect("rendezvous share dir");
        let (store, gate) = resolve_account_registry_store(
            RendezvousChoice::Folder {
                dir: share.path().to_path_buf(),
            },
            GistRendezvous {
                event_store,
                scope_home: PathBuf::from("/unused/for/folder"),
                gh_bin: None,
                token_override: GhTokenOverride::new(),
            },
        );
        assert!(
            matches!(gate, RegistryRefreshGate::Always),
            "folder door must pair the no-auth Always gate"
        );

        let mesh = MeshIdentity::new("joelteply");
        let document =
            crate::account_registry::AccountRegistryDocument::new(mesh.clone(), 0, vec![], vec![]);
        store
            .publish(&document)
            .await
            .expect("folder store publishes");

        // A file landed under <share>/<sanitized-identity>/ — proof the
        // boxed store is the fs store, wired end to end, no GitHub.
        let identity_dir = share.path().join(mesh.as_str());
        let entries: Vec<_> = std::fs::read_dir(&identity_dir)
            .expect("identity dir exists after publish")
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(entries.len(), 1, "exactly one writer file published");
    }

    // what this catches: the gist door pairs the gh-auth gate carrying the
    // SAME scope_home it was given — the coupling that lets stale-token
    // recovery re-probe the right home.
    #[tokio::test]
    async fn gist_choice_pairs_ghauth_gate_with_the_scope_home() {
        let (_home, event_store) = temp_event_store().await;
        let scope_home = PathBuf::from("/scope/home/gist");
        let (_store, gate) = resolve_account_registry_store(
            RendezvousChoice::Gist,
            GistRendezvous {
                event_store,
                scope_home: scope_home.clone(),
                gh_bin: None,
                token_override: GhTokenOverride::new(),
            },
        );
        match gate {
            RegistryRefreshGate::GhAuth {
                scope_home: gated_home,
                ..
            } => assert_eq!(gated_home, scope_home),
            other => panic!("gist door must pair a GhAuth gate, got {other:?}"),
        }
    }
}
