//! `airc peer ...`: the peer trust subcommand's arguments.

use std::path::PathBuf;

use airc_lib::PeerSpec;
use clap::{Args, Subcommand, ValueEnum};

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
