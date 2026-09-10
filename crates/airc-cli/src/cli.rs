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
use std::path::{Path, PathBuf};

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

/// Default home directory for persisted identity + IPC state.
///
/// Resolution order:
///   1. `$AIRC_HOME` → explicit scope override.
///   2. First `.airc` ancestor when cwd is inside a scope.
///   3. Git project root `.airc` when cwd is inside a worktree.
///   4. Canonical machine-account home (`$HOME/.airc`) — a rootless cwd
///      uses the user's real identity, never a throwaway `./.airc`
///      scratch scope (seam #1). `./.airc` only as a last resort when no
///      `$HOME`/`$USERPROFILE` exists.
///
/// Account-wide state still lives under the canonical machine account
/// home (`$HOME/.airc`) inside `airc-lib`; this default is the
/// consumer/project scope. That preserves the original public contract:
/// running `airc join` in a repo uses that repo's `.airc`.
pub fn default_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("AIRC_HOME") {
        return PathBuf::from(home);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    default_home_dir_for(&cwd)
}

/// Resolve the explicit home override from the global `--home` / `--here`
/// flags. `--home <path>` wins (and clap forbids passing both). `--here`
/// is the ergonomic shortcut for `AIRC_HOME=$PWD/.airc` (seam #1): the
/// intentional opt-in to a cwd-LOCAL scope. Returns `None` when neither
/// is set — the caller then falls back to [`default_home_dir`] (which
/// itself honours `$AIRC_HOME`, then the machine-account / git-project
/// resolution). Pure: `cwd` is injected so tests need no real `$PWD`.
pub fn explicit_home_override(
    home_flag: Option<PathBuf>,
    here_flag: bool,
    cwd: &Path,
) -> Option<PathBuf> {
    if let Some(home) = home_flag {
        return Some(home);
    }
    if here_flag {
        return Some(cwd.join(".airc"));
    }
    None
}

fn default_home_dir_for(cwd: &Path) -> PathBuf {
    let machine_account = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".airc"));
    default_home_dir_for_with(cwd, machine_account.as_deref(), &git_main_working_tree)
}

/// Card a1b4552a — pure-function half of the resolution so tests can
/// drive it with synthetic paths instead of needing a real `$HOME`
/// and git repo. `machine_account_home` is the path we MUST NOT
/// resolve to (typically `$HOME/.airc`); `git_main_working_tree_fn`
/// resolves the main working tree of a git checkout from any of its
/// worktrees (production: shells `git rev-parse --git-common-dir`).
fn default_home_dir_for_with(
    cwd: &Path,
    machine_account_home: Option<&Path>,
    git_main_working_tree_fn: &dyn Fn(&Path) -> Option<PathBuf>,
) -> PathBuf {
    for ancestor in cwd.ancestors() {
        let matches_dotairc = ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".airc");
        if !matches_dotairc {
            continue;
        }
        // Card a1b4552a: SKIP the machine-account home itself when
        // walking ancestors. `~/.airc` is a SYSTEM-LEVEL airc state
        // directory holding the singleton machine-account identity +
        // the daemon socket — not a per-project agent scope. Agents
        // who `cd ~/.airc/worktrees/<short>/` (the d1b2798d auto-spawn
        // workflow) would otherwise resolve home here and silently
        // borrow the machine-account's identity, attributing their
        // actions to whoever owns the singleton row. Reproducibly
        // observed today: peer 9bb24964 claiming cards from a
        // spawned worktree → board shows the claims under peer
        // cdff6a9d (machine-account-owner) instead.
        if Some(ancestor) == machine_account_home {
            continue;
        }
        return ancestor.to_path_buf();
    }
    // Card a1b4552a: when cwd is inside a git worktree spawned by
    // `airc work claim` (worktrees live under `~/.airc/worktrees/`),
    // the canonical project scope is the MAIN repo's `.airc`, not
    // the worktree's own (worktrees rarely contain their own `.airc`).
    // `git rev-parse --git-common-dir` points at the MAIN repo's
    // `.git/` for any worktree; its parent is the main working tree.
    // Seam #1 (solidification doc): a cwd that is neither inside a git
    // project NOR under an existing `.airc` scope must NOT mint a
    // throwaway `cwd/.airc` scope. That scratch home gets its own
    // identity, which then publishes a phantom beacon to the mesh — the
    // observed `/tmp` `57059a56` ghost. Fall back to the canonical
    // MACHINE-ACCOUNT home (the user's real identity) instead. Explicit
    // cwd-scoping stays available via `$AIRC_HOME` (a future `--here`
    // flag is the ergonomic shortcut; tracked as a follow-up). Only the
    // truly-rootless case (no `$HOME`/`$USERPROFILE`) keeps the old
    // cwd-local behavior as a last resort.
    git_main_working_tree_fn(cwd)
        .map(|root| root.join(".airc"))
        .or_else(|| git_toplevel(cwd).map(|root| root.join(".airc")))
        .or_else(|| machine_account_home.map(|h| h.to_path_buf()))
        .unwrap_or_else(|| cwd.join(".airc"))
}

