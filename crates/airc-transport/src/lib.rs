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
//! - **udp** — future low-latency realtime/game/live control path.
//! - **gh-gist** — bootstrap/rendezvous beacon only. It is not a
//!   `Transport`; GitHub can publish identity/address invitations, not
//!   sustained chat/event frames.
//!
//! Designed agent-first. The canonical caller is an AI peer sending
//! frames to other AI peers; human-chat features (typing, presence)
//! ride on top via `FrameKind::Event` and headers.

pub mod error;
pub mod gh_gist;
pub mod lan_tcp;
pub mod local_fs;
pub mod relay;
pub mod signed;
pub mod transport;
pub mod udp;
pub mod webrtc_datachannel;

// Re-exports — stable public surface.
pub use error::LocalFsError;
pub use gh_gist::{GhCliClient, GhGistError, GhGistInviteStore, GistClient, GIST_INVITE_FILE};
pub use lan_tcp::{
    build_client_config, build_server_config, extract_ed25519_pubkey, generate_self_signed_cert,
    CertGenError, CertParseError, LanTcpAdapter, LanTcpError, PinnedClientVerifier,
    PinnedServerVerifier, TlsConfigError,
};
pub use local_fs::LocalFsAdapter;
pub use relay::{RelayAdapter, RelayClientConfig, RelayClientError};
pub use signed::{SignedError, SignedTransport};
pub use transport::{FrameStream, Transport};
pub use udp::{UdpAdapter, UdpConfig, UdpError};
pub use webrtc_datachannel::{WebRtcDataChannelAdapter, WebRtcDataChannelError};
