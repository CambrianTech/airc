use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct RegistryArgs {
    #[command(subcommand)]
    pub action: RegistryAction,
}

#[derive(Debug, Subcommand)]
pub enum RegistryAction {
    /// Run one account-registry publish + refresh against the gh-gist
    /// rendezvous and print what was published and who was enrolled.
    ///
    /// This is the same operation the daemon runs on a cadence. Use it
    /// to bootstrap a fresh machine (Mac onboarding) or to prove
    /// discovery without waiting for the next daemon tick. Skips
    /// cleanly with a notice if `gh` isn't authenticated.
    ///
    /// Publishes the running daemon's dialable endpoints (read back
    /// over IPC). If no daemon is running — or it advertises no
    /// endpoints — the sync REFUSES rather than overwrite this
    /// machine's registry gist with an endpoint-less beacon (card
    /// 4b6a0ffa / #33).
    Sync {
        /// Publish a key-only (endpoint-less) beacon even when no
        /// daemon endpoint can be read back. Same-account peers can
        /// then enrol this machine's key but CANNOT dial it until a
        /// daemon publishes real endpoints.
        #[arg(long)]
        allow_endpointless: bool,
    },
}
