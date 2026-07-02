use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct IdentityArgs {
    #[command(subcommand)]
    pub action: IdentityAction,
}

#[derive(Debug, Subcommand)]
pub enum IdentityAction {
    /// Generate the legacy X25519 keypair if missing and print the pubkey.
    Bootstrap {
        /// Legacy identity directory containing x25519_priv/x25519_pub.
        #[arg(long = "dir")]
        identity_dir: std::path::PathBuf,
    },

    /// Generate the legacy Ed25519 PEM keypair if missing.
    BootstrapEd25519 {
        /// Legacy identity directory containing private.pem/public.pem.
        #[arg(long = "dir")]
        identity_dir: std::path::PathBuf,
    },

    /// Print an enrolled peer's legacy X25519 public key.
    PeerPub {
        /// Legacy peers directory containing <peer>.json files.
        #[arg(long)]
        peers_dir: std::path::PathBuf,
        /// Peer display/name key.
        #[arg(long)]
        peer_name: String,
    },

    /// Print an enrolled peer's SSH public key from its peer record.
    PeerSshPub {
        /// Legacy peers directory containing <peer>.json files.
        #[arg(long)]
        peers_dir: std::path::PathBuf,
        /// Peer display/name key.
        #[arg(long)]
        peer_name: String,
    },

    /// Sign stdin bytes with the legacy Ed25519 PEM identity.
    SignEd25519 {
        /// Legacy identity directory containing private.pem.
        #[arg(long = "dir")]
        identity_dir: std::path::PathBuf,
    },

    /// Pretty-print an identity JSON blob for whois output.
    Pretty {
        /// Display name to show.
        #[arg(long)]
        name: String,
        /// Identity JSON blob.
        #[arg(long, default_value = "{}")]
        identity_json: String,
        /// Optional host address.
        #[arg(long, default_value = "")]
        host: String,
    },

    /// Write a legacy peer record learned from the join handshake.
    WritePeerRecord {
        /// Legacy peers directory containing <peer>.json files.
        #[arg(long)]
        peers_dir: std::path::PathBuf,
        /// Peer display/name key.
        #[arg(long)]
        peer_name: String,
        /// SSH target for the peer.
        #[arg(long)]
        host: String,
        /// Peer airc home path.
        #[arg(long, default_value = "")]
        airc_home: String,
        /// Optional X25519 public key for envelope encryption.
        #[arg(long, default_value = "")]
        x25519_pub: String,
        /// Pair timestamp.
        #[arg(long)]
        paired: String,
    },

    /// Print the session state path for a transport identity.
    SessionFile {
        #[arg(long)]
        write_dir: PathBuf,
        #[arg(long, default_value = "anonymous")]
        transport_name: String,
    },

    /// Print the default work identity for a transport + session.
    DefaultWorkName {
        #[arg(long, default_value = "anonymous")]
        transport_name: String,
        #[arg(long)]
        session_file: PathBuf,
    },

    /// Read the saved work identity from a session file.
    ReadWorkName {
        #[arg(long)]
        session_file: PathBuf,
    },

    /// Write the saved work identity for a session.
    WriteWorkSession {
        #[arg(long)]
        session_file: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "anonymous")]
        transport_name: String,
    },

    /// Print local identity from the ORM store.
    Show,

    /// Update local identity fields in the ORM store.
    Set {
        /// Display name peers see in the roster (resolves `display_name` /
        /// `peer_alias`). Set this on a long-lived identity that was
        /// created before the agent_name nick-seed and shows a blank name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        pronouns: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        bio: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },

    /// Link or unlink a platform handle in the ORM store.
    Link {
        #[arg(long)]
        platform: String,
        #[arg(long, default_value = "")]
        handle: String,
    },

    /// Exit 2 when local identity should show the first-run nudge.
    NudgeNeeded,

    /// Merge a continuum persona JSON blob into the ORM identity store.
    ImportContinuum {
        #[arg(long)]
        blob: String,
    },

    /// Print linked continuum handle from the ORM identity store.
    ContinuumHandle,

    /// Push local identity fields to a linked continuum persona.
    PushContinuum {
        #[arg(long)]
        handle: String,
    },
}
