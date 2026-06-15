use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct MonitorArgs {
    #[command(subcommand)]
    pub action: MonitorAction,
}

#[derive(Debug, Subcommand)]
pub enum MonitorAction {
    /// Read legacy JSONL monitor frames from stdin and render stdout notifications.
    Format {
        /// Legacy peers directory containing <peer>.json records.
        #[arg(long)]
        peers_dir: PathBuf,
        /// Current local display name.
        #[arg(long)]
        my_name: String,
    },

    /// Attach to the Rust event stream without owning transport.
    Attach {
        /// Current local display name. Optional and currently unused by
        /// `attach` (the daemon already owns identity); accepted for
        /// backward compatibility with callers that still pass it. A
        /// fresh agent can simply run `airc monitor attach`.
        #[arg(long)]
        my_name: Option<String>,
        /// **Card 7d5b6a65.** Subscribe from the LIVE EDGE — only
        /// events published strictly after the attach call return.
        ///
        /// Default. The agent-Monitor live-tail shape: no transcript
        /// replay, no per-historical-event notification flood. Pass
        /// `--include-backlog` for the legacy "give me everything"
        /// behaviour.
        #[arg(long, default_value_t = true, overrides_with = "include_backlog")]
        from_now: bool,
        /// **Card 7d5b6a65.** Opt INTO backlog replay. When set, the
        /// daemon replays historical events on attach. With
        /// `--coalesce-backlog` (default ON when this is set), the
        /// catch-up is collapsed to ONE `caught up: skipped N events`
        /// summary line; without it, each historical event is rendered
        /// individually.
        #[arg(long, default_value_t = false, conflicts_with = "from_now")]
        include_backlog: bool,
        /// **Card 7d5b6a65.** When backlog is replayed, collapse the
        /// catch-up phase to ONE summary line instead of N
        /// per-historical-event lines. Default ON whenever backlog is
        /// included — pass `--no-coalesce-backlog` for the legacy
        /// event-by-event replay (audit / replay tooling that needs
        /// every historical envelope on stdout).
        #[arg(long, default_value_t = true)]
        coalesce_backlog: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: MonitorAction,
    }

    // regression: `airc monitor attach` previously REQUIRED --my-name and
    // then ignored it (unused in attach::run), failing fresh users with a
    // clap exit-2. It must now parse with no flags. (audit F7)
    #[test]
    fn attach_parses_without_my_name() {
        let cli = TestCli::try_parse_from(["airc", "attach"])
            .expect("`airc monitor attach` must parse with no flags");
        match cli.action {
            MonitorAction::Attach { my_name, .. } => assert!(my_name.is_none()),
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    // backward compat: callers that still pass --my-name keep working.
    #[test]
    fn attach_still_accepts_my_name() {
        let cli = TestCli::try_parse_from(["airc", "attach", "--my-name", "M5"])
            .expect("`airc monitor attach --my-name X` must still parse");
        match cli.action {
            MonitorAction::Attach { my_name, .. } => assert_eq!(my_name.as_deref(), Some("M5")),
            other => panic!("expected Attach, got {other:?}"),
        }
    }
}
