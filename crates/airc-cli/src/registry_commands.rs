//! `airc registry sync` — manual one-shot of the account-registry
//! publish/refresh the daemon otherwise runs on a cadence (keystone
//! card a134b370-10b1-49c6-aa42-e1a05446e887). Bootstraps a fresh
//! machine and proves same-account discovery on demand.

use std::path::Path;
use std::sync::Arc;

use airc_lib::{machine_account_home, Airc, GhAccountRegistryStore, RegistryRefreshGate};
use airc_store::SqliteEventStore;

pub async fn run_sync(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;

    // The gh sentinel + registry rows live in the machine-account
    // home's events.sqlite — the same DB the daemon's store uses.
    let db_path = machine_account_home(home).join("events.sqlite");
    let event_store = Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let store = GhAccountRegistryStore::new(event_store);
    let gate = RegistryRefreshGate::GhAuth { gh_bin: None };

    match airc_lib::registry_sync_once(&airc, &store, &gate).await? {
        Some(report) => {
            println!(
                "registry sync ok: published {} peer(s) to the account rendezvous; \
                 enrolled {} peer(s) from the merged registry.",
                report.published_peers, report.enrolled_peers
            );
            if report.enrolled_peers <= 1 {
                println!(
                    "  (only this node is visible so far — run `airc registry sync` on \
                     another machine signed into the same GitHub account to converge.)"
                );
            }
        }
        None => {
            println!(
                "registry sync skipped: `gh` is not authenticated. The account-mesh \
                 rendezvous is the same-account gist transport — sign in with `gh auth \
                 login` to enable zero-touch cross-machine discovery. (This is optional, \
                 like every airc transport; LAN/relay/Reticulum paths are unaffected.)"
            );
        }
    }
    Ok(())
}
