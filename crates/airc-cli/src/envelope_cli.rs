use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct EnvelopeArgs {
    #[command(subcommand)]
    pub action: EnvelopeAction,
}

#[derive(Debug, Subcommand)]
pub enum EnvelopeAction {
    /// Encrypt the legacy envelope `msg` field for a recipient.
    Wrap {
        /// Recipient X25519 public key, URL-safe base64 without padding.
        #[arg(long, default_value = "")]
        recipient_pub: String,
        /// Legacy identity directory containing x25519_priv.
        #[arg(long)]
        identity_dir: std::path::PathBuf,
    },

    /// Decrypt the legacy envelope `msg` field from a sender.
    Unwrap {
        /// Sender X25519 public key, URL-safe base64 without padding.
        #[arg(long, default_value = "")]
        sender_pub: String,
        /// Legacy identity directory containing x25519_priv.
        #[arg(long)]
        identity_dir: std::path::PathBuf,
    },
}
