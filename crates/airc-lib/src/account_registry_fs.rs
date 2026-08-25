//! Filesystem (shared-folder) account-registry rendezvous — the
//! **no-GitHub** door onto the same mesh gist opens.
//!
//! `[[airc-grid-identity-unification-trust-bridge]]` /
//! `[[positron-identity-security-first-class]]`: rendezvous **initiates**
//! the exchange, it does not **supply** the security. The security lives
//! in the E2E data plane (paired peer keys, trust tiers) regardless of
//! which door a node came through, so the rendezvous is free to be a dumb,
//! untrusted meeting point — exactly the Tailscale coordination-server
//! shape. [`crate::gh::GhAccountRegistryStore`] is one such door (gist,
//! chosen because our agents already live on GitHub for kanban/PRs). This
//! is a second, maximally-different one: a **shared folder** every machine
//! of one account can see — an iCloud Drive, a Syncthing folder, a hospital
//! NFS mount — needing **no `gh` CLI, no token, no network**. That is the
//! on-prem/behind-firewall grid rendezvous (Amit's hospital trial can't
//! reach GitHub), and it proves the [`AccountRegistryStore`] seam is
//! genuinely swappable: if gist went away, this is already a working
//! substitute.
//!
//! ## Fidelity: the SAME convergence core as gist
//!
//! gh keeps one gist **per writer/machine** on the account and `refresh`
//! fetches all of them and folds via [`merge_registry_documents`]
//! (freshest beacon per peer, union across machines, skip foreign-mesh,
//! prune stale). This store does the byte-identical thing on a filesystem:
//! each machine writes ITS document to its own per-writer file
//! (`<dir>/<identity>/<writer_file>`), so two machines never clobber, and
//! `refresh` reads every writer file for the identity and runs the exact
//! same [`merge_registry_documents`] + [`prune_stale_peers`] core. So the
//! two rendezvous transports converge identically — the merge/freshness
//! logic lives in exactly one place (`[[compression-principle]]`).

use std::path::PathBuf;

use async_trait::async_trait;

use crate::account_registry::{
    merge_registry_documents, prune_stale_peers, AccountRegistryDocument, AccountRegistryError,
    AccountRegistryStore, DEFAULT_PEER_FRESHNESS_TTL_MS,
};
use crate::subscriptions::MeshIdentity;

/// A shared-folder [`AccountRegistryStore`]: publish/refresh the account's
/// mesh registry through a directory N machines share (iCloud / Syncthing /
/// NFS), with no GitHub dependency.
pub struct FsAccountRegistryStore {
    /// The shared rendezvous root every machine of the account can see.
    dir: PathBuf,
    /// This machine's stable per-writer document basename. Production
    /// passes [`crate::gh::writer_filename`] so fs and gh agree on writer
    /// identity (`[[compression-principle]]` — one writer-identity source);
    /// tests pass distinct names to simulate several machines sharing one
    /// folder. MUST end in `.json` (the read filter ignores everything else,
    /// which is what makes the store robust to OS cruft an iCloud folder
    /// carries — `.DS_Store`, `.icloud` placeholders).
    writer_file: String,
}

impl FsAccountRegistryStore {
    /// `dir` = the shared rendezvous folder; `writer_file` = this machine's
    /// per-writer `.json` document basename (see the field doc for the
    /// production vs test contract).
    pub fn new(dir: impl Into<PathBuf>, writer_file: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            writer_file: writer_file.into(),
        }
    }

    /// One filesystem-safe subdir per mesh identity. The merge still filters
    /// by exact `mesh_identity`, so a sanitize collision can only ever
    /// UNDER-partition (two identities land in one dir) and the merge drops
    /// the foreign-mesh docs — never a correctness bug, just tidiness.
    fn identity_dir(&self, identity: &MeshIdentity) -> PathBuf {
        self.dir
            .join(sanitize_identity_component(identity.as_str()))
    }
}

