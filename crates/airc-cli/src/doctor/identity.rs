//! Identity check — `identity.key` + the `local_identity` ORM row.
//!
//! The most common new-machine friction, and the one where automatic
//! recovery is deliberately withheld: wiping a `peer_id` discards every
//! trust relationship remote peers enrolled against it, so partial state
//! is REPORTED with an exact one-liner rather than silently repaired.

use std::path::Path;

use airc_identity::LocalIdentity;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Identity pairing through read-only observation, so it
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
    let key_exists = match key_path.try_exists() {
        Ok(exists) => exists,
        Err(error) => {
            return vec![Finding::blocked(
                "identity key",
                format!("cannot inspect identity.key: {error}"),
                "restore access to the existing key; preserve identity",
            )]
        }
    };
    let metadata_exists = match home.join("identity.json").try_exists() {
        Ok(exists) => exists,
        Err(error) => {
            return vec![Finding::blocked(
                "identity metadata",
                format!("cannot inspect identity.json: {error}"),
                "restore access to existing identity material",
            )]
        }
    };
    let path = home.join("events.sqlite");
    let database_exists = match path.try_exists() {
        Ok(exists) => exists,
        Err(error) => {
            return vec![Finding::blocked(
                "identity store",
                format!("cannot inspect events.sqlite: {error}"),
                "restore access to the existing store; preserve identity and scope state",
            )]
        }
    };
    if !database_exists {
        return if key_exists || metadata_exists {
            vec![Finding::blocked("identity", "identity material exists without its database", "restore events.sqlite from backup; preserve the existing key and identity material")]
        } else {
            vec![Finding::info(
                "identity",
                "no identity material (fresh scope; `airc join` will generate)",
            )]
        };
    }
    let store = match airc_store::StoreObservation::open(&path).await {
        Ok(store) => store,
        Err(error) => {
            return vec![Finding::blocked(
            "identity store",
            format!("cannot observe events.sqlite: {error}"),
            "check disk and permissions; preserve the database and restore from backup if damaged",
        )]
        }
    };
    let present = match store.has_identity(airc_store::DEFAULT_AGENT_NAME).await {
        Ok(present) => present,
        Err(error) => {
            return vec![Finding::blocked(
                "identity row",
                format!("cannot query local_identity: {error}"),
                "repair the store through its supported lifecycle; diagnostics never migrate it",
            )]
        }
    };
    match (key_exists, present) {
        (true, true) => vec![Finding::ok("identity", "key + ORM row both present")],
        (false, false) if metadata_exists => vec![Finding::blocked(
            "identity",
            "identity metadata has no matching key or database identity",
            "restore the matching identity state from backup; preserve existing material",
        )],
        (false, false) => vec![Finding::info(
            "identity",
            "no identity material (fresh scope; `airc join` will generate)",
        )],
        (true, false) => vec![Finding::blocked(
            "identity",
            "key present but identity row missing",
            "restore the matching database from backup; preserve the key",
        )],
        (false, true) => vec![Finding::blocked(
            "identity",
            "identity row present but signing key missing",
            "restore the matching identity.key from backup; preserve the database",
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
        assert!(!dir.path().join("events.sqlite").exists());
    }

    #[tokio::test]
    async fn key_without_row_is_blocked_with_clear_fix() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("identity.key"), [7u8; 32]).unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Blocked);
        let fix = findings[0].fix.as_ref().unwrap();
        assert!(
            fix.contains("restore"),
            "recovery must preserve identity: {fix}"
        );
        assert!(
            !fix.contains("rm "),
            "diagnosis must not recommend wiping identity: {fix}"
        );
        assert!(!dir.path().join("events.sqlite").exists());
    }

    #[tokio::test]
    async fn metadata_without_database_is_blocked_without_migration() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("identity.key"), [7u8; 32]).unwrap();
        std::fs::write(dir.path().join("identity.json"), "{}").unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Blocked);
        assert!(!dir.path().join("events.sqlite").exists());
        assert!(dir.path().join("identity.json").exists());
    }
}
