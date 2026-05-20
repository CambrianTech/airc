use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct KnockArgs {
    #[command(subcommand)]
    pub action: KnockAction,
}

#[derive(Debug, Subcommand)]
pub enum KnockAction {
    /// Emit a fresh X25519 keypair as JSON.
    GenKeys,
    /// Encrypt plaintext to a knocker's ephemeral public key.
    EncryptForKnocker {
        #[arg(long)]
        knocker_pub: String,
        #[arg(long)]
        plaintext: String,
    },
    /// Decrypt approver-posted ciphertext with the knocker's private key.
    DecryptFromApprover {
        #[arg(long)]
        knocker_priv: String,
        #[arg(long)]
        approver_pub: String,
        #[arg(long)]
        nonce: String,
        #[arg(long)]
        ciphertext: String,
    },
    /// Read one string field from an approval JSON envelope on stdin.
    ApprovalField {
        #[arg(long)]
        field: String,
    },
    /// Extract knocker_pub from a knock issue markdown body on stdin.
    ExtractKnockerPub,
    /// Extract the latest approval JSON envelope from gh comments JSON on stdin.
    ExtractApproval,
}