#[async_trait]
impl AccountRegistryStore for FsAccountRegistryStore {
    async fn publish(
        &self,
        document: &AccountRegistryDocument,
    ) -> Result<(), AccountRegistryError> {
        document.validate()?;
        let body = serde_json::to_string_pretty(document).map_err(|error| {
            AccountRegistryError::Adapter(format!("serialize registry document: {error}"))
        })?;
        let dir = self.identity_dir(&document.mesh_identity);
        tokio::fs::create_dir_all(&dir).await.map_err(|error| {
            AccountRegistryError::Adapter(format!(
                "create rendezvous dir {}: {error}",
                dir.display()
            ))
        })?;
        // Atomic publish: write a temp file in the SAME dir (so the rename
        // is atomic on one filesystem) then rename over the writer file, so
        // a concurrent `refresh` on another machine never reads a
        // half-written document. The temp name is pid-scoped so two writers
        // never collide on the staging file.
        let final_path = dir.join(&self.writer_file);
        let tmp_path = dir.join(format!("{}.{}.tmp", self.writer_file, std::process::id()));
        tokio::fs::write(&tmp_path, body.as_bytes())
            .await
            .map_err(|error| {
                AccountRegistryError::Adapter(format!("write {}: {error}", tmp_path.display()))
            })?;
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .map_err(|error| {
                AccountRegistryError::Adapter(format!(
                    "rename {} -> {}: {error}",
                    tmp_path.display(),
                    final_path.display()
                ))
            })?;
        Ok(())
    }

    async fn refresh(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError> {
        let dir = self.identity_dir(mesh_identity);
        let mut read_dir = match tokio::fs::read_dir(&dir).await {
            Ok(read_dir) => read_dir,
            // No writer has published for this identity yet — legitimately
            // empty, not a fault (mirrors gh's "no gist → None").
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AccountRegistryError::Adapter(format!(
                    "read rendezvous dir {}: {error}",
                    dir.display()
                )))
            }
        };

        let mut documents = Vec::new();
        while let Some(entry) = read_dir.next_entry().await.map_err(|error| {
            AccountRegistryError::Adapter(format!(
                "iterate rendezvous dir {}: {error}",
                dir.display()
            ))
        })? {
            let path = entry.path();
            // Only writer documents. This skips both the in-flight `.tmp`
            // staging files a concurrent publish is mid-rename on AND any OS
            // cruft a synced folder carries (`.DS_Store`, `.icloud`).
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let bytes = tokio::fs::read(&path).await.map_err(|error| {
                AccountRegistryError::Adapter(format!("read {}: {error}", path.display()))
            })?;
            // Fail loud on a corrupt writer file: a shared-folder rendezvous
            // holding a malformed document is an operator-visible fault, not
            // something to silently skip (`[[fallbacks-are-illegal-fail-loud]]`).
            let document: AccountRegistryDocument =
                serde_json::from_slice(&bytes).map_err(|error| {
                    AccountRegistryError::Adapter(format!(
                        "parse registry document {}: {error}",
                        path.display()
                    ))
                })?;
            documents.push(document);
        }

        // The SAME convergence core gist uses — one source of truth for the
        // merge/freshness rules across both rendezvous transports.
        let outcome = merge_registry_documents(documents, mesh_identity);
        let Some(mut document) = outcome.document else {
            return Ok(None);
        };
        let now_ms = crate::time::now_ms().map_err(|error| {
            AccountRegistryError::Adapter(format!("system clock before unix epoch: {error}"))
        })?;
        prune_stale_peers(&mut document.peers, now_ms, DEFAULT_PEER_FRESHNESS_TTL_MS);
        Ok(Some(document))
    }
}

/// One safe filesystem path component from a mesh-identity string.
fn sanitize_identity_component(identity: &str) -> String {
    let mut out = String::with_capacity(identity.len());
    for ch in identity.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        // A `MeshIdentity::unset()` still gets a stable, non-empty home.
        out.push_str("_unset");
    }
    out
}

#[cfg(test)]
mod tests {
    // what this catches: the shared-folder rendezvous must round-trip a
    // published document AND — the outlier-B proof — two DIFFERENT machines
    // writing per-writer files into ONE shared folder must CONVERGE through
    // the same merge core (both peers visible from either machine), with
    // zero GitHub. A regression that made publish clobber (single shared
    // file) or that skipped the merge would drop one machine's peer here.
    use super::*;
    use crate::account_registry::AccountPeerBeacon;
    use crate::account_registry::AccountRoom;
    use crate::route::RouteEndpoint;
    use crate::subscriptions::MeshIdentity;
    use crate::PeerSpec;
    use airc_core::PeerId;
    use airc_core::RoomId;
    use airc_protocol::PeerKeypair;
    use tempfile::TempDir;

    // Year-2100 heartbeat: `refresh` runs the reader-side freshness pass
    // (`prune_stale_peers` vs real `now_ms`), so a beacon stamped in the
    // 1970s would be pruned as a dead route before we ever see it. A
    // future-dated beacon is always fresh (saturating-sub guard), which is
    // exactly how the account_registry tests' `write_identity` helper dodges
    // the same clock coupling.
    const FRESH_MS: u64 = 4_102_444_800_000;

