//! Command-line interface definitions (clap derive).
//!
//! All commands default to the persisted state at `<home>` (default
//! the current git project's `.airc`), which contains:
//!   - `identity.key`   — 32-byte Ed25519 secret (0600 on Unix)
//!   - daemon IPC endpoint, derived from scope + IPC protocol version
//!   - `events.sqlite`  — ORM-backed identity metadata, events, cursors, peer
//!     trust, subscriptions, and coordinator state
//!
//! The `--home` flag overrides for testing / multi-identity setups.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use airc_lib::PeerSpec;

use crate::channel_gist_cli::ChannelGistArgs;
use crate::collaboration_cli::CollaborationArgs;
use crate::envelope_cli::EnvelopeArgs;
use crate::gh_cli::GhArgs;
use crate::gist_cli::GistArgs;
use crate::handshake_cli::HandshakeArgs;
use crate::hygiene_cli::HygieneArgs;
use crate::identity_cli::IdentityArgs;
use crate::integrations::codex::{CodexHookArgs, CodexStartArgs};
use crate::knock_cli::KnockArgs;
use crate::pending_cli::PendingArgs;
use crate::route_cli::RouteArgs;
use crate::state_cli::StateArgs;
use crate::transport_cli::TransportArgs;
use crate::work_cli::WorkArgs;

mod home;
mod peer;

pub use home::{default_home_dir, explicit_home_override};
pub use peer::{PeerAction, PeerArgs};

/// The canonical IPC socket derivation now lives in `airc-lib`
/// ([`airc_lib::socket_path`]), because a BINARY-ONLY crate cannot be called by anything
/// else — which is why every other program that needed this path had to spawn `airc
/// ipc-endpoint`, a process launch to evaluate a pure function of a directory.
///
/// Re-exported, not reimplemented: ONE derivation, no second copy to drift, no fallback.
pub use airc_lib::socket_path::default_socket_path_in;

/// AIRC substrate CLI.
#[derive(Debug, Parser)]
#[command(
    name = "airc",
    version,
    about = "AIRC substrate CLI",
    long_about = "Cross-process / cross-machine AI chat over the airc substrate. \
                  Provides the public AIRC command surface."
)]
pub struct Cli {
    /// State directory for persisted identity + IPC socket. Defaults
    /// to the current git project root's `.airc` unless `$AIRC_HOME`
    /// is set. Override for tests or multi-identity setups.
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    /// Use a cwd-LOCAL `.airc` scope (`$PWD/.airc`) for this invocation,
    /// instead of the default machine-account / git-project resolution
    /// (seam #1). The ergonomic shortcut for `AIRC_HOME=$PWD/.airc` —
    /// the explicit, intentional way to mint or use a per-directory
    /// scope (`airc init --here`, `airc join --here`) when you genuinely
    /// want one, which is what lets the default safely resolve to the
    /// machine-account home rather than a throwaway cwd scope. Conflicts
    /// with `--home`.
    #[arg(long, global = true, conflicts_with = "home")]
    pub here: bool,

    /// Ad-hoc peers to enrol for this invocation only, repeatable.
    /// Format: `<uuid>:<base64-pubkey-no-padding>`. Persistent peers
    /// come from the peer trust store (managed via `airc peer add`);
    /// this flag unions on top for one-shot use.
    #[arg(long = "peer", value_name = "SPEC", global = true)]
    pub peers: Vec<PeerSpec>,

    #[command(subcommand)]
    pub command: Command,
}

/// The `--room <NAME>` selector, declared ONCE for every room-scoped
/// verb (`send`, `msg`, `publish`, `inbox`).
///
/// It used to be hand-copied per subcommand — three near-identical
/// `room: Option<String>` fields with three near-identical doc
/// comments, and `inbox` never got one at all. That asymmetry is not a
/// missing flag, it is what a per-command declaration DOES: the writes
/// could each name a room while the READ could only see whichever room
/// the scope happened to be sitting in, so reading a subscribed room
/// required `airc room <name>` — mutating shared scope state to perform
/// a read (#270). One struct, flattened everywhere, means a new
/// room-scoped verb inherits the flag instead of re-deriving it.
///
/// Resolution is likewise single-source: `Airc::room_by_name_or_channel`
/// accepts a name OR a channel id, refuses loudly for a room this scope
/// is not subscribed to, and NEVER auto-joins.
#[derive(Debug, Args)]
pub struct RoomSelector {
    /// Channel name (or channel id) to act on. Must already be
    /// subscribed — this never auto-joins. Defaults to the current
    /// room. Using it does NOT move this scope's default-room pointer.
    #[arg(long)]
    pub room: Option<String>,
}

