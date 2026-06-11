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
    Sync,
}
