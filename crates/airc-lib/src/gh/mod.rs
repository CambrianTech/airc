//! GitHub access — one subtree, one chokepoint.
//!
//! Everything that touches GitHub lives here so the boundary is
//! discoverable and enforceable (per the account-mesh join contract:
//! nothing calls GitHub except through the governed path):
//!
//! - [`client`] — the typed [`client::GhClient`] trait, the *only* gh
//!   surface consumers program against (mechanism).
//! - [`governor`] — the [`governor::GhBudget`] request governor: the
//!   single file-locked counter + GitHub-header-driven backoff (policy).
//! - [`account_registry`] — the gist-backed account-mesh registry store,
//!   the periodic gh consumer, which routes its calls through the
//!   governor.
//!
//! Crate-root re-exports (`airc_lib::GhClient`, `GhAccountRegistryStore`,
//! `GhBudget`, …) keep consumers on stable names; the module split is an
//! internal organization concern.

pub mod account_registry;
pub mod client;
pub mod governor;
