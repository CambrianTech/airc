//! airc-transport — the wire layer between `airc-protocol` envelopes and
//! whatever bytes actually move between peers.
//!
//! One trait, many adapters. The substrate is intentionally agnostic
//! about the underlying transport so AI peers (Claude Code, Codex,
//! vHSM sessions, personas, OpenClaw extensions, Hermes agents,
//! OpenCode app-server bridges) can pick whatever path makes sense:
//!
//! - **local-fs** (this PR) — same-machine multi-process via an
//!   append-only JSONL log. The direct retirement path for gh-polling
//!   when multiple AI agents share one Mac.
//! - **lan-tcp** (PR-3) — same-LAN peer-to-peer via TLS-wrapped TCP.
//! - **tailscale** (PR-4) — mesh transport for cross-network peers.
//! - **gh-gist** (legacy adapter, eventual removal) — wraps the
//!   current Python-airc gist transport so a Rust peer can talk to a
//!   Python peer during the migration window.
//!
//! Designed agent-first. The canonical caller is an AI peer sending
//! frames to other AI peers; human-chat features (typing, presence)
//! ride on top via `FrameKind::Event` and headers.

pub mod error;
pub mod lan_tcp;
pub mod local_fs;
pub mod transport;

// Re-exports — stable public surface.
pub use error::LocalFsError;
pub use lan_tcp::{
    build_client_config, build_server_config, extract_ed25519_pubkey, generate_self_signed_cert,
    CertGenError, CertParseError, LanTcpAdapter, LanTcpError, PinnedClientVerifier,
    PinnedServerVerifier, TlsConfigError,
};
pub use local_fs::LocalFsAdapter;
pub use transport::{FrameStream, Transport};
