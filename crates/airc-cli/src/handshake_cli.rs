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
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        my_ssh_pub: String,
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        my_sign_pub: String,
        #[arg(long, default_value = "", allow_hyphen_values = true)]
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    #[test]
    fn send_accepts_multiline_hyphen_leading_public_keys() {
        let pem = "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----";
        let cli = Cli::try_parse_from([
            "airc-rs",
            "handshake",
            "send",
            "127.0.0.1",
            "7549",
            "--my-name",
            "joiner",
            "--my-host",
            "user@host",
            "--my-ssh-pub",
            "ssh-ed25519 AAAA joiner",
            "--my-sign-pub",
            pem,
            "--my-x25519-pub",
            "-----x25519-hyphen-leading",
        ])
        .expect("hyphen-leading public keys should parse as option values");

        let Command::Handshake(args) = cli.command else {
            panic!("expected handshake command");
        };
        let super::HandshakeAction::Send {
            my_sign_pub,
            my_x25519_pub,
            ..
        } = args.action
        else {
            panic!("expected handshake send");
        };
        assert_eq!(my_sign_pub, pem);
        assert_eq!(my_x25519_pub, "-----x25519-hyphen-leading");
    }
}
