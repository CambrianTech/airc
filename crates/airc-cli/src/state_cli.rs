use clap::{Args, Subcommand};

/// `airc state` — inspect and edit private scoped state: the peer-private
/// `key → JSON` sibling of the room wall (prefs, "where was I last" /
/// tool-menu cursor, widget UI state). Scope defaults to THIS peer (user
/// scope); `--in-room` scopes to (this peer, the current room).
///
/// Shared room documents — a room's plan, coding instructions, the recipe
/// — belong on the broadcast wall (`airc publish --room …`), not here.
/// Scoped state never broadcasts.
#[derive(Debug, Args)]
pub struct StateArgs {
    #[command(subcommand)]
    pub action: StateAction,
}

#[derive(Debug, Subcommand)]
pub enum StateAction {
    /// Print one scoped-state value. Prints nothing (exit 0) if unset.
    Get {
        /// The state key (e.g. `tool.mode`, `prefs`, `ui.tabs`).
        #[arg(long)]
        key: String,
        /// Scope to (this peer, the current room) instead of this peer alone.
        #[arg(long)]
        in_room: bool,
    },

    /// Write one scoped-state value (last-write-wins). The value is
    /// opaque JSON stored verbatim — the store never parses it.
    Set {
        #[arg(long)]
        key: String,
        /// Opaque JSON value, stored verbatim (e.g. `"code"` or `{"x":1}`).
        #[arg(long)]
        value: String,
        /// LWW version counter the caller owns; the store records it but
        /// never arbitrates.
        #[arg(long, default_value_t = 1)]
        version: i64,
        #[arg(long)]
        in_room: bool,
    },

    /// List every key under the scope (tab-separated `key\tvalue_json`).
    List {
        #[arg(long)]
        in_room: bool,
    },

    /// Delete one scoped-state key. Idempotent — deleting an absent key
    /// is not an error.
    Delete {
        #[arg(long)]
        key: String,
        #[arg(long)]
        in_room: bool,
    },
}