fn git_main_working_tree(cwd: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let common_dir = text.trim();
    if common_dir.is_empty() {
        return None;
    }
    let common_path = PathBuf::from(common_dir);
    let abs = if common_path.is_absolute() {
        common_path
    } else {
        cwd.join(common_path).canonicalize().ok()?
    };
    // common dir is `<main-working-tree>/.git`; parent is the main
    // working tree.
    abs.parent().map(|p| p.to_path_buf())
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let root = text.trim();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

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

    /// Daemon health snapshot.
    Status {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Ask the daemon to shut down gracefully.
    Stop {
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
        /// Frame kind. Defaults to `event` for structured payloads.
        #[arg(long, value_enum, default_value = "event")]
        kind: PublishFrameKind,
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
    /// land in the same channel without sharing the UUID.
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
        room: Option<String>,
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

    /// Print the resolved daemon IPC socket path for this scope and exit.
    ///
    /// The canonical answer to "where does my daemon bind?". Consumers
    /// (Continuum's airc discovery) call this to locate the socket
    /// without re-deriving airc's path convention or hand-setting an env
    /// var — airc owns the path, callers ask for it. Resolves the path
    /// only; does NOT require the daemon to be running (callers probe
    /// liveness separately via `status`/`ping`).
    IpcEndpoint,

    /// Fast-forward the installed source checkout and refresh the
    /// installed `airc` binary + skills from that source.
    #[command(visible_aliases = ["upgrade", "pull"])]
    Update {
        /// Self-update with a smoke-test and rollback: back up the live
        /// binary, rebuild, verify the new binary runs + reports the
        /// pulled SHA, and roll back to the backup if it doesn't. Safe to
        /// run unattended (e.g. when a peer detects it's stale).
        #[arg(long)]
        auto: bool,
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
    use super::{default_home_dir_for, explicit_home_override};
    use std::path::{Path, PathBuf};

    /// what this catches (seam #1 `--here`): the override precedence.
    /// `--home` wins; `--here` resolves to the cwd-LOCAL `.airc` (the
    /// `AIRC_HOME=$PWD/.airc` shortcut); neither set returns None so the
    /// caller falls through to the machine-account / git-project default.
    /// If `--here` ever resolved to the machine-account home instead of
    /// `$PWD/.airc`, the whole point of the flag (an explicit local
    /// scope) would be lost — this pins it.
    #[test]
    fn explicit_home_override_precedence() {
        let cwd = Path::new("/work/project");

        // --home wins outright.
        assert_eq!(
            explicit_home_override(Some(PathBuf::from("/custom/home")), false, cwd),
            Some(PathBuf::from("/custom/home"))
        );
        // (and still wins even if --here were somehow also set; clap
        // forbids the combination, but the resolver is unambiguous.)
        assert_eq!(
            explicit_home_override(Some(PathBuf::from("/custom/home")), true, cwd),
            Some(PathBuf::from("/custom/home"))
        );
        // --here → cwd-local .airc, NOT the machine-account home.
        assert_eq!(
            explicit_home_override(None, true, cwd),
            Some(PathBuf::from("/work/project/.airc"))
        );
        // neither → None (caller falls back to default_home_dir).
        assert_eq!(explicit_home_override(None, false, cwd), None);
    }

    /// what this catches: `--home` and `--here` are mutually exclusive at
    /// the CLAP layer (`conflicts_with = "home"`). The resolver test above
    /// proves the value precedence, but not the parse-time guard — a
    /// future arg-id rename could silently drop the `conflicts_with` and
    /// let both be passed, with the resolver then quietly preferring
    /// `--home` while the user thinks `--here` took effect. This pins the
    /// guard: passing both must be a hard parse error.
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
            matches!(parsed.command, Command::IpcEndpoint),
            "ipc-endpoint must map to Command::IpcEndpoint, got {:?}",
            parsed.command
        );
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

    #[test]
    fn default_home_uses_enclosing_airc_scope() {
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(default_home_dir_for(&nested), scope);
    }

    #[test]
    fn default_home_uses_git_project_root_scope() {
        // Hermetic: STUB the git-main-working-tree resolver instead of
        // shelling out to real `git` in a temp repo. The earlier version
        // called the real `default_home_dir_for` (which shells `git
        // rev-parse`), and that is environment-fragile under parallel
        // unit tests + CI runners: git's dubious-ownership / safe.directory
        // check can intermittently REFUSE a freshly-init'd temp repo (and
        // a sibling test mutating the process-global `$HOME`/cwd can race
        // git's config read), so `git_main_working_tree` returns None and
        // `default_home_dir_for` falls back to the machine-account home —
        // a non-deterministic red that blocked unrelated PRs (the #1230
        // diagnostic assertion is what surfaced exactly this fallback).
        //
        // The mapping logic under test — "a resolved git main working
        // tree ⇒ <tree>/.airc, NOT the machine-account home" — is what
        // this pins, deterministically, via the same `..._with` stub seam
        // the sibling default_home tests use. The git-shell integration
        // itself is exercised in production + the daemon integration
        // tests, not in a flaky parallel unit test.
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let repo = PathBuf::from("/Users/test/Development/myproj");
        let nested = repo.join("src").join("inner");
        let machine_account = PathBuf::from("/Users/test/.airc");
        // No `.airc` ancestor of `nested`; the resolver must fall through
        // to the git main working tree (stubbed) and scope to <repo>/.airc.
        let stub = |_: &Path| -> Option<PathBuf> { Some(repo.clone()) };
        let resolved = default_home_dir_for_with(&nested, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved,
            repo.join(".airc"),
            "a resolved git main working tree must scope home to <tree>/.airc, \
             not fall back to the machine-account home"
        );
    }

    #[test]
    fn default_home_skips_machine_account_home_when_inside_worktrees_subdir() {
        // Card a1b4552a — the leak we caught live today. Without this
        // guard an agent who `cd ~/.airc/worktrees/<short>/` per the
        // d1b2798d auto-spawn workflow would resolve home to ~/.airc
        // and borrow the machine-account identity. Test pins that
        // ancestor walk SKIPS the machine-account ~/.airc and falls
        // through to git_main_working_tree (the main repo's .airc).
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let machine_account = PathBuf::from("/Users/test/.airc");
        let cwd = machine_account.join("worktrees").join("abc12345");
        // Stub git_main_working_tree_fn to a known main repo working tree.
        let main_repo = PathBuf::from("/Users/test/Development/airc");
        let stub = |_: &Path| -> Option<PathBuf> { Some(main_repo.clone()) };
        let resolved = default_home_dir_for_with(&cwd, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved,
            main_repo.join(".airc"),
            "must NOT resolve to machine-account ~/.airc when cwd is under its worktrees dir",
        );
    }

    #[test]
    fn default_home_still_resolves_project_scope_when_not_under_machine_account() {
        // Sanity: a nested cwd that has an enclosing project .airc that
        // is NOT the machine-account home resolves correctly (regression
        // guard — we don't want the fix above to skip legitimate
        // project scopes too).
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let machine_account = PathBuf::from("/Users/test/.airc");
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&nested, Some(machine_account.as_path()), &stub);
        assert_eq!(resolved, scope);
    }

    #[test]
    fn default_home_falls_through_to_cwd_airc_when_nothing_else_resolves() {
        // Final fallback: no ancestor .airc, no git context — cwd's
        // own .airc is the answer. Pre-existing behaviour, pinned so
        // the refactor preserves it.
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let root = tempfile::TempDir::new().unwrap();
        let cwd = root.path().join("standalone");
        std::fs::create_dir_all(&cwd).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&cwd, None, &stub);
        assert_eq!(resolved, cwd.join(".airc"));
    }

    // what this catches: seam #1 — a rootless cwd (no enclosing `.airc`,
    // no git context) with a known machine-account home must resolve to
    // that canonical home, NOT mint a throwaway `cwd/.airc` scratch scope
    // (the `/tmp` `57059a56` phantom-beacon ghost). Mutation check:
    // reverting the machine-account fallback resolves to cwd/.airc and
    // this fails.
    #[test]
    fn default_home_rootless_cwd_uses_machine_account_not_throwaway() {
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let root = tempfile::TempDir::new().unwrap();
        let cwd = root.path().join("nowhere"); // no .airc, no git
        std::fs::create_dir_all(&cwd).unwrap();
        let machine_account = PathBuf::from("/Users/test/.airc");
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&cwd, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved, machine_account,
            "rootless cwd must use the canonical machine-account home, not a throwaway cwd/.airc"
        );
        assert_ne!(resolved, cwd.join(".airc"), "must NOT mint a scratch scope");
    }
}

