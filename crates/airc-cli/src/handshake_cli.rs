use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct HandshakeArgs {
    #[command(subcommand)]
    pub action: HandshakeAction,
}

#[derive(Debug, Subcommand)]
pub enum HandshakeAction {
    /// Joiner-side TCP pair request.
    Send {
        host: String,
        port: u16,
        #[arg(long, default_value = "")]
        my_name: String,
        #[arg(long, default_value = "")]
        my_host: String,
        #[arg(long, default_value = "")]
        my_ssh_pub: String,
        #[arg(long, default_value = "")]
        my_sign_pub: String,
        #[arg(long, default_value = "")]
        my_x25519_pub: String,
        #[arg(long, default_value = "")]
        my_airc_home: String,
        #[arg(long, default_value = "{}")]
        my_identity_json: String,
    },

    /// Host-side TCP listener; accepts and records one peer.
    AcceptOne {
        #[arg(long, default_value_t = 7547)]
        host_port: u16,
        #[arg(long)]
        peers_dir: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        host_name: String,
        #[arg(long, default_value_t = 300)]
        reminder_interval: u64,
        #[arg(long)]
        airc_home: PathBuf,
        #[arg(long)]
        messages: PathBuf,
        #[arg(long, default_value_t = 0)]
        watch_pid: u32,
    },
}
