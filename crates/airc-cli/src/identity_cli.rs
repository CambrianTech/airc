use clap::{Args, Subcommand};

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
}
