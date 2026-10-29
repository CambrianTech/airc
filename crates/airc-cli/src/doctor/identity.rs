//! Identity check — `identity.key` + the `local_identity` ORM row.
//!
//! The most common new-machine friction, and the one where automatic
//! recovery is deliberately withheld: wiping a `peer_id` discards every
//! trust relationship remote peers enrolled against it, so partial state
//! is REPORTED with an exact one-liner rather than silently repaired.

use std::path::Path;

use airc_identity::LocalIdentity;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Identity pairing. Cheap (two `exists()` probes + one sqlite open), so it
/// inherits the default [`super::CheckTier::Always`].
pub(super) struct IdentityCheck;

#[async_trait::async_trait]
impl Check for IdentityCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::always("identity")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_identity(ctx.home).await
    }
}

/// Walks the same partial-state logic `LocalIdentity::load_or_generate`
/// does, but reports rather than fails.
async fn check_identity(home: &Path) -> Vec<Finding> {
    let key_path = LocalIdentity::key_path(home);
    let key_exists = key_path.exists();
    // Probe legacy json so a half-migrated install is named for what
    // it is, not just "row missing".
    let legacy_json = home.join("identity.json").exists();

    // Open the store to ask about the singleton row. If the store
    // itself can't open, surface that instead — that's a different
    // class of breakage (disk full, permissions, db corruption).
    let store = match airc_store::SqliteEventStore::open_path(&home.join("events.sqlite")).await {
        Ok(store) => store,
        Err(error) => {
            return vec![Finding::blocked(
                "identity store",
                format!("can't open events.sqlite: {error}"),
                "check disk/permissions; if corrupted, `airc stop` then `rm <home>/events.sqlite` and `airc join` to rebuild (loses scope state)",
            )];
        }
    };
    let row = match store.load_local_identity().await {
        Ok(opt) => opt,
        Err(error) => {
            return vec![Finding::blocked(
                "identity row",
                format!("can't query local_identity: {error}"),
                "schema may be from an older binary; `airc update` or rebuild",
            )];
        }
    };

    match (key_exists, row.is_some(), legacy_json) {
        (true, true, _) => vec![Finding::ok("identity", "key + ORM row both present")],
        (false, false, false) => vec![Finding::info(
            "identity",
            "no identity material (fresh scope; `airc join` will generate)",
        )],
        (false, false, true) => vec![Finding::warn(
            "identity",
            "legacy identity.json present without identity.key — orphan metadata",
            "`rm <home>/identity.json` then `airc join` to regenerate identity cleanly",
        )],
        (true, false, true) => vec![Finding::warn(
            "identity",
            "key present + legacy identity.json present, no ORM row — pre-#902 install",
            "`airc join` will auto-migrate (post-#902 logic; identity.json gets consumed)",
        )],
        (true, false, false) => vec![Finding::blocked(
            "identity",
            "key present but no ORM row and no legacy json — orphan key, no recovery without backup",
            "`airc stop` then `rm <home>/identity.key` (loses peer_id), then `airc join` to regenerate",
        )],
        (false, true, _) => vec![Finding::blocked(
            "identity",
            "ORM row present but key file missing — can't sign without the secret",
            "restore <home>/identity.key from backup, OR `airc stop` + `rm -rf <home>` then `airc join` (loses peer_id)",
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Status;

    #[tokio::test]
    async fn fresh_scope_reports_no_identity_material() {
        let dir = tempfile::TempDir::new().unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Info);
        assert!(findings[0].detail.contains("no identity material"));
    }

    #[tokio::test]
    async fn key_without_row_is_blocked_with_clear_fix() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("identity.key"), [7u8; 32]).unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Blocked);
        let fix = findings[0].fix.as_ref().unwrap();
        // what this catches: the fix must name a REAL recovery path —
        // wipe the orphan key, then regenerate — using verbs that exist
        // in the rust rewrite. `teardown`/`--flush` were legacy Python
        // verbs removed in the cutover; recommending them hands the user
        // a broken command (regression guard for that dead-verb drift).
        assert!(
            fix.contains("airc join"),
            "fix must point at the regenerate step: {fix}"
        );
        assert!(fix.contains("rm "), "fix must name the wipe step: {fix}");
        assert!(
            !fix.contains("teardown"),
            "must not recommend the removed teardown verb: {fix}"
        );
    }

    #[tokio::test]
    async fn key_plus_legacy_json_reports_pre_902_install() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("identity.key"), [7u8; 32]).unwrap();
        std::fs::write(dir.path().join("identity.json"), "{}").unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Warn);
        assert!(findings[0].detail.contains("pre-#902"));
    }
}
