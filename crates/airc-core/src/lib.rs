//! Typed AIRC core substrate primitives.
//!
//! This crate is intentionally storage-neutral. SQLite, GitHub gists, local
//! files, and future transports adapt to these types instead of owning the
//! transcript model. Continuum and other consumers (OpenClaw, Hermes, IRC-
//! style clients, game-lobby orchestrators) consume generated/API shapes
//! built from these Rust contracts, not query AIRC storage directly.
//!
//! Module layout — small files, one concern per file, per Joel's
//! "as small as possible from the start" + "many files per module"
//! direction:
//!
//! - [`ids`]        — newtype wrappers for identifier strings
//! - [`identity`]   — the per-peer Identity card (the user/account abstraction)
//! - [`transcript`] — TranscriptEvent + TranscriptKind + MentionTarget
//! - [`cursor`]     — cursor + paging primitives for transcript fetch
//! - [`receipt`]    — delivered/read/applied acknowledgments
//! - [`attachment`] — file-attachment manifest (consumer-side richer view)
//! - [`filter`]     — self-echo filtering for multi-tab consumers
//!
//! Every public type a consumer needs is re-exported at the crate root so
//! `use airc_core::Identity;` works without knowing the module split.

pub mod attachment;
pub mod cursor;
pub mod filter;
pub mod identity;
pub mod ids;
pub mod receipt;
pub mod transcript;

// Re-exports — the public API surface. Callers that want stable imports
// use `airc_core::Foo`; internal cross-module references use the explicit
// module paths so refactors stay clear.

pub use attachment::AttachmentManifest;
pub use cursor::{page_before, page_recent, TranscriptCursor, TranscriptPage};
pub use filter::{filter_self_echoes, SelfFilter};
pub use identity::Identity;
pub use ids::{ClientId, ContentHash, EventId, FileId, PeerId, RoomId};
pub use receipt::{Receipt, ReceiptKind};
pub use transcript::{MentionTarget, TranscriptEvent, TranscriptKind};
