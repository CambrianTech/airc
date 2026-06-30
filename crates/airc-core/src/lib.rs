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
//! - [`persona`]    — typed persona capability metadata on Identity.integrations
//! - [`body`]       — opaque payload (Json | Binary) consumers carry
//! - [`transcript`] — TranscriptEvent + TranscriptKind + MentionTarget
//! - [`cursor`]     — cursor + paging primitives for transcript fetch
//! - [`datetime`]   — fixed-format UTC timestamp parsing
//! - [`receipt`]    — delivered/read/applied acknowledgments
//! - [`attachment`] — file-attachment manifest (consumer-side richer view)
//! - [`filter`]     — self-echo filtering for multi-tab consumers
//! - [`headers`]    — envelope headers for routing/filtering without body parse
//! - [`humanhash`]  — stable hash mnemonics for invite/client labels
//! - [`temp_home`]  — temp-rooted scope-home detection (hermetic test daemons)
//!
//! Every public type a consumer needs is re-exported at the crate root so
//! `use airc_core::Identity;` works without knowing the module split.

pub mod attachment;
pub mod body;
pub mod channel_purpose;
pub mod cursor;
pub mod datetime;
pub mod doctrine;
pub mod filter;
pub mod headers;
pub mod humanhash;
pub mod identity;
pub mod ids;
pub mod persona;
pub mod receipt;
pub mod scoped_state;
pub mod temp_home;
pub mod transcript;

// Re-exports — the public API surface. Callers that want stable imports
// use `airc_core::Foo`; internal cross-module references use the explicit
// module paths so refactors stay clear.

pub use attachment::AttachmentManifest;
pub use body::Body;
pub use cursor::{page_before, page_recent, TranscriptCursor, TranscriptPage};
pub use datetime::{iso_to_epoch, DateTimeError};
pub use filter::{filter_self_echoes, SelfFilter};
pub use headers::{HeaderFilter, Headers};
pub use humanhash::{humanhash, HumanhashError};
pub use identity::Identity;
pub use ids::{ClientId, ContentHash, EventId, FileId, PeerId, RoomId};
pub use persona::{PersonaCapabilities, PersonaCapabilitiesError, PERSONA_CAPABILITIES_KEY};
pub use receipt::{Receipt, ReceiptKind};
pub use scoped_state::{ScopeRef, ScopedStateEntry, PEER_IDENTITY_STATE_KEY};
pub use temp_home::scope_home_is_temp_rooted;
pub use transcript::{MentionTarget, TranscriptEvent, TranscriptKind};
