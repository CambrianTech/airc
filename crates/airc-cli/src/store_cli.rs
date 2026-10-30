use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct StoreArgs {
    #[command(subcommand)]
    pub action: StoreAction,
}

#[derive(Debug, Subcommand)]
pub enum StoreAction {
    /// Retire a migration this airc does not have from the machine store, then migrate
    /// it forward. For a store a branch or unpublished build migrated (refused "migrated
    /// by a newer airc" while no published airc has that version). Name the exact
    /// version from the refusal; a version this airc has, or one never applied, is refused.
    RetireMigration {
        /// The migration version, as the refusal names it.
        version: String,
    },
}