    fn mesh() -> MeshIdentity {
        MeshIdentity::new("joelteply")
    }

    fn peer_spec(peer_id: PeerId) -> PeerSpec {
        PeerSpec {
            peer_id,
            pubkey: PeerKeypair::generate().public_bytes(),
        }
    }

    // A beacon with a NON-temp scope_home (a temp-rooted one is dropped by
    // the merge's hermetic guard) carrying one dialable relay endpoint.
    fn beacon(peer_id: PeerId, scope_home: &str, relay: &str) -> AccountPeerBeacon {
        AccountPeerBeacon {
            endpoints_advertised_at_ms: None,
            endpoints_peer_id: None,
            presence: crate::coordinator::beacon_now(
                peer_id,
                scope_home.into(),
                vec![RoomId::from_u128(1)],
                123,
                FRESH_MS,
            ),
            peer_spec: peer_spec(peer_id),
            endpoints: vec![RouteEndpoint::Relay {
                url: relay.to_string(),
            }],
        }
    }

    fn doc_for(peer: &AccountPeerBeacon, generated_at_ms: u64) -> AccountRegistryDocument {
        AccountRegistryDocument::new(
            mesh(),
            generated_at_ms,
            vec![AccountRoom::new(
                RoomId::from_u128(1),
                Some("general".to_string()),
            )],
            vec![peer.clone()],
        )
    }

    #[tokio::test]
    async fn publish_then_refresh_round_trips_the_document() {
        let dir = TempDir::new().unwrap();
        let store = FsAccountRegistryStore::new(
            dir.path().to_path_buf(),
            "airc-account-mesh-registry.machine-a.json",
        );
        let peer = PeerId::new();
        let document = doc_for(
            &beacon(peer, "/machine/a/.airc", "https://a.example.test"),
            5_000,
        );

        store.publish(&document).await.expect("publish");
        let refreshed = store
            .refresh(&mesh())
            .await
            .expect("refresh")
            .expect("a document exists after publish");

        assert_eq!(refreshed.mesh_identity, mesh());
        assert_eq!(refreshed.peers.len(), 1);
        assert_eq!(refreshed.peers[0].peer_id(), peer);
    }

    #[tokio::test]
    async fn two_machines_sharing_a_folder_converge_via_merge() {
        // ONE shared rendezvous folder (the iCloud/NFS mount), TWO machines
        // — modelled as two stores over the SAME dir with DISTINCT writer
        // files (production derives these from host-user; here they're
        // explicit so one process can play both machines).
        let shared = TempDir::new().unwrap();
        let machine_a = FsAccountRegistryStore::new(
            shared.path().to_path_buf(),
            "airc-account-mesh-registry.machine-a.json",
        );
        let machine_b = FsAccountRegistryStore::new(
            shared.path().to_path_buf(),
            "airc-account-mesh-registry.machine-b.json",
        );

        let peer_a = PeerId::new();
        let peer_b = PeerId::new();
        machine_a
            .publish(&doc_for(
                &beacon(peer_a, "/machine/a/.airc", "https://a.example.test"),
                5_000,
            ))
            .await
            .expect("machine a publishes");
        machine_b
            .publish(&doc_for(
                &beacon(peer_b, "/machine/b/.airc", "https://b.example.test"),
                6_000,
            ))
            .await
            .expect("machine b publishes");

        // Either machine, refreshing the shared folder, sees BOTH peers —
        // the per-writer files did not clobber and the merge unioned them.
        for (label, store) in [("a", &machine_a), ("b", &machine_b)] {
            let merged = store
                .refresh(&mesh())
                .await
                .expect("refresh")
                .unwrap_or_else(|| panic!("machine {label} sees a merged document"));
            let ids: Vec<PeerId> = merged.peers.iter().map(|p| p.peer_id()).collect();
            assert!(
                ids.contains(&peer_a) && ids.contains(&peer_b),
                "machine {label} must see both peers via the merge, saw {ids:?}"
            );
        }
    }

    #[tokio::test]
    async fn refresh_of_an_unpublished_identity_is_none_not_error() {
        let dir = TempDir::new().unwrap();
        let store = FsAccountRegistryStore::new(dir.path().to_path_buf(), "writer.json");
        // Nothing published for this identity → empty, not a fault.
        assert!(store.refresh(&mesh()).await.expect("refresh").is_none());
    }
}
