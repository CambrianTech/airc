use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct IdentityArgs {
    #[command(subcommand)]
    pub action: IdentityAction,
}

#[derive(Debug, Subcommand)]
pub enum IdentityAction {
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