impl RoomSelector {
    /// The room the caller named, if any. `None` means "the current room".
    pub fn named(&self) -> Option<&str> {
        self.room.as_deref()
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or load the persisted identity (`<home>/identity.key`
    /// plus ORM-backed metadata), then print this peer's spec for
    /// out-of-band sharing. Idempotent — repeat runs return the same
    /// peer_id.
    Init {
        /// Local agent identity name. Same effect as `AIRC_AGENT_NAME`,
        /// but explicit CLI input takes precedence.
        #[arg(long = "as", value_name = "AGENT_NAME")]
        agent_name: Option<String>,
    },

    /// Print the primary non-loopback LAN IPv4 address, if detectable.
    LanIp,

    /// Inspect collaboration health during Rust cutover.
    Collaboration(CollaborationArgs),

    /// Resolve channel-to-gist discovery state during Rust cutover.
    ChannelGist(ChannelGistArgs),

    /// Identity and whois helpers during Rust cutover.
    Identity(IdentityArgs),

    /// Inspect and edit private scoped state (prefs, cursors, widget UI
    /// state). The peer-private sibling of the room wall.
    State(StateArgs),

    /// Legacy envelope encryption helpers during Rust cutover.
    Envelope(EnvelopeArgs),

    /// Send a single text Message frame to a subscribed room and
    /// exit. With `--room`, sends to that room without mutating this
    /// scope's default-room pointer (one-shot routing — same shape
    /// as `airc publish --room`). Without `--room`, the default
    /// channel lives in the ORM store.
    Send {
        #[command(flatten)]
        room: RoomSelector,
        /// Message body.
        text: String,
    },

    /// Subscribe to the current room and print frames until
    /// interrupted (Ctrl-C).
    Listen {
        /// Replay from the start of the wire instead of live-only.
        #[arg(long)]
        replay: bool,
    },

    /// Self-healing join — manual recovery dial: connect to `HOST:PORT`
    /// with the full authenticated (mTLS-pinned) handshake and report
    /// LOUDLY. A successful dial also teaches the REMOTE our real
    /// source address (learn-live-address, #9), so a peer stuck dialing
    /// our stale endpoint can recover from one inbound contact. The
    /// expected peer is inferred from the trust store (stored endpoint
    /// match, else the identity-derived stable port); pass `--expected-peer`
    /// when inference is ambiguous.
    Dial {
        /// Endpoint to dial (e.g. `192.168.1.249:57958`).
        to: SocketAddr,
        /// UUID of the peer expected at that endpoint (for cert
        /// pinning). Inferred from the trust store when omitted.
        /// (`--peer` is the global volatile-peer-spec flag, hence the
        /// longer name — same convention as `lan-send`.)
        #[arg(long = "expected-peer")]
        peer: Option<String>,
        /// Dial + handshake deadline. Generous next to discovery's 3s
        /// budget — this is a hands-on recovery verb, not a sweep.
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
    },

    /// Same-LAN secure send: dial a peer over TLS, send a single
    /// text frame to the current room's channel, and wait for the
    /// receiver's typed delivery ack (card 39d37629). Exit 0 only on
    /// `delivered`; undeliverable or no-ack outcomes exit nonzero.
    LanSend {
        /// Address of the listening peer (e.g. `127.0.0.1:7474`).
        #[arg(long)]
        to: SocketAddr,
        /// UUID of the listening peer (for cert pinning).
        #[arg(long)]
        expected_peer: String,
        /// How long to wait for the receiver's delivery ack before
        /// reporting no-ack (older receivers never ack).
        #[arg(long, default_value_t = 10_000)]
        ack_timeout_ms: u64,
        /// Message body.
        text: String,
    },

    /// Same-LAN secure listen: bind a TLS server, accept peers,
    /// print received frames.
    LanListen {
        /// Bind address (e.g. `127.0.0.1:7474` or `0.0.0.0:7474`).
        #[arg(long)]
        bind: SocketAddr,
        /// Replay-mode subscription (defaults to live-only).
        #[arg(long)]
        replay: bool,
    },

    /// Start the daemon in the foreground. Holds substrate state so
    /// subsequent short-lived CLI calls (`ping`, `msg`, `status`)
    /// don't re-load identity or re-handshake.
    Daemon {
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Probe the daemon — returns immediately if alive.
    Ping {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Observe daemon health without starting it; use `join` to start or reconnect.
    Status {
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Emit schema_version=1 JSON: running, absent, or unknown. Unknown
        /// exits nonzero; absent means the endpoint was missing or refused.
        #[arg(long)]
        json: bool,
    },

    /// Stop the account daemon and retain operator intent until explicit `join`.
    Stop {
        /// Must match this home's canonical endpoint; another owner is refused.
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Send a text message to a subscribed room via the running
    /// daemon (fast — no per-call substrate setup). With `--room`,
    /// sends to that room without mutating this scope's
    /// default-room pointer (one-shot routing — same shape as
    /// `airc publish --room`). Without `--room`, defaults to the
    /// current room.
    Msg {
        /// Address a peer UUID in the selected subscribed room. @name in prose is only a label.
        #[arg(long, value_name = "PEER_UUID")]
        to: Option<uuid::Uuid>,
        #[arg(long)]
        socket: Option<PathBuf>,
        #[command(flatten)]
        room: RoomSelector,
        /// Message body. Omit it and pass `--stdin` to read the body from
        /// standard input instead — see `--stdin` for why you should.
        text: Option<String>,
        /// Read the message body from STDIN instead of the argument.
        ///
        /// Prose does not belong in a shell argument. Backticks inside a
        /// double-quoted string are command substitution: the shell runs them
        /// before airc ever sees the text. On 2026-09-05 that cost this grid
        /// three mangled messages (identifiers silently eaten mid-sentence —
        /// the QUIET failure, which corrupts the record with nobody noticing)
        /// and one CORE: a peer's post contained the words for a reboot in
        /// backticks, the shell executed them, and every citizen on that node
        /// went down. Three independent instances of one shell behaviour in a
        /// single night is a missing affordance, not operator error, so the
        /// fix belongs here rather than in everyone's quoting discipline.
        ///
        /// With `--stdin` the body never reaches the shell's parser, so the
        /// whole class — backticks, `$VAR`, `$(...)`, history expansion —
        /// stops existing instead of needing to be remembered:
        ///
        ///   airc msg --stdin <<'EOF'
        ///   anything at all, including `backticks` and $VARS
        ///   EOF
        #[arg(long, conflicts_with = "text")]
        stdin: bool,
    },

    /// Publish a structured frame and emit a JSON receipt on
    /// stdout. Designed for consumers (Continuum chat, OpenClaw,
    /// bridge processes) that need typed event id + lamport +
    /// channel without human-prose parsing, and to route to a
    /// non-default room without mutating this scope's default
    /// pointer.
    Publish {
        #[command(flatten)]
        room: RoomSelector,
        /// Inline UTF-8 body. Mutually exclusive with
        /// `--body-json`.
        #[arg(long, conflicts_with = "body_json", group = "body")]
        body_text: Option<String>,
        /// Path to a UTF-8 JSON file whose contents become the
        /// frame body. Pass `-` to read from stdin.
        #[arg(long, group = "body")]
        body_json: Option<String>,
        /// Read the message body from stdin instead of an argument,
        /// so prose never passes through shell quoting. Same flag,
        /// same meaning as `airc msg --stdin` — `--body-text` covers
        /// the current room only via `msg`, and every cross-room post
        /// was forced back to inline quoting without this.
        ///
        /// Measured 2026-09-05: a backtick in a `--body-text` argument
        /// was command-substituted by the shell and ate part of the
        /// message, in a room where the fix for exactly that hazard
        /// had already merged for `msg`. A flag that stops at one verb
        /// stops at the verb people happen to use least.
        #[arg(long, group = "body")]
        stdin: bool,
        /// Header in `key=value` form. Repeatable.
        #[arg(long = "header", value_name = "KEY=VALUE")]
        headers: Vec<String>,
        /// Required producer-declared semantic class; payloads are never inspected.
        #[arg(long, value_enum)]
        class: PublishClass,
    },

    /// Pull buffered frames from a subscribed room's wire. With
    /// `--room`, reads that room without mutating this scope's
    /// default-room pointer — the read sibling of `airc msg --room`.
    /// Without `--room`, reads the current room.
    Inbox {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[command(flatten)]
        room: RoomSelector,
        /// Cursor lamport — pair with `--since-event-id`. The cursor
        /// is `(lamport, event_id)`; both halves required when paging
        /// from a specific point.
        #[arg(long, requires = "since_event_id")]
        since_lamport: Option<u64>,
        /// Cursor event_id (UUID) — pair with `--since-lamport`.
        #[arg(long, requires = "since_lamport")]
        since_event_id: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// EVERY subscribed room, grouped, newest activity first — the
        /// one-room blindness fix (card a0772bba, 2026-09-06: seven hours
        /// of a peer's reports sat in a room the reader was not looking at,
        /// and the fleet's silent-means-down rule read a reporting node as
        /// down). `--limit` applies per room. Refused alongside `--room` or
        /// a cursor: one scope-wide view, not a paged one.
        #[arg(long, conflicts_with_all = ["room", "since_lamport", "since_event_id"])]
        all: bool,
        /// Emit a single JSON document on stdout instead of
        /// human-readable text. Shape mirrors `airc events list
        /// --json` and `airc publish` for machine consumers
        /// (continuum's CliAircRealtimeStore, shell scripts, CI
        /// smoke tests).
        #[arg(long)]
        json: bool,
    },

    /// Print or switch the current room. With no name, prints the
    /// current room's name + wire + channel. With a name, derives a
    /// deterministic `(wire, channel)` from the name and sets it as
    /// the current room — two peers who run `airc room project-x`
    /// land in the same channel without sharing the UUID. Verbs are never
    /// room names: `airc room list` prints the listing, and `join`, `leave`
    /// and similar refuse with the right form instead of creating a room.
    Room {
        /// Room name. Omit to just print the current room.
        name: Option<String>,
    },

    /// Publish the room's operating doctrine (card 2903a8ef slice 2/4).
    /// Reads a markdown file and emits a `RoomDoctrinePublished`
    /// substrate event so every attaching agent loads the latest
    /// doctrine on join. Default file is `AGENTS.md` at the git repo
    /// root; pass `--from-file` to override.
    DoctrinePublish {
        /// Path to the markdown file. Defaults to `AGENTS.md` at the
        /// git repo root if omitted.
        #[arg(long)]
        from_file: Option<std::path::PathBuf>,
    },

    /// Leave a subscribed room without deleting identity or trust.
    /// With no room, leaves the current default channel.
    Part {
        /// Optional channel name to leave.
        room: Option<String>,
    },

    /// Manage the persisted peer trust registry.
    Peer(PeerArgs),

    /// List enrolled peers in the current scope.
    ///
    /// IRC-shaped public command. Equivalent to `airc peer list`,
    /// kept as the low-friction human / agent coordination surface.
    Peers,

    /// Show the live mesh at a glance: which peers are live, on which
    /// channels, with what dialable endpoint — plus a loud warning when
    /// your default room has 0 other reachable peers.
    ///
    /// Read-only over the daemon's coordinator beacon store + peer
    /// trust store (no `gh` round-trip, no publish). Run `airc registry
    /// sync` first if you want the freshest cross-machine beacons.
    Network {
        /// List every stale beacon individually. Default collapses them
        /// to a one-line summary so the live set + convergence verdict
        /// stay the headline.
        #[arg(long, short)]
        all: bool,
    },

    /// Show identity information for self or an enrolled peer.
    ///
    /// With no target, prints this scope's identity card. With a peer
    /// id or prefix, prints the enrolled trust entry for that peer.
    Whois {
        /// Optional peer UUID or unambiguous UUID prefix.
        peer: Option<String>,
    },

    /// Inspect transport route policy and candidate selection.
    Route(RouteArgs),

    /// Account-mesh registry: same-account cross-machine discovery.
    ///
    /// The daemon runs publish/refresh on a cadence automatically; this
    /// verb is the manual proof + Mac-bootstrap surface — one
    /// publish+refresh against the gh-gist rendezvous, printing what was
    /// published and who was enrolled.
    Registry(crate::registry_cli::RegistryArgs),

    /// Inspect transport health and substrate connectivity.
    Transport(TransportArgs),

    /// Inspect persisted events through subscription-style filters.
    Events(crate::events_cli::EventsArgs),

    /// Parse legacy GitHub gist envelope JSON.
    Gist(GistArgs),

    /// Join the account mesh. With no room, subscribes to #general
    /// and the inferred repo/org channel. With a room, subscribes to
    /// that channel and makes it the default.
    ///
    /// Sets up the account mesh and, in interactive/agent runtimes,
    /// streams live events from ALL subscribed channels to stdout
    /// until interrupted. Scripts/tests return after setup; there is
    /// no separate public "attach" mode.
    Join {
        /// Optional channel name to join.
        #[arg(conflicts_with = "ensure")]
        room: Option<String>,
        /// Ensure the daemon is available without resuming an operator stop,
        /// changing rooms, or attaching a live feed. For unattended recovery.
        #[arg(long)]
        ensure: bool,
    },

    /// Out-of-band coordination channel of last resort.
    ///
    /// A GitHub-gist-comment thread that works whenever `gh` works —
    /// which is exactly when the airc wire is broken (blind rooms,
    /// stale peers, dead dials). Post with `send`, surface peers'
    /// posts with `watch`, and find/read the channel with `status`.
    /// Every post is prefixed with this node's machine label so a
    /// human can tell machines apart at a glance; `watch` self-filters
    /// this node's own posts.
    Sos {
        #[command(subcommand)]
        action: SosAction,
    },

    /// Print the installed `airc` build metadata: short commit, branch,
    /// commit subject, and install dir. Use this to verify two scopes
    /// are on the same build. (`--version` flag prints just the
    /// package version.)
    Version,

    /// Validate existing stores without mutation before adopting this binary.
    #[command(hide = true)]
    StorePublishFresh {
        #[arg(long)]
        destination: PathBuf,
    },
    #[command(hide = true)]
    StorePreflight {
        /// Refuse any existing store: direct installation cannot quiesce its writer.
        #[arg(long)]
        fresh_install: bool,
    },

    /// Print the resolved daemon IPC socket path for this scope and exit.
    ///
    /// The canonical answer to "where does my daemon bind?". Consumers
    /// (Continuum's airc discovery) call this to locate the socket
    /// without re-deriving airc's path convention or hand-setting an env
    /// var — airc owns the path, callers ask for it. Resolves the path
    /// only; does NOT require the daemon to be running (callers probe
    /// liveness separately via `status`/`ping`).
    #[cfg(windows)]
    #[command(hide = true)]
    SetupRecoverElevatedOwner {
        #[arg(long)]
        probe: bool,
        #[arg(long)]
        endpoint: PathBuf,
        #[arg(long)]
        caller_sid: String,
        #[arg(long)]
        installed_binary: PathBuf,
    },

    IpcEndpoint {
        /// Print the OS transport endpoint (Windows named pipe; Unix socket).
        #[arg(long)]
        native: bool,
    },

    /// Install a verified published build from the configured release channel
    /// and reconcile the daemon through its guarded maintenance handoff.
    #[command(visible_aliases = ["upgrade", "pull"])]
    Update {
        /// Self-update with a smoke-test and rollback: back up the live
        /// binary, verify the published binary runs + reports the
        /// selected SHA, and roll back to the backup if it doesn't. Safe to
        /// run unattended (e.g. when a peer detects it's stale).
        #[arg(long)]
        auto: bool,
        /// Install this exact full commit SHA from the configured channel.
        /// Its published platform artifact must pass the normal verification.
        #[arg(long, conflicts_with = "adopt_installed")]
        revision: Option<String>,
        /// Start and verify an already-installed binary, or replace a stale
        /// daemon through the maintenance handoff, without rebuilding.
        #[arg(long, conflicts_with = "auto")]
        adopt_installed: bool,
    },

    /// Self-diagnose the airc install + scope state.
    ///
    /// Walks the install/identity/daemon/route checklist that
    /// `skills/doctor/SKILL.md` documents agents calling. Default
    /// mode is the env probe (fast, local). `--health` adds live
    /// route/process state. `--fix` applies only safe auto-recovery
    /// for detected issues (currently stale daemon sockets).
    Doctor {
        /// After diagnosing, apply safe auto-recovery. Identity
        /// partial states are reported with manual fix commands;
        /// doctor does not wipe identity/trust state automatically.
        /// Without `--fix`, doctor only reports.
        #[arg(long)]
        fix: bool,

        /// Include live route/process health (calls into the
        /// route resolver + daemon status).
        #[arg(long)]
        health: bool,
    },

    /// Shared GitHub request governor.
    Gh(GhArgs),

    /// TCP pairing handshake during Rust cutover.
    Handshake(HandshakeArgs),

    /// Workspace/resource hygiene policy.
    Hygiene(HygieneArgs),

    /// Knock/approve crypto helpers during Rust cutover.
    Knock(KnockArgs),

    /// Pending-queue routing helpers during Rust cutover.
    Pending(PendingArgs),

    /// Codex lifecycle hook adapters backed by Rust AIRC events.
    CodexHook(CodexHookArgs),

    /// Claude Code lifecycle hook adapters backed by Rust AIRC events.
    ClaudeHook(crate::integrations::claude::ClaudeHookArgs),

    /// Launch the runtime feed owner for Codex integration.
    CodexStart(CodexStartArgs),

    /// Coordinate work cards over the current room's AIRC substrate.
    Work(WorkArgs),

    /// Coordinate work lanes over the current room's AIRC substrate.
    Lane(crate::lane_cli::LaneArgs),

    /// Manage local git worktree lane registry.
    WorktreeLane(crate::worktree_lane_cli::WorktreeLaneArgs),

    /// Queue-card parsing and mutation primitives during Rust cutover.
    QueueCard(crate::queue_card_cli::QueueCardArgs),

    /// Save and deliver per-agent resume context through existing runtimes.
    AgentResume(crate::agent_resume::ResumeArgs),

    /// Format monitor events for AI/runtime consumers.
    Monitor(crate::monitor::MonitorArgs),

    /// Coordinate workspace leases over the current room's AIRC substrate.
    Workspace(crate::workspace_cli::WorkspaceArgs),

    /// Print the stable mnemonic for a hex digest.
    Humanhash {
        /// Hex input to convert into a mnemonic.
        hex_input: String,
        /// Number of words to emit.
        #[arg(long, default_value_t = 4)]
        words: usize,
    },

    /// Print this runtime process's client id, if one can be derived.
    ClientId,

    /// Generate a UUIDv4.
    UuidV4,

    /// Convert a canonical UTC timestamp to Unix epoch seconds.
    IsoToEpoch {
        /// Timestamp in `YYYY-MM-DDTHH:MM:SSZ` form.
        timestamp: String,
    },
}

/// Subcommands for `airc sos` — the out-of-band gist-comment channel.
#[derive(Debug, Subcommand)]
pub enum SosAction {
    /// Post a message to the account's SOS gist, prefixed with this
    /// node's machine label (e.g. `[BIGMAMA] daemon wedged`). Finds or
    /// creates the SOS gist first.
    Send {
        /// The message body to post.
        message: String,
    },

    /// Surface new PEER messages from the SOS gist (self-filtering this
    /// node's own posts). Default (agent mode) prints any new peer
    /// message(s) and exits so a harness can re-invoke; `--follow`
    /// streams continuously for a human.
    Watch {
        /// Stream continuously instead of printing new messages once and
        /// exiting.
        #[arg(long)]
        follow: bool,
    },

    /// Print the SOS gist id, its html url, and the last few comments so
    /// a peer can find and read the channel.
    Status,
}

#[cfg(test)]
mod tests {
    #[test]
    fn msg_recipient_is_explicit_uuid_not_body_text() {
        use super::{Cli, Command};
        use clap::Parser;
        let id = "9bb24964-1a1a-43e2-a5aa-8140362bab63";
        let parsed = Cli::try_parse_from(["airc", "msg", "--room", "other", "--to", id, "hello"])
            .expect("typed recipient");
        assert!(
            matches!(parsed.command, Command::Msg { to: Some(peer), .. } if peer.to_string() == id)
        );
        let broadcast = Cli::try_parse_from(["airc", "msg", "@Cormac hello"]).expect("body label");
        assert!(matches!(broadcast.command, Command::Msg { to: None, .. }));
        assert!(Cli::try_parse_from(["airc", "msg", "--to", "Cormac", "hello"]).is_err());
    }
    #[test]
    fn home_and_here_conflict_at_parse_time() {
        use super::Cli;
        use clap::Parser;

        // Both flags → parse error (clap exit code 2 class).
        assert!(
            Cli::try_parse_from(["airc", "--home", "/x", "--here", "init"]).is_err(),
            "--home and --here together must be rejected, not silently merged"
        );
        // Each alone parses fine (guard isn't over-broad).
        assert!(Cli::try_parse_from(["airc", "--here", "init"]).is_ok());
        assert!(Cli::try_parse_from(["airc", "--home", "/x", "init"]).is_ok());
    }

    #[test]
    fn unattended_join_cannot_change_rooms() {
        use super::{Cli, Command};
        use clap::Parser;
        let parsed = Cli::try_parse_from(["airc", "join", "--ensure"]).unwrap();
        assert!(matches!(
            parsed.command,
            Command::Join {
                room: None,
                ensure: true
            }
        ));
        assert!(Cli::try_parse_from(["airc", "join", "--ensure", "another-room"]).is_err());
    }

    // what this catches: the `ipc-endpoint` subcommand silently vanishing
    // or being renamed. Continuum's airc discovery shells out to exactly
    // `airc ipc-endpoint` to locate the daemon socket; when this command
    // was never shipped, discovery returned Unreachable and personas never
    // spawned. This pins the kebab-case spelling to the contract.
    #[test]
    fn ipc_endpoint_subcommand_parses() {
        use super::{Cli, Command};
        use clap::Parser;

        let parsed = Cli::try_parse_from(["airc", "ipc-endpoint"])
            .expect("`airc ipc-endpoint` must parse — Continuum discovery depends on it");
        assert!(
            matches!(parsed.command, Command::IpcEndpoint { native: false }),
            "ipc-endpoint must map to Command::IpcEndpoint, got {:?}",
            parsed.command
        );
        let native = Cli::try_parse_from(["airc", "ipc-endpoint", "--native"])
            .expect("native endpoint diagnostics must parse without a daemon");
        assert!(matches!(
            native.command,
            Command::IpcEndpoint { native: true }
        ));
    }

    // what this catches (self-healing join): the `airc dial HOST:PORT`
    // recovery verb — positional endpoint, optional --peer pin, bounded
    // --timeout-ms. The runbook for a wedged mesh says exactly
    // `airc dial <host:port>`; a silent rename/removal strands the
    // operator mid-recovery.
    #[test]
    fn dial_verb_parses_endpoint_and_optional_peer() {
        use super::{Cli, Command};
        use clap::Parser;

        let parsed = Cli::try_parse_from(["airc", "dial", "192.168.1.249:57958"])
            .expect("`airc dial HOST:PORT` must parse");
        match parsed.command {
            Command::Dial {
                to,
                peer,
                timeout_ms,
            } => {
                assert_eq!(
                    to,
                    "192.168.1.249:57958"
                        .parse::<std::net::SocketAddr>()
                        .unwrap()
                );
                assert_eq!(peer, None, "expected-peer inference is the default");
                assert_eq!(timeout_ms, 10_000, "generous recovery-verb default");
            }
            other => panic!("dial must map to Command::Dial, got {other:?}"),
        }
        assert!(
            Cli::try_parse_from(["airc", "dial", "not-an-endpoint"]).is_err(),
            "a non-socket-addr endpoint must be a parse error"
        );
    }
}

/// Explicit semantic class for `airc publish`; transport kind is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum PublishClass {
    Message,
    Announcement,
    Intent,
    WorkReceipt,
    WorkState,
    Presence,
    Session,
    Lifecycle,
}

impl From<PublishClass> for airc_core::EventClass {
    fn from(value: PublishClass) -> Self {
        use airc_core::{ActivityKind, ControlKind, CorrespondenceKind, EventClass};
        match value {
            PublishClass::Message => EventClass::Correspondence(CorrespondenceKind::Message),
            PublishClass::Announcement => {
                EventClass::Correspondence(CorrespondenceKind::Announcement)
            }
            PublishClass::Intent => EventClass::Activity(ActivityKind::Intent),
            PublishClass::WorkReceipt => EventClass::Activity(ActivityKind::WorkReceipt),
            PublishClass::WorkState => EventClass::Activity(ActivityKind::WorkState),
            PublishClass::Presence => EventClass::Activity(ActivityKind::Presence),
            PublishClass::Session => EventClass::Control(ControlKind::Session),
            PublishClass::Lifecycle => EventClass::Control(ControlKind::Lifecycle),
        }
    }
}