#[derive(Debug, Args)]
pub struct PeerArgs {
    #[command(subcommand)]
    pub action: PeerAction,
}

/// clap value_parser shim for `--endpoint` — clap wants a
/// `fn(&str) -> Result<T, E>` and the typed parser lives with the
/// enum in airc-lib.
fn parse_cli_route_endpoint(input: &str) -> Result<airc_lib::RouteEndpoint, String> {
    airc_lib::RouteEndpoint::parse_cli(input)
}

#[derive(Debug, Subcommand)]
pub enum PeerAction {
    /// Enrol a peer by spec. If a daemon is running on
    /// the scope's default IPC endpoint, also tells it via RPC so the
    /// in-memory registry stays in sync — no daemon restart required.
    Add {
        /// Peer spec: `<uuid>:<base64-pubkey-no-padding>` (the
        /// `peer_spec:` line from the other side's `airc init`).
        spec: PeerSpec,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Card 34942ec1 Sub-C: enrol at this trust tier instead of
        /// the substrate-default Untrusted. Used to manually pin
        /// Friend / OwnAccount / OwnMachine when the operator knows
        /// the relationship out-of-band (Joel pinning Friend on
        /// Toby's airc, OwnAccount on his other machine, etc.).
        #[arg(long, value_enum)]
        tier: Option<CliTrustTier>,
        /// Card 625abe6d slice 1 (DEV verb — production endpoints
        /// arrive via the account registry / mDNS): advertise where
        /// this peer can be dialed. Repeatable; stored order = dial
        /// cost order. Forms: `lan-tcp:HOST:PORT`,
        /// `tailscale-tcp:HOST:PORT`, `udp:HOST:PORT`, `relay:URL`.
        /// Route discovery (`airc transport health`, daemon refresh)
        /// dials these outbound — the peer never needs an inbound
        /// rule on OUR side.
        #[arg(long = "endpoint", value_parser = parse_cli_route_endpoint)]
        endpoints: Vec<airc_lib::RouteEndpoint>,
    },
    /// Remove a peer from local trust.
    Remove {
        /// Peer UUID to remove from the trust store.
        peer_id: String,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Card 34942ec1 Sub-C: update the trust tier of an
    /// already-enrolled peer without rotating the key. Pubkey-
    /// rotation has its own path (`peer add` with a re-pair flow);
    /// this is the orthogonal tier-update.
    ///
    /// Refuses for unknown peers (no implicit add). Idempotent for
    /// no-op transitions.
    SetTier {
        /// Peer UUID to re-tier.
        peer_id: String,
        /// New trust tier.
        #[arg(value_enum)]
        tier: CliTrustTier,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List enrolled peers from the peer trust store.
    List {
        /// Print as JSON ({peer_id, pubkey_b64, tier, added_at_ms}
        /// per row). Consumers (continuum bridge, hermes router)
        /// read this to build their grid routing tables — see card
        /// 34942ec1 Sub-C V4.
        #[arg(long)]
        json: bool,
    },
    /// Evict DEAD trust-store enrolments — peers that are `untrusted`
    /// AND absent from the current fresh account registry (e.g. the
    /// `172.18.0.x` Docker-container ghosts that leak failed dials). The
    /// peer-store analog of `registry gc`.
    ///
    /// NEVER touches trusted peers (a cross-grid Friend publishes to
    /// THEIR account, so is absent from yours) or live peers. Dry-run by
    /// default — prints the plan; pass `--apply` to evict. If a fresh
    /// live set can't be established (gh unauth / unreachable / empty
    /// registry), it prunes nothing rather than risk a live peer.
    Prune {
        /// Actually evict the dead enrolments. Without this flag, prune
        /// only prints what it WOULD evict (dry run).
        #[arg(long)]
        apply: bool,
        /// Staleness grace window, in hours: an untrusted peer absent
        /// from the fresh registry is evicted only once its last_seen is
        /// older than this. Recently-contacted peers inside the window
        /// are kept (a momentary registry-snapshot lag is not death).
        /// Omit for the 1-hour default; `0` evicts every absent untrusted
        /// peer immediately (no grace).
        #[arg(long)]
        stale_after_hours: Option<u64>,
    },
}

/// CLI mirror of `airc_store::TrustTier`. Kept distinct so clap's
/// value_enum machinery can derive the snake-case rename without
/// pulling clap into the storage layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CliTrustTier {
    OwnMachine,
    OwnAccount,
    Friend,
    Untrusted,
}

impl From<CliTrustTier> for airc_store::TrustTier {
    fn from(value: CliTrustTier) -> Self {
        match value {
            CliTrustTier::OwnMachine => airc_store::TrustTier::OwnMachine,
            CliTrustTier::OwnAccount => airc_store::TrustTier::OwnAccount,
            CliTrustTier::Friend => airc_store::TrustTier::Friend,
            CliTrustTier::Untrusted => airc_store::TrustTier::Untrusted,
        }
    }
}

/// Frame kind selector for `airc publish`. Maps 1:1 onto
/// `airc_protocol::FrameKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum PublishFrameKind {
    /// Plain message frame (human-readable chat).
    Message,
    /// Structured event frame (recommended for typed envelopes
    /// like Continuum's `AircRealtimeEnvelope`).
    Event,
    /// Control-plane signalling.
    Control,
}
