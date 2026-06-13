//! OpenAI Codex integration boundary.
//!
//! Codex-specific hook installation, hook rendering, config mutation,
//! and detached feed startup live here. Generic CLI command handlers
//! should call this boundary instead of naming Codex implementation
//! files directly.

pub mod cli;
pub mod config;
pub mod hook;
pub mod hooks_json;
pub mod install;
pub mod start;

pub use cli::{CodexHookAction, CodexHookArgs, CodexStartArgs};
