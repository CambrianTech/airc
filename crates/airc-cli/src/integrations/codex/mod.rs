//! OpenAI Codex integration boundary.
//!
//! Codex-specific config mutation, detached feed startup, and the
//! Codex lines of the shared hook core (`integrations::agent_hook`) live here. Generic CLI command handlers
//! should call this boundary instead of naming Codex implementation
//! files directly.

pub mod cli;
pub mod config;
pub mod hook;
pub mod install;
pub mod start;

pub use cli::{CodexHookArgs, CodexStartArgs};
